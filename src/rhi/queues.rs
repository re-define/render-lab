use super::*;
use vk::PipelineStageFlags2 as Stage;

/// Queues in one universal family share ownership of all frame resources. Each physical
/// queue has its own monotonically increasing semaphore, including on two-queue devices.
pub(super) struct Timelines {
    gpu: Arc<Gpu>,
    sems: Vec<vk::Semaphore>,
    values: Vec<u64>,
    histories: HashMap<u64, (usize, u64)>,
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
        let queue = |step: &Step| match p.graph.passes[step.index].queue {
            Queue::Graphics => 0,
            Queue::Compute => 1.min(g.queues.len() - 1),
            Queue::Transfer => {
                if g.queues.len() > 2 {
                    2
                } else {
                    0
                }
            }
        };
        let mut previous = std::mem::take(&mut self.histories);
        let mut signals = vec![(0, 0u64); p.steps.len()];
        let mut covered = vec![vec![0u64; g.queues.len()]; g.queues.len()];
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
            let mut needed = vec![0u64; g.queues.len()];
            let mut present = false;
            for step in &p.steps[start..end] {
                for &parent in &step.parents {
                    let (src, v) = signals[parent];
                    if src != q {
                        needed[src] = needed[src].max(v);
                    }
                }
                for (r, a) in p.graph.uses(&p.graph.passes[step.index].op) {
                    if let Some(key) = p.graph.histories.get(&r) {
                        if let Some(&(src, v)) = previous.get(key)
                            && src != q
                        {
                            needed[src] = needed[src].max(v);
                        }
                        if a.writes() {
                            previous.remove(key);
                            self.histories.insert(*key, (q, value));
                        }
                    }
                }
                signals[step.index] = (q, value);
                present |= matches!(p.graph.passes[step.index].op, Op::Present(_));
            }
            let sem = |s, v| {
                vk::SemaphoreSubmitInfo::builder()
                    .semaphore(s)
                    .value(v)
                    .stage_mask(Stage::ALL_COMMANDS)
                    .build()
            };
            let mut waits = vec![];
            for (src, &v) in needed.iter().enumerate() {
                if v > covered[q][src] {
                    waits.push(sem(self.sems[src], v));
                    covered[q][src] = v;
                }
            }
            timeline_waits += waits.len();
            let mut signal = vec![sem(self.sems[q], value)];
            if let Some(img) = image.filter(|_| present) {
                waits.push(sem(img.available, 0));
                signal.push(sem(img.finished, 0));
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
    pub fn retire(&self, g: &Gpu) -> Result<()> {
        unsafe {
            g.dev.wait_semaphores(
                &vk::SemaphoreWaitInfo::builder()
                    .semaphores(&self.sems)
                    .values(&self.values),
                u64::MAX,
            )?;
        }
        Ok(())
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
