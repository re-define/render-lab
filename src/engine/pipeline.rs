//! Render/RHI hand-off and completion queues for the primary and offscreen views.
use super::{FrameResult, Input};
use crate::{
    render::{FramePlan, post},
    rhi::{Backend, Frame, ManagedMemory, RayCapabilities},
};
use anyhow::{Result, anyhow};
use rayon::ThreadPool;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Default)]
struct LastFrame {
    results: std::collections::VecDeque<FrameResult>,
    ordered: bool,
    memory: Option<ManagedMemory>,
}

#[derive(Default)]
pub(super) struct Completions {
    views: [Mutex<LastFrame>; 2],
}

impl Completions {
    fn publish(&self, secondary: bool, frame: FrameResult) {
        let mut last = self.views[usize::from(secondary)].lock().unwrap();
        if !secondary && let Ok(frame) = &frame {
            last.memory = Some(frame.memory.managed.clone());
        }
        if !last.ordered {
            last.results.clear();
        }
        last.results.push_back(frame);
    }
    pub(super) fn full(&self, secondary: bool) -> bool {
        let view = self.views[usize::from(secondary)].lock().unwrap();
        view.ordered && view.results.len() >= 2
    }
    pub(super) fn take(&self, secondary: bool) -> Option<FrameResult> {
        self.views[usize::from(secondary)]
            .lock()
            .unwrap()
            .results
            .pop_front()
    }
    pub(super) fn primary_memory(&self) -> Option<ManagedMemory> {
        self.views[0].lock().unwrap().memory.clone()
    }
    pub(super) fn set_ordered(&self, ordered: bool) {
        for view in &self.views {
            let mut view = view.lock().unwrap();
            view.ordered = ordered;
            if !ordered && let Some(latest) = view.results.pop_back() {
                view.results.clear();
                view.results.push_back(latest);
            }
        }
    }
}

struct Submission {
    plan: FramePlan,
    window: Option<isize>,
    secondary: bool,
}

pub(super) struct DeviceInfo {
    pub(super) dlss_unavailable: Option<String>,
    pub(super) ray_caps: RayCapabilities,
    buffer_atomic64: bool,
}

struct Shared {
    pool: Arc<ThreadPool>,
    completions: Completions,
    generation: AtomicU64,
}

pub(super) struct Pipeline {
    shared: Arc<Shared>,
    input: Option<SyncSender<Input>>,
    threads: Vec<JoinHandle<()>>,
}

impl Pipeline {
    pub(super) fn start(pool: &Arc<ThreadPool>) -> Result<(Self, DeviceInfo)> {
        let (input, scenes) = mpsc::sync_channel(1);
        let (out, plans) = mpsc::sync_channel(1);
        let (ready, startup) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared {
            pool: pool.clone(),
            completions: Completions::default(),
            generation: AtomicU64::new(0),
        });
        let rhi = {
            let shared = shared.clone();
            thread::Builder::new()
                .name("lab-rhi".into())
                .spawn(move || run_rhi(plans, ready, &shared))?
        };
        let mut pipeline = Self {
            shared,
            input: Some(input),
            threads: vec![rhi],
        };
        let device = startup.recv()?.map_err(anyhow::Error::msg)?;
        let shared = pipeline.shared.clone();
        let atomic64 = device.buffer_atomic64;
        let rays = device.ray_caps;
        let render = thread::Builder::new()
            .name("lab-render".into())
            .spawn(move || run_render(scenes, out, &shared, atomic64, rays))?;
        pipeline.threads.push(render);
        Ok((pipeline, device))
    }
    pub(super) fn completions(&self) -> &Completions {
        &self.shared.completions
    }
    pub(super) fn submit(&self, frame: Input) -> Result<()> {
        self.input
            .as_ref()
            .expect("pipeline input exists until drop")
            .send(frame)
            .map_err(|_| anyhow!("render thread exited"))
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.input.take();
        // The render thread closes the plan channel; RHI drains its in-flight frames.
        for worker in self.threads.drain(..).rev() {
            let _ = worker.join();
        }
    }
}

fn run_rhi(
    plans: Receiver<Submission>,
    ready: SyncSender<Result<DeviceInfo, String>>,
    shared: &Shared,
) {
    let mut backend = match Backend::open() {
        Ok(b) => b,
        Err(e) => {
            let _ = ready.send(Err(format!("{e:#}")));
            return;
        }
    };
    if ready
        .send(Ok(DeviceInfo {
            dlss_unavailable: backend.dlss_unavailable(),
            buffer_atomic64: backend.buffer_atomic64(),
            ray_caps: backend.ray_capabilities(),
        }))
        .is_err()
    {
        return;
    }
    let report = |result: Result<Frame>, secondary: bool| {
        if result.is_err() {
            shared.generation.fetch_add(1, Ordering::AcqRel);
        }
        shared
            .completions
            .publish(secondary, result.map_err(|e| format!("{e:#}")));
    };
    loop {
        let incoming = if backend.has_pending() {
            plans.recv_timeout(Duration::from_millis(1))
        } else {
            plans.recv().map_err(|_| RecvTimeoutError::Disconnected)
        };
        match incoming {
            Ok(Submission {
                plan,
                window,
                secondary,
            }) => {
                if window.is_none() {
                    while let Some(frame) = backend.retire_next() {
                        report(frame, false);
                    }
                }
                backend.select_view(secondary, plan.stamp.has_secondary);
                let id = plan.stamp.snapshot_id;
                if plan.stamp.generation != shared.generation.load(Ordering::Acquire) {
                    shared.completions.publish(
                        secondary,
                        Err(format!(
                            "skipped snapshot {id} after RHI history invalidation"
                        )),
                    );
                    continue;
                }
                let result = match window {
                    Some(window) => backend.enqueue(plan, &shared.pool, window),
                    None => backend.execute(plan, &shared.pool).map(Some),
                };
                match result {
                    Ok(Some(frame)) => report(Ok(frame), secondary),
                    Ok(None) => (),
                    Err(error) => report(Err(error.context(format!("snapshot {id}"))), secondary),
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
}

fn run_render(
    scenes: Receiver<Input>,
    out: SyncSender<Submission>,
    shared: &Shared,
    buffer_atomic64: bool,
    ray_caps: RayCapabilities,
) {
    let mut sequences = [post::Sequence::default(), post::Sequence::default()];
    let mut compilers = [
        crate::rdg::Compiler::for_view(),
        crate::rdg::Compiler::for_view(),
    ];
    let mut previous_generation = 0;
    for mut frame in scenes {
        drop(frame.admission);
        let epoch = shared.generation.load(Ordering::Acquire);
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
        for ((mut snapshot, settings), temporal) in views.zip(&mut sequences) {
            let secondary = snapshot.secondary_view;
            let window = frame.window.filter(|_| !secondary);
            temporal.assign(settings, &frame.technique, &mut snapshot);
            let result = shared.pool.install(|| {
                let started = Instant::now();
                let mut graph = frame.technique.build(&snapshot)?;
                if window.is_some() {
                    let output = graph.present()?;
                    if frame.readback {
                        graph.pass("Capture recording", crate::rdg::Op::HostRead(output));
                    }
                }
                let mut plan = graph.compile_with(Some(&mut compilers[usize::from(secondary)]))?;
                plan.stamp = frame.stamp;
                plan.stamp.plan_ready = Some(Instant::now());
                // Include finalizing skin batches, output validation and view products in build time.
                plan.build_ms =
                    (started.elapsed().as_secs_f64() * 1000. - plan.commands.compile_ms).max(0.);
                plan.main_ms = frame.main_ms;
                plan.extract_ms = if secondary { 0. } else { frame.extract_ms };
                plan.extraction = snapshot.scene.stats;
                Ok::<_, anyhow::Error>(plan)
            });
            match result {
                Ok(plan) => {
                    if out
                        .send(Submission {
                            plan,
                            window,
                            secondary,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                Err(error) => {
                    temporal.discard();
                    shared
                        .completions
                        .publish(secondary, Err(format!("{error:#}")));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_completion_mode_keeps_the_latest_result_for_each_view() {
        let queues = Completions::default();
        queues.set_ordered(true);
        for (secondary, message) in [
            (true, "secondary 1"),
            (false, "primary 1"),
            (true, "secondary 2"),
        ] {
            queues.publish(secondary, Err(message.to_owned()));
        }
        assert!(!queues.full(false));
        assert!(queues.full(true));
        queues.set_ordered(false);
        assert!(!queues.full(true));
        assert_eq!(
            queues.take(true).and_then(Result::err).as_deref(),
            Some("secondary 2")
        );
        assert_eq!(
            queues.take(false).and_then(Result::err).as_deref(),
            Some("primary 1")
        );
        assert!(queues.take(true).is_none());
    }
}
