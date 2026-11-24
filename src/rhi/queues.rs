use super::*;
use vk::PipelineStageFlags2 as Stage;

/// A queue run normally shares one signal. Release the next queue at the last
/// producer it actually consumes; only the independent suffix gets its own batch.
fn submission_ranges(
    steps: &[Step],
    queue: impl Fn(&Step) -> usize,
    has_device_work: impl Fn(&Step) -> bool,
    release_ready: bool,
) -> Vec<std::ops::Range<usize>> {
    let mut ranks = vec![0; steps.len()];
    let mut runs: Vec<std::ops::Range<usize>> = Vec::new();
    for (rank, step) in steps.iter().enumerate() {
        ranks[step.index] = rank;
        if rank == 0 || queue(step) != queue(&steps[rank - 1]) {
            runs.push(rank..rank + 1);
        } else {
            runs.last_mut().unwrap().end = rank + 1;
        }
    }
    if !release_ready {
        return runs;
    }
    let mut batches = Vec::with_capacity(runs.len());
    for (i, run) in runs.iter().enumerate() {
        let last_producer = runs.get(i + 1).and_then(|next| {
            steps[next.clone()]
                .iter()
                .flat_map(|s| &s.parents)
                .map(|&id| ranks[id])
                .filter(|rank| run.contains(rank))
                .max()
        });
        if let Some(cut) = last_producer
            .map(|rank| rank + 1)
            .filter(|&cut| steps[cut..run.end].iter().any(&has_device_work))
        {
            batches.push(run.start..cut);
            batches.push(cut..run.end);
        } else {
            batches.push(run.clone());
        }
    }
    batches
}

/// A signal/wait provides availability and visibility for a buffer read on another
/// physical queue. Preserve image transitions, host visibility, and frame-external
/// producers. Aliasing barriers are installed after this pass.
pub(super) fn remove_semaphore_covered_reads(plan: &mut Plan, queue_count: usize) {
    if queue_count == 1 {
        return;
    }
    let mut writer = vec![None; plan.graph.resources.len()];
    for step in &mut plan.steps {
        let queue = physical_queue(plan.graph.passes[step.index].queue, queue_count);
        step.barriers.retain(|barrier| {
            barrier.to.writes()
                || barrier.to == Access::HostRead
                || plan.graph.image_desc(barrier.resource).is_some()
                || !writer[barrier.resource.0].is_some_and(|(producer, source)| {
                    source != queue && step.parents.contains(&producer)
                })
        });
        for &(r, access) in &plan.pass_uses[step.index] {
            if access.writes() {
                writer[r.0] = Some((step.index, queue));
            }
        }
    }
}

/// Queues in one universal family share ownership of all frame resources. Each physical
/// queue has its own monotonically increasing semaphore, including on two-queue devices.
pub(super) struct Timelines {
    gpu: Arc<Gpu>,
    sems: Vec<vk::Semaphore>,
    values: Vec<u64>,
    histories: HashMap<u64, Vec<u64>>,
    release_ready: bool,
}
impl Timelines {
    pub fn new(g: &Arc<Gpu>) -> Result<Self> {
        let release_ready = match std::env::var("RENDER_LAB_GPU_BATCHING").as_deref() {
            Ok("compact") | Err(std::env::VarError::NotPresent) => false,
            Ok("release") => true,
            _ => anyhow::bail!("RENDER_LAB_GPU_BATCHING must be release or compact"),
        };
        let mut result = Self {
            release_ready,
            gpu: g.clone(),
            sems: vec![],
            histories: HashMap::new(),
            values: vec![0; g.queues.len()],
        };
        for _ in &g.queues {
            let mut ty =
                vk::SemaphoreTypeCreateInfo::builder().semaphore_type(vk::SemaphoreType::TIMELINE);
            result.sems.push(unsafe {
                g.dev.create_semaphore(
                    &vk::SemaphoreCreateInfo::builder().push_next(&mut ty),
                    None,
                )?
            });
        }
        Ok(result)
    }
    pub fn submit(
        &mut self,
        g: &Gpu,
        p: &Plan,
        recorders: &[Recorder],
        resources: &[Physical],
        image: Option<present::Acquired>,
    ) -> Result<Vec<Arc<frame::QueueBatch>>> {
        let queue = |step: &Step| physical_queue(p.graph.passes[step.index].queue, g.queues.len());
        let previous = std::mem::take(&mut self.histories);
        let uses = &p.pass_uses;
        let mut signals = vec![(0, 0u64); p.steps.len()];
        let mut covered = vec![vec![(0u64, Stage::empty()); g.queues.len()]; g.queues.len()];
        let mut batches = Vec::with_capacity(p.steps.len());
        let has_device_work = |step: &Step| {
            !matches!(
                p.graph.passes[step.index].op,
                Op::HostRead(_) | Op::Counters { .. }
            )
        };
        for range in submission_ranges(&p.steps, queue, has_device_work, self.release_ready) {
            let (start, end) = (range.start, range.end);
            let q = queue(&p.steps[start]);
            self.values[q] += 1;
            let value = self.values[q];
            let mut needed = vec![(0u64, Stage::empty()); g.queues.len()];
            let mut require = |src: usize, value: u64, stage: Stage| {
                if value != 0 {
                    needed[src].0 = needed[src].0.max(value);
                    needed[src].1 |= stage;
                }
            };
            let mut present = false;
            for step in &p.steps[start..end] {
                if let Op::BuildBlas { output, .. } = p.graph.passes[step.index].op
                    && let Physical::StaticBlas { blas, .. } = &resources[output.0]
                    && let Some(&(src, value)) = blas.ready.get()
                    && src != q
                {
                    require(src, value, Stage::ALL_COMMANDS);
                }
                for &parent in &step.parents {
                    let (src, v) = signals[parent];
                    if src != q {
                        let pass = &p.graph.passes[step.index];
                        let stages = uses[step.index]
                            .iter()
                            .filter(|(r, access)| {
                                uses[parent].iter().any(|(prior, source)| {
                                    prior == r
                                        && (source.writes()
                                            || access.writes()
                                            || p.graph.image_desc(*r).is_some())
                                })
                            })
                            .fold(Stage::empty(), |stages, (_, a)| stages | wait_stage(*a));
                        require(
                            src,
                            v,
                            if stages.is_empty() || pass.after.contains(&parent) {
                                Stage::ALL_COMMANDS
                            } else {
                                stages
                            },
                        );
                    }
                }
                for &(r, a) in &uses[step.index] {
                    if let Some(key) = p.graph.histories.get(&r) {
                        if let Some(prior) = previous.get(key) {
                            for (src, &point) in prior.iter().enumerate() {
                                require(src, point, wait_stage(a));
                            }
                        }
                        self.histories
                            .entry(*key)
                            .or_insert_with(|| vec![0; g.queues.len()])[q] = value;
                    }
                }
                signals[step.index] = (q, value);
                present |= matches!(p.graph.passes[step.index].op, Op::Present(_));
            }
            let sem = |s, v, stage| {
                vk::SemaphoreSubmitInfo::builder()
                    .semaphore(s)
                    .value(v)
                    .stage_mask(stage)
                    .build()
            };
            let mut waits = vec![];
            let mut sources = vec![];
            for (src, &(v, stages)) in needed.iter().enumerate() {
                let (done, mask) = &mut covered[q][src];
                if v == 0
                    || (*done >= v && (mask.contains(stages) || mask.contains(Stage::ALL_COMMANDS)))
                {
                    continue;
                }
                waits.push(sem(self.sems[src], v, stages));
                sources.push((src, v));
                if v > *done {
                    *mask = stages;
                    *done = v;
                } else if v == *done {
                    *mask |= stages;
                }
            }
            let batch = Arc::new(frame::QueueBatch {
                queue: q,
                signal: value,
                waits: sources,
            });
            batches.extend(std::iter::repeat_n(batch, end - start));
            let mut signal = vec![sem(self.sems[q], value, Stage::ALL_COMMANDS)];
            if let Some(img) = image.filter(|_| present) {
                // The acquire must also order the initial layout transition.
                waits.push(sem(img.available, 0, Stage::ALL_COMMANDS));
                signal.push(sem(img.finished, 0, Stage::ALL_COMMANDS));
            }
            let cmds = recorders[start..end]
                .iter()
                .map(|r| {
                    vk::CommandBufferSubmitInfo::builder()
                        .command_buffer(r.cmd)
                        .build()
                })
                .collect::<Vec<_>>();
            let info = vk::SubmitInfo2::builder()
                .wait_semaphore_infos(&waits)
                .signal_semaphore_infos(&signal)
                .command_buffer_infos(&cmds)
                .build();
            unsafe {
                g.dev
                    .queue_submit2(g.queues[q], &[info], vk::Fence::null())?;
            }
        }
        Ok(batches)
    }
    pub fn targets(&self) -> Vec<u64> {
        self.values.clone()
    }
    pub fn retire(&self, g: &Gpu, values: &[u64]) -> Result<()> {
        unsafe {
            g.dev.wait_semaphores(
                &vk::SemaphoreWaitInfo::builder()
                    .semaphores(&self.sems)
                    .values(values),
                u64::MAX,
            )?;
        }
        Ok(())
    }
}
pub(super) fn physical_queue(queue: Queue, count: usize) -> usize {
    match queue {
        Queue::Graphics => 0,
        Queue::Compute => 1.min(count - 1),
        Queue::Transfer if count > 2 => 2,
        Queue::Transfer => 0,
    }
}
fn wait_stage(access: Access) -> Stage {
    let stage = record::scope(access).0;
    if stage == Stage::HOST {
        Stage::ALL_COMMANDS
    } else {
        stage
    }
}
impl Drop for Timelines {
    fn drop(&mut self) {
        unsafe {
            for &s in &self.sems {
                self.gpu.dev.destroy_semaphore(s, None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_after_last_producer_needed_by_the_entire_next_queue_run() {
        // Submission ranks deliberately differ from registration IDs.
        let mut steps = [
            (2, vec![]),
            (0, vec![]),
            (4, vec![]),
            (1, vec![2]),
            (3, vec![0]),
        ]
        .map(|(index, parents)| Step {
            index,
            parents,
            barriers: vec![],
        });
        let queue = |s: &Step| usize::from([1, 3].contains(&s.index));
        assert_eq!(
            submission_ranges(&steps, queue, |_| true, true),
            [0..2, 2..3, 3..5]
        );
        assert_eq!(
            submission_ranges(&steps, queue, |s| s.index != 4, true),
            [0..3, 3..5]
        );
        assert_eq!(
            submission_ranges(&steps, queue, |_| true, false),
            [0..3, 3..5]
        );
        steps[4].parents.push(4);
        assert_eq!(
            submission_ranges(&steps, queue, |_| true, true),
            [0..3, 3..5]
        );
        let single = submission_ranges(&steps, |_| 0, |_| true, true);
        assert_eq!(single.len(), 1);
        assert_eq!(single[0], 0..5);
    }

    #[test]
    fn semaphore_covers_only_device_reads_on_distinct_physical_queues() -> Result<()> {
        for queues in 1..=3 {
            let mut g = Graph::new([1, 1]);
            let r = g.input("values", vec![0; 16]);
            for (queue, access) in [
                (Queue::Graphics, Access::ComputeWrite),
                (Queue::Transfer, Access::ComputeRead),
                (Queue::Compute, Access::ComputeRead),
            ] {
                let i = g.pass(
                    "dispatch",
                    Op::Compute {
                        program: Dispatch {
                            shader: Shader::Compute(&[]),
                            bindings: vec![(r, access)],
                            push: vec![],
                        },
                        groups: [1; 3],
                    },
                );
                g.passes[i].queue = queue;
            }
            g.pass("readback", Op::HostRead(r));
            let mut plan = g.compile()?;
            remove_semaphore_covered_reads(&mut plan, queues);
            let barrier = |id| {
                plan.steps
                    .iter()
                    .find(|s| s.index == id)
                    .unwrap()
                    .barriers
                    .len()
            };
            assert_eq!(barrier(1), usize::from(queues < 3));
            assert_eq!(barrier(2), usize::from(queues == 1));
            assert_eq!(barrier(3), 1);
        }
        Ok(())
    }
}
