use crate::{
    render::{Camera, SceneExtractor, Snapshot, Technique, post},
    rhi::Frame,
    scene::World,
};
use anyhow::Result;
use rayon::{ThreadPool, ThreadPoolBuilder};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Instant,
};

mod pipeline;
pub use crate::movement::{MoveIntent, MovementControls, MovementDriver};
use pipeline::Pipeline;
mod schedule;
pub use schedule::{
    BindingCheck, BoundSystems, Edit, FixedDomain, Schedule, SimulationData, SimulationSystem,
    System, SystemAccess, SystemContext, SystemOrder, TickProfile, TickRate, TransformBindings,
    TransformTargets,
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
pub struct Engine {
    // Stop the workers before releasing scene, technique and device-facing state.
    pipeline: Pipeline,
    pub capture_viewport: bool,
    pub materials: Arc<crate::render::MaterialPrograms>,
    pub(crate) window: Option<isize>,
    pub world: Arc<World>,
    pub camera: Camera,
    pub post: post::Settings,
    dlss_unavailable: Option<String>,
    pub ray_caps: crate::rhi::RayCapabilities,
    pub audio: crate::audio::Audio,
    pub schedule: Schedule,
    pub movement: MovementControls,
    pub technique: Arc<dyn Technique>,
    pub tasks: Arc<ThreadPool>,
    admission: Arc<AtomicBool>,
    origin: Instant,
    tick_id: u64,
    snapshot_id: u64,
    skipped: u64,
    pub(crate) extraction: SceneExtractor,
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
        let (pipeline, device) = Pipeline::start(&tasks)?;
        Ok(Self {
            capture_viewport: false,
            window: None,
            materials: Default::default(),
            world: Arc::new(world),
            camera: Camera::default(),
            post: Default::default(),
            dlss_unavailable: device.dlss_unavailable,
            ray_caps: device.ray_caps,
            audio: Default::default(),
            schedule: Schedule::default(),
            movement: Default::default(),
            technique,
            tasks,
            pipeline,
            admission: Default::default(),
            origin: Instant::now(),
            tick_id: 0,
            snapshot_id: 0,
            skipped: 0,
            extraction: SceneExtractor::default(),
        })
    }
    /// Save committed component state and the clock phase needed to resume at the next substep.
    pub fn save_scene(&self, path: &std::path::Path) -> Result<()> {
        self.world
            .save_with_clocks(path, &self.schedule.checkpoints(&self.world))
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
        self.audio.update();
        self.schedule.tick(&mut self.world, dt, &self.tasks)?;
        self.tick_id += 1;
        let pending_reports = self.pipeline.completions().full(false)
            || (secondary.is_some() && self.pipeline.completions().full(true));
        let Some(admission) = (!pending_reports)
            .then(|| Admission::reserve(&self.admission))
            .flatten()
        else {
            self.skipped += 1;
            return Ok(false);
        };
        let extraction_started = Instant::now();
        if self.extraction.visual_pose_budget().is_some() {
            self.extraction.focused_actor = self.movement.target(&self.world);
        }
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
        self.pipeline.submit(frame)?;
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
        self.pipeline.completions().set_ordered(ordered);
    }
    pub fn take_frame(&self) -> Option<FrameResult> {
        self.pipeline.completions().take(false)
    }
    pub fn take_secondary_frame(&self) -> Option<FrameResult> {
        self.pipeline.completions().take(true)
    }
    pub(crate) fn last_counter(&self, key: &str) -> Option<u64> {
        self.pipeline.completions().counter(key)
    }
    pub fn last_memory(&self) -> Option<crate::rhi::ManagedMemory> {
        self.pipeline.completions().primary_memory()
    }
}
#[cfg(test)]
mod pipeline_tests {
    use super::*;
    use crate::render::{Forward, FrameGraph};
    use std::{sync::Mutex, time::Duration};

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
