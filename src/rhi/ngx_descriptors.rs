use super::*;

/// NGX uses traditional descriptor sets internally. Establish that bind state in
/// its command buffer before handing it to the SDK (NVIDIA nvbug 6436675).
pub(super) struct VendorDescriptors {
    device: vulkanalia::Device,
    layout: vk::DescriptorSetLayout,
    pipeline: vk::PipelineLayout,
    pool: vk::DescriptorPool,
    set: vk::DescriptorSet,
}

impl VendorDescriptors {
    pub(super) fn new(device: &vulkanalia::Device) -> Result<Self> {
        let mut state = Self {
            device: device.clone(),
            layout: vk::DescriptorSetLayout::null(),
            pipeline: vk::PipelineLayout::null(),
            pool: vk::DescriptorPool::null(),
            set: vk::DescriptorSet::null(),
        };
        // No engine shader reads this binding. A nonempty layout ensures the
        // driver installs a conventional descriptor table for vendor kernels.
        unsafe {
            let binding = [vk::DescriptorSetLayoutBinding::builder()
                .binding(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
                .build()];
            state.layout = device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::builder().bindings(&binding),
                None,
            )?;
            let layouts = [state.layout];
            state.pipeline = device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::builder().set_layouts(&layouts),
                None,
            )?;
            let sizes = [vk::DescriptorPoolSize {
                type_: vk::DescriptorType::UNIFORM_BUFFER,
                descriptor_count: 1,
            }];
            state.pool = device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::builder()
                    .max_sets(1)
                    .pool_sizes(&sizes),
                None,
            )?;
            state.set = device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::builder()
                    .descriptor_pool(state.pool)
                    .set_layouts(&layouts),
            )?[0];
        }
        Ok(state)
    }

    pub(super) unsafe fn bind(&self, cmd: vk::CommandBuffer) {
        unsafe {
            self.device.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline,
                0,
                &[self.set],
                &[],
            )
        };
    }
}

impl Drop for VendorDescriptors {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_descriptor_pool(self.pool, None);
            self.device.destroy_pipeline_layout(self.pipeline, None);
            self.device.destroy_descriptor_set_layout(self.layout, None);
        }
    }
}
