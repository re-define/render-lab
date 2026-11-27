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
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

mod movement;
pub use movement::{MoveIntent, MovementControls, MovementDriver};
mod schedule;
pub use schedule::{
    BindingCheck, BoundSystems, Edit, FixedDomain, Schedule, SimulationData, SimulationSystem,
    System, SystemAccess, SystemContext, SystemOrder, TransformBindings, TransformTargets,
};

pub type FrameResult = Result<Frame, String>;
/// Owns the single waiting snapshot slot until the render thread dequeues it.
struct Admission(Arc<AtomicBool>);
impl Admission {
    fn reserve(slot: &Arc<AtomicBool>) -> Option<Self> {
        slot.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(slot.clone()))
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
/// Optional readback camera evaluated at the same simulation tick as the main view.
#[derive(Clone, Copy)]
pub struct OffscreenView {
    pub camera: Camera,
    pub extent: [u32; 2],
    pub post: post::Settings,
}
struct Input {
    secondary: Option<OffscreenView>,
    admission: Admission,
    stamp: crate::render::FrameStamp,
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
    results: std::collections::VecDeque<FrameResult>,
    ordered: bool,
    memory: Option<crate::rhi::ManagedMemory>,
}
type Latest = Arc<Mutex<LastFrame>>;
fn publish(mailbox: &Latest, frame: FrameResult) {
    let mut last = mailbox.lock().unwrap();
    if let Ok(frame) = &frame {
        last.memory = Some(frame.memory.managed.clone());
    }
    if !last.ordered {
        last.results.clear();
    }
    last.results.push_back(frame);
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
    pub movement: MovementControls,
    pub technique: Arc<dyn Technique>,
    pub tasks: Arc<ThreadPool>,
    input: Option<SyncSender<Input>>,
    admission: Arc<AtomicBool>,
    origin: Instant,
    tick_id: u64,
    snapshot_id: u64,
    skipped: u64,
    latest: Latest,
    secondary_results: Latest,
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
        let (out, plans) = mpsc::sync_channel::<(FramePlan, Option<isize>, bool)>(1);
        let latest = Latest::default();
        let secondary_results = Latest::default();
        let secondary_mailbox = secondary_results.clone();
        let generation = Arc::new(AtomicU64::new(0));
        let rhi_generation = generation.clone();
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
                let report = |result: Result<Frame>, secondary: bool| {
                    if result.is_err() {
                        rhi_generation.fetch_add(1, Ordering::AcqRel);
                    }
                    publish(
                        if secondary {
                            &secondary_mailbox
                        } else {
                            &mailbox
                        },
                        result.map_err(|e| format!("{e:#}")),
                    );
                };
                loop {
                    let incoming = if backend.has_pending() {
                        plans.recv_timeout(Duration::from_millis(1))
                    } else {
                        plans.recv().map_err(|_| RecvTimeoutError::Disconnected)
                    };
                    match incoming {
                        Ok((plan, window, secondary)) => {
                            if window.is_none() {
                                while let Some(frame) = backend.retire_next() {
                                    report(frame, false);
                                }
                            }
                            backend.select_view(secondary, plan.stamp.has_secondary);
                            let id = plan.stamp.snapshot_id;
                            if plan.stamp.generation != rhi_generation.load(Ordering::Acquire) {
                                publish(
                                    if secondary {
                                        &secondary_mailbox
                                    } else {
                                        &mailbox
                                    },
                                    Err(format!(
                                        "skipped snapshot {id} after RHI history invalidation"
                                    )),
                                );
                                continue;
                            }
                            let result = match window {
                                Some(window) => backend.enqueue(plan, &pool, window),
                                None => backend.execute(plan, &pool).map(Some),
                            };
                            match result {
                                Ok(Some(frame)) => report(Ok(frame), secondary),
                                Ok(None) => (),
                                Err(error) => {
                                    report(Err(error.context(format!("snapshot {id}"))), secondary)
                                }
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            if let Some(frame) = backend.retire_ready() {
                                report(frame, false);
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                while let Some(frame) = backend.retire_next() {
                    report(frame, false);
                }
            })?;
        let (dlss_unavailable, buffer_atomic64, ray_caps) =
            startup.recv()?.map_err(|e| anyhow!(e))?;
        let pool = tasks.clone();
        let mailbox = latest.clone();
        let secondary_mailbox = secondary_results.clone();
        let render = thread::Builder::new()
            .name("lab-render".into())
            .spawn(move || {
                let mut sequences = [post::Sequence::default(), post::Sequence::default()];
                let mut previous_generation = 0;
                for mut frame in scenes {
                    drop(frame.admission);
                    let epoch = generation.load(Ordering::Acquire);
                    if epoch != previous_generation {
                        for sequence in &mut sequences {
                            sequence.discard();
                        }
                        previous_generation = epoch;
                    }
                    frame.stamp.generation = epoch;
                    frame.stamp.render_dequeued = Some(Instant::now());
                    frame.snapshot.buffer_atomic64 = buffer_atomic64;
                    frame.snapshot.ray_caps = ray_caps;
                    let secondary = frame.secondary.map(|view| {
                        (
                            frame.snapshot.for_camera(view.camera, view.extent),
                            view.post,
                        )
                    });
                    if secondary.is_none() {
                        sequences[1].discard();
                    }
                    let views = std::iter::once((frame.snapshot, frame.post)).chain(secondary);
                    for (index, (mut snapshot, settings)) in views.enumerate() {
                        let secondary = index == 1;
                        let window = frame.window.filter(|_| !secondary);
                        let temporal = &mut sequences[index];
                        temporal.assign(settings, &frame.technique, &mut snapshot);
                        let result = pool.install(|| {
                            let started = Instant::now();
                            let mut graph = frame.technique.build(&snapshot)?;
                            if window.is_some() {
                                let output = graph.present()?;
                                if frame.readback {
                                    graph.pass(
                                        "Capture recording",
                                        crate::rdg::Op::HostRead(output),
                                    );
                                }
                            }
                            let build_ms = started.elapsed().as_secs_f64() * 1000.;
                            let mut plan = graph.compile()?;
                            plan.stamp = frame.stamp;
                            plan.stamp.plan_ready = Some(Instant::now());
                            plan.build_ms = build_ms;
                            plan.main_ms = frame.main_ms;
                            plan.extract_ms = if secondary { 0. } else { frame.extract_ms };
                            plan.extraction = snapshot.scene.stats;
                            Ok::<_, anyhow::Error>(plan)
                        });
                        match result {
                            Ok(plan) => {
                                if out.send((plan, window, secondary)).is_err() {
                                    return;
                                }
                            }
                            Err(error) => {
                                temporal.discard();
                                publish(
                                    if secondary {
                                        &secondary_mailbox
                                    } else {
                                        &mailbox
                                    },
                                    Err(format!("{error:#}")),
                                );
                            }
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
            movement: Default::default(),
            technique,
            tasks,
            input: Some(input),
            admission: Default::default(),
            origin: Instant::now(),
            tick_id: 0,
            snapshot_id: 0,
            skipped: 0,
            latest,
            secondary_results,
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
        self.tick_with_view(dt, extent, None)
    }
    pub fn tick_with_view(
        &mut self,
        dt: f32,
        extent: [u32; 2],
        secondary: Option<OffscreenView>,
    ) -> Result<bool> {
        if let Some(view) = secondary {
            anyhow::ensure!(
                view.extent.iter().all(|&x| x > 0),
                "invalid secondary view extent"
            );
            anyhow::ensure!(
                !view.post.mode().uses_ngx(),
                "the secondary view supports off and TAA; NGX uses the primary viewport feature"
            );
        }
        let started = std::time::Instant::now();
        self.schedule.tick(&mut self.world, dt, &self.tasks)?;
        self.tick_id += 1;
        let full = |mailbox: &Latest| {
            let mailbox = mailbox.lock().unwrap();
            mailbox.ordered && mailbox.results.len() >= 2
        };
        let pending_reports =
            full(&self.latest) || (secondary.is_some() && full(&self.secondary_results));
        let Some(admission) = (!pending_reports)
            .then(|| Admission::reserve(&self.admission))
            .flatten()
        else {
            self.skipped += 1;
            return Ok(false);
        };
        let extraction_started = Instant::now();
        let snapshot =
            self.extraction
                .snapshot(&self.world, self.camera, extent, self.materials.clone())?;
        self.snapshot_id += 1;
        let frame = Input {
            secondary,
            admission,
            stamp: crate::render::FrameStamp {
                origin: Some(self.origin),
                tick_id: self.tick_id,
                snapshot_id: self.snapshot_id,
                skipped_ticks: self.skipped,
                has_secondary: secondary.is_some(),
                tick_started: Some(started),
                snapshot_ready: Some(Instant::now()),
                ..Default::default()
            },
            readback: self.capture_viewport,
            main_ms: (extraction_started - started).as_secs_f64() * 1000.,
            extract_ms: extraction_started.elapsed().as_secs_f64() * 1000.,
            window: self.window,
            snapshot,
            technique: self.technique.clone(),
            post: self.post,
        };
        self.input
            .as_ref()
            .context("engine stopped")?
            .send(frame)
            .map_err(|_| anyhow!("render thread exited"))?;
        Ok(true)
    }
    /// # Safety
    /// The window owner must keep this Win32 child alive until the engine has stopped.
    pub unsafe fn set_viewport_window(&mut self, hwnd: isize) {
        self.window = Some(hwnd);
    }
    /// Enable before driving a benchmark. Render admission applies backpressure when the
    /// ordered reader falls behind; simulation continues advancing on each tick.
    pub fn set_ordered_completions(&mut self, ordered: bool) {
        for mailbox in [&self.latest, &self.secondary_results] {
            let mut mailbox = mailbox.lock().unwrap();
            mailbox.ordered = ordered;
            if !ordered {
                let latest = mailbox.results.pop_back();
                mailbox.results.clear();
                mailbox.results.extend(latest);
            }
        }
    }
    pub fn take_frame(&self) -> Option<FrameResult> {
        self.latest.lock().unwrap().results.pop_front()
    }
    pub fn take_secondary_frame(&self) -> Option<FrameResult> {
        self.secondary_results.lock().unwrap().results.pop_front()
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

#[cfg(test)]
mod pipeline_tests {
    use super::*;
    use crate::render::{Forward, FrameGraph};

    #[test]
    fn waiting_snapshot_slot_is_returned_on_failure_or_dequeue() {
        let slot = Default::default();
        let held = Admission::reserve(&slot).unwrap();
        assert!(Admission::reserve(&slot).is_none());
        drop(held);
        assert!(Admission::reserve(&slot).is_some());
    }

    #[test]
    #[ignore = "requires Vulkan descriptor heap"]
    fn failed_rhi_snapshot_invalidates_pose_history_and_preserves_frame_ids() -> Result<()> {
        struct Probe {
            resets: Arc<Mutex<Vec<bool>>>,
        }
        impl Technique for Probe {
            fn needs_surface_motion(&self, _: &Snapshot) -> bool {
                true
            }
            fn build(&self, snapshot: &Snapshot) -> Result<FrameGraph> {
                let mut resets = self.resets.lock().unwrap();
                resets.push(snapshot.temporal.as_ref().unwrap().previous_scene.is_none());
                if resets.len() == 2 {
                    // Compiles as an ordinary buffer, rejected by the device extent check before allocation.
                    let mut g = FrameGraph::new([1_000_000, 1]);
                    let output = g.input("oversize extent", vec![0; 4_000_000]);
                    g.set_output(output);
                    return Ok(g);
                }
                Forward::default().build(snapshot)
            }
        }
        let resets = Arc::new(Mutex::new(Vec::new()));
        let mut engine = Engine::new(
            World::demo(),
            Arc::new(Probe {
                resets: resets.clone(),
            }),
        )?;
        engine.set_ordered_completions(true);
        for id in 1..=4 {
            assert!(engine.tick(1. / 60., [96, 64])?);
            let start = Instant::now();
            let result = loop {
                if let Some(result) = engine.take_frame() {
                    break result;
                }
                anyhow::ensure!(start.elapsed() < Duration::from_secs(15), "frame timeout");
                thread::sleep(Duration::from_millis(1));
            };
            if id == 2 {
                assert!(
                    result
                        .err()
                        .unwrap()
                        .contains("viewport exceeds device limits")
                );
                continue;
            }
            let frame = result.map_err(anyhow::Error::msg)?;
            let p = &frame.profile;
            assert_eq!((p.tick_id, p.snapshot_id), (id, id));
            assert_eq!(p.history_generation, u64::from(id > 2));
            let t = p.cpu_times_us.as_ref().unwrap();
            let marks = [
                t.tick, t.snapshot, t.render, t.plan, t.rhi, t.acquire, t.submit, t.retired,
            ];
            assert!(marks.windows(2).all(|w| w[0] <= w[1]));
            assert!(t.present_return.is_none());
        }
        assert_eq!(*resets.lock().unwrap(), [true, false, true, false]);
        Ok(())
    }
}
