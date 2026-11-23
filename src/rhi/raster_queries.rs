//! Optional native counters for visibility and overdraw studies.
use super::*;

pub(super) struct Pools {
    gpu: Arc<Gpu>,
    invocations: vk::QueryPool,
    samples: vk::QueryPool,
}

impl Pools {
    pub fn new(gpu: &Arc<Gpu>) -> Result<Self> {
        let info = vk::QueryPoolCreateInfo::builder().query_count(1);
        unsafe {
            let mut pools = Self {
                gpu: gpu.clone(),
                invocations: gpu.dev.create_query_pool(
                    &info
                        .query_type(vk::QueryType::PIPELINE_STATISTICS)
                        .pipeline_statistics(
                            vk::QueryPipelineStatisticFlags::FRAGMENT_SHADER_INVOCATIONS,
                        ),
                    None,
                )?,
                samples: vk::QueryPool::null(),
            };
            pools.samples = gpu.dev.create_query_pool(
                &vk::QueryPoolCreateInfo::builder()
                    .query_count(1)
                    .query_type(vk::QueryType::OCCLUSION),
                None,
            )?;
            Ok(pools)
        }
    }
    pub fn reset(&self, cmd: vk::CommandBuffer) {
        unsafe {
            for pool in [self.invocations, self.samples] {
                self.gpu.dev.cmd_reset_query_pool(cmd, pool, 0, 1);
            }
        }
    }
    pub fn begin(&self, cmd: vk::CommandBuffer) {
        unsafe {
            self.gpu
                .dev
                .cmd_begin_query(cmd, self.invocations, 0, vk::QueryControlFlags::empty());
            self.gpu
                .dev
                .cmd_begin_query(cmd, self.samples, 0, vk::QueryControlFlags::PRECISE);
        }
    }
    pub fn end(&self, cmd: vk::CommandBuffer) {
        unsafe {
            self.gpu.dev.cmd_end_query(cmd, self.samples, 0);
            self.gpu.dev.cmd_end_query(cmd, self.invocations, 0);
        }
    }
    pub fn counts(&self) -> Result<RasterCounts> {
        let mut values = [0_u64; 2];
        for (value, pool) in values.iter_mut().zip([self.invocations, self.samples]) {
            unsafe {
                self.gpu.dev.get_query_pool_results(
                    pool,
                    0,
                    1,
                    bytemuck::bytes_of_mut(value),
                    8,
                    vk::QueryResultFlags::_64 | vk::QueryResultFlags::WAIT,
                )?;
            }
        }
        Ok(RasterCounts {
            fragment_invocations: values[0],
            passed_samples: values[1],
        })
    }
}
impl Drop for Pools {
    fn drop(&mut self) {
        unsafe {
            self.gpu.dev.destroy_query_pool(self.samples, None);
            self.gpu.dev.destroy_query_pool(self.invocations, None);
        }
    }
}
