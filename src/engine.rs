use crate::{
    rdg::Plan,
    render::{Camera, Snapshot, Technique},
    rhi::{Backend, Frame},
    scene::World,
};
use anyhow::{Context, Result, anyhow};
use rayon::{ThreadPool, ThreadPoolBuilder, prelude::*};
use std::{
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
};

pub type Edit = Box<dyn FnOnce(&mut World) + Send>;
pub trait System: Send + Sync {
    /// Systems in a stage see the same snapshot. Edits commit in registration order.
    fn update(&self, world: &World, dt: f32) -> Result<Edit>;
}
#[derive(Default)]
pub struct Schedule {
    stages: Vec<Vec<Arc<dyn System>>>,
}
impl Schedule {
    pub fn stage(&mut self, systems: Vec<Arc<dyn System>>) {
        self.stages.push(systems);
    }
    pub fn tick(&self, world: &mut Arc<World>, dt: f32, pool: &ThreadPool) -> Result<()> {
        for systems in &self.stages {
            let edits = pool.install(|| {
                systems
                    .par_iter()
                    .map(|s| s.update(world, dt))
                    .collect::<Result<Vec<_>>>()
            })?;
            for edit in edits {
                edit(Arc::make_mut(world));
            }
        }
        Ok(())
    }
}
pub type FrameResult = Result<Frame, String>;
struct Input {
    main_ms: f64,
    readback: bool,
    window: Option<isize>,
    snapshot: Snapshot,
    technique: Arc<dyn Technique>,
}
type Latest = Arc<Mutex<Option<FrameResult>>>;
fn publish(mailbox: &Latest, frame: FrameResult) {
    *mailbox.lock().unwrap() = Some(frame);
}
pub struct Engine {
    pub capture_viewport: bool,
    pub materials: Arc<crate::render::MaterialPrograms>,
    pub(crate) window: Option<isize>,
    pub world: Arc<World>,
    pub camera: Camera,
    pub schedule: Schedule,
    pub technique: Arc<dyn Technique>,
    pub tasks: Arc<ThreadPool>,
    input: Option<SyncSender<Input>>,
    latest: Latest,
    threads: Vec<JoinHandle<()>>,
}
impl Engine {
    pub fn new(world: World, technique: Arc<dyn Technique>) -> Result<Self> {
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
        let (out, plans) = mpsc::sync_channel::<(Plan, Option<isize>)>(1);
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
                if ready.send(Ok(())).is_err() {
                    return;
                }
                for (plan, window) in plans {
                    publish(
                        &mailbox,
                        backend
                            .execute_to(plan, &pool, window)
                            .map_err(|e| format!("{e:#}")),
                    );
                }
            })?;
        startup.recv()?.map_err(|e| anyhow!(e))?;
        let pool = tasks.clone();
        let mailbox = latest.clone();
        let render = thread::Builder::new()
            .name("lab-render".into())
            .spawn(move || {
                for frame in scenes {
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
                            Ok(p)
                        })
                    }) {
                        Ok(p) => {
                            if out.send((p, frame.window)).is_err() {
                                break;
                            }
                        }
                        Err(e) => publish(&mailbox, Err(format!("{e:#}"))),
                    }
                }
            })?;
        let camera = Camera::fit(&world)?;
        Ok(Self {
            capture_viewport: false,
            window: None,
            materials: Default::default(),
            world: Arc::new(world),
            camera,
            schedule: Schedule::default(),
            technique,
            tasks,
            input: Some(input),
            latest,
            threads: vec![render, rhi],
        })
    }
    pub fn tick(&mut self, dt: f32, extent: [u32; 2]) -> Result<bool> {
        let started = std::time::Instant::now();
        self.schedule.tick(&mut self.world, dt, &self.tasks)?;
        let frame = Input {
            readback: self.capture_viewport,
            main_ms: started.elapsed().as_secs_f64() * 1000.,
            window: self.window,
            snapshot: Snapshot {
                materials: self.materials.clone(),
                world: self.world.clone(),
                camera: self.camera,
                extent,
            },
            technique: self.technique.clone(),
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
        self.latest.lock().unwrap().take()
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
