use super::*;
use vk::PipelineStageFlags2 as Stage;

/// Queues in one universal family share ownership of all frame resources. Each physical
/// queue has its own monotonically increasing semaphore, including on two-queue devices.
pub(super) struct Timelines {
    gpu: Arc<Gpu>,
    sems: Vec<vk::Semaphore>,
    values: Vec<u64>,
    histories: HashMap<u64, Vec<u64>>,
}
impl Timelines {
    pub fn new(g: &Arc<Gpu>) -> Result<Self> {
        let mut result = Self {
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
        image: Option<present::Acquired>,
    ) -> Result<(usize, usize)> {
        let queue = |step: &Step| physical_queue(p.graph.passes[step.index].queue, g.queues.len());
        let previous = std::mem::take(&mut self.histories);
        let uses = &p.pass_uses;
        let mut signals = vec![(0, 0u64); p.steps.len()];
        let mut covered = vec![vec![(0u64, Stage::empty()); g.queues.len()]; g.queues.len()];
        let mut start = 0;
        let mut submits = 0;
        let mut timeline_waits = 0;
        while start < p.steps.len() {
            let q = queue(&p.steps[start]);
            let mut end = start + 1;
            while end < p.steps.len() && queue(&p.steps[end]) == q {
                end += 1;
            }
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
                for &parent in &step.parents {
                    let (src, v) = signals[parent];
                    if src != q {
                        let pass = &p.graph.passes[step.index];
                        let stages = uses[step.index]
                            .iter()
                            .filter(|(r, _)| uses[parent].iter().any(|(written, _)| written == r))
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
            for (src, &(v, stages)) in needed.iter().enumerate() {
                let (done, mask) = &mut covered[q][src];
                if v == 0
                    || (*done >= v && (mask.contains(stages) || mask.contains(Stage::ALL_COMMANDS)))
                {
                    continue;
                }
                waits.push(sem(self.sems[src], v, stages));
                if v > *done {
                    *mask = stages;
                    *done = v;
                } else if v == *done {
                    *mask |= stages;
                }
            }
            timeline_waits += waits.len();
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
            start = end;
            submits += 1;
        }
        Ok((submits, timeline_waits))
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
