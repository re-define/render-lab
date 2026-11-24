use crate::{
    render::FramePlan,
    render::{Camera, SceneExtractor, Snapshot, Technique, post},
    rhi::{Backend, Frame},
    scene::World,
};
use anyhow::{Context, Result, anyhow};
use rayon::{ThreadPool, ThreadPoolBuilder};
use std::{
    sync::{
        Arc, Mutex,
        mpsc::{self, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

mod schedule;
pub use schedule::{Edit, Schedule, SimulationData, System, SystemAccess};

pub type FrameResult = Result<Frame, String>;
struct Input {
    main_ms: f64,
    extract_ms: f64,
    readback: bool,
    window: Option<isize>,
    snapshot: Snapshot,
    technique: Arc<dyn Technique>,
    post: post::Settings,
}
#[derive(Default)]
struct LastFrame {
    result: Option<FrameResult>,
    memory: Option<crate::rhi::ManagedMemory>,
}
type Latest = Arc<Mutex<LastFrame>>;
fn publish(mailbox: &Latest, frame: FrameResult) {
    let mut last = mailbox.lock().unwrap();
    if let Ok(frame) = &frame {
        last.memory = Some(frame.memory.managed.clone());
    }
    last.result = Some(frame);
}
pub struct Engine {
    pub capture_viewport: bool,
    pub materials: Arc<crate::render::MaterialPrograms>,
    pub(crate) window: Option<isize>,
    pub world: Arc<World>,
    pub camera: Camera,
    pub post: post::Settings,
    dlss_unavailable: Option<String>,
    pub ray_caps: crate::rhi::RayCapabilities,
    pub schedule: Schedule,
    pub technique: Arc<dyn Technique>,
    pub tasks: Arc<ThreadPool>,
    input: Option<SyncSender<Input>>,
    latest: Latest,
    threads: Vec<JoinHandle<()>>,
    extraction: SceneExtractor,
}
impl Engine {
    pub fn new(world: World, technique: Arc<dyn Technique>) -> Result<Self> {
        let mut engine = Self::start_workers(world, technique)?;
        engine.camera = Camera::fit(&engine.world)?;
        Ok(engine)
    }

    /// Start the pipeline before the caller applies its scene-specific camera and systems.
    pub(crate) fn start_workers(world: World, technique: Arc<dyn Technique>) -> Result<Self> {
        world.validate()?;
        let workers = match std::env::var("RENDER_LAB_WORKERS") {
            Ok(s) => {
                let n = s.parse::<usize>()?;
                anyhow::ensure!(n > 0, "worker count must be positive");
                n
            }
            Err(_) => thread::available_parallelism()
                .map_or(2, usize::from)
                .clamp(2, 16),
        };
        let tasks = Arc::new(
            ThreadPoolBuilder::new()
                .num_threads(workers)
                .thread_name(|i| format!("lab-task-{i}"))
                .build()?,
        );
        let (input, scenes) = mpsc::sync_channel::<Input>(1);
        let (out, plans) = mpsc::sync_channel::<(FramePlan, Option<isize>)>(1);
        let latest = Latest::default();
        let pool = tasks.clone();
        let mailbox = latest.clone();
        let (ready, startup) = mpsc::sync_channel(1);
        let rhi = thread::Builder::new()
            .name("lab-rhi".into())
            .spawn(move || {
                let mut backend = match Backend::open() {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = ready.send(Err(format!("{e:#}")));
                        return;
                    }
                };
                if ready
                    .send(Ok((
                        backend.dlss_unavailable(),
                        backend.buffer_atomic64(),
                        backend.ray_capabilities(),
                    )))
                    .is_err()
                {
                    return;
                }
                let report =
                    |result: Result<Frame>| publish(&mailbox, result.map_err(|e| format!("{e:#}")));
                loop {
                    let incoming = if backend.has_pending() {
                        plans.recv_timeout(Duration::from_millis(1))
                    } else {
                        plans.recv().map_err(|_| RecvTimeoutError::Disconnected)
                    };
                    match incoming {
                        Ok((plan, Some(window))) => match backend.enqueue(plan, &pool, window) {
                            Ok(Some(frame)) => report(Ok(frame)),
                            Ok(None) => {}
                            Err(error) => report(Err(error)),
                        },
                        Ok((plan, None)) => {
                            while let Some(frame) = backend.retire_next() {
                                report(frame);
                            }
                            report(backend.execute(plan, &pool));
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            if let Some(frame) = backend.retire_next() {
                                report(frame);
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                while let Some(frame) = backend.retire_next() {
                    report(frame);
                }
            })?;
        let (dlss_unavailable, buffer_atomic64, ray_caps) =
            startup.recv()?.map_err(|e| anyhow!(e))?;
        let pool = tasks.clone();
        let mailbox = latest.clone();
        let render = thread::Builder::new()
            .name("lab-render".into())
            .spawn(move || {
                let mut temporal = post::Sequence::default();
                for mut frame in scenes {
                    frame.snapshot.buffer_atomic64 = buffer_atomic64;
                    frame.snapshot.ray_caps = ray_caps;
                    temporal.assign(frame.post, &frame.technique, &mut frame.snapshot);
                    match pool.install(|| {
                        let started = std::time::Instant::now();
                        frame.technique.build(&frame.snapshot).and_then(|mut g| {
                            if frame.window.is_some() {
                                let output = g.present()?;
                                if frame.readback {
                                    g.pass("Capture recording", crate::rdg::Op::HostRead(output));
                                }
                            }
                            let build_ms = started.elapsed().as_secs_f64() * 1000.;
                            let mut p = g.compile()?;
                            p.build_ms = build_ms;
                            p.main_ms = frame.main_ms;
                            p.extract_ms = frame.extract_ms;
                            p.extraction = frame.snapshot.scene.stats;
                            Ok(p)
                        })
                    }) {
                        Ok(p) => {
                            if out.send((p, frame.window)).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            temporal.discard();
                            publish(&mailbox, Err(format!("{e:#}")));
                        }
                    }
                }
            })?;
        Ok(Self {
            capture_viewport: false,
            window: None,
            materials: Default::default(),
            world: Arc::new(world),
            camera: Camera::default(),
            post: Default::default(),
            dlss_unavailable,
            ray_caps,
            schedule: Schedule::default(),
            technique,
            tasks,
            input: Some(input),
            latest,
            threads: vec![render, rhi],
            extraction: SceneExtractor::default(),
        })
    }
    pub fn dlss_unavailable(&self) -> Option<&str> {
        self.dlss_unavailable.as_deref()
    }

    pub fn select_post(&mut self, mode: post::Mode) -> Result<()> {
        if mode.uses_ngx()
            && let Some(reason) = self.dlss_unavailable()
        {
            anyhow::bail!("DLSS unavailable: {reason}");
        }
        self.post.select(mode);
        Ok(())
    }
    pub fn tick(&mut self, dt: f32, extent: [u32; 2]) -> Result<bool> {
        let started = std::time::Instant::now();
        self.schedule.tick(&mut self.world, dt, &self.tasks)?;
        let extraction_started = std::time::Instant::now();
        let snapshot =
            self.extraction
                .snapshot(&self.world, self.camera, extent, self.materials.clone())?;
        let frame = Input {
            readback: self.capture_viewport,
            main_ms: (extraction_started - started).as_secs_f64() * 1000.,
            extract_ms: extraction_started.elapsed().as_secs_f64() * 1000.,
            window: self.window,
            snapshot,
            technique: self.technique.clone(),
            post: self.post,
        };
        match self
            .input
            .as_ref()
            .context("engine stopped")?
            .try_send(frame)
        {
            Ok(()) => Ok(true),
            Err(TrySendError::Full(_)) => Ok(false),
            Err(TrySendError::Disconnected(_)) => Err(anyhow!("render thread exited")),
        }
    }
    /// # Safety
    /// The window owner must keep this Win32 child alive until the engine has stopped.
    pub unsafe fn set_viewport_window(&mut self, hwnd: isize) {
        self.window = Some(hwnd);
    }
    pub fn take_frame(&self) -> Option<FrameResult> {
        self.latest.lock().unwrap().result.take()
    }
    pub fn last_memory(&self) -> Option<crate::rhi::ManagedMemory> {
        self.latest.lock().unwrap().memory.clone()
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        self.input.take();
        for h in self.threads.drain(..) {
            let _ = h.join();
        }
    }
}
