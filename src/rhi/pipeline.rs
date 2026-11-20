use super::*;
use device::{COLOR, DEPTH, Gpu};
struct Module<'a>(&'a vulkanalia::Device, vk::ShaderModule);
impl Drop for Module<'_> {
    fn drop(&mut self) {
        unsafe { self.0.destroy_shader_module(self.1, None) }
    }
}
pub(super) struct Pipeline(pub Arc<Gpu>, pub vk::Pipeline);
impl Drop for Pipeline {
    fn drop(&mut self) {
        unsafe { self.0.dev.destroy_pipeline(self.1, None) }
    }
}
impl Pipeline {
    pub fn new(g: &Arc<Gpu>, program: &Dispatch, stride: u32) -> Result<Self> {
        unsafe {
            let d = &g.dev;
            let mapping = program
                .bindings
                .iter()
                .enumerate()
                .map(|(i, _)| vk::DescriptorSetAndBindingMappingEXT {
                    first_binding: i as u32,
                    binding_count: 1,
                    resource_mask: vk::SpirvResourceTypeFlagsEXT::ALL,
                    source: vk::DescriptorMappingSourceEXT::HEAP_WITH_PUSH_INDEX,
                    source_data: vk::DescriptorMappingSourceDataEXT {
                        push_index: vk::DescriptorMappingSourcePushIndexEXT {
                            push_offset: 4 * i as u32,
                            heap_index_stride: stride,
                            heap_array_stride: stride,
                            ..Default::default()
                        },
                    },
                    ..Default::default()
                })
                .collect::<Vec<_>>();
            let map = vk::ShaderDescriptorSetAndBindingMappingInfoEXT {
                mapping_count: mapping.len() as u32,
                mappings: mapping.as_ptr(),
                ..Default::default()
            };
            let shaders = match program.shader {
                Shader::Compute(cs) => vec![(vk::ShaderStageFlags::COMPUTE, cs)],
                Shader::Mesh(m, f) => {
                    vec![
                        (vk::ShaderStageFlags::MESH_EXT, m),
                        (vk::ShaderStageFlags::FRAGMENT, f),
                    ]
                }
                Shader::Graphics(v, f) => vec![
                    (vk::ShaderStageFlags::VERTEX, v),
                    (vk::ShaderStageFlags::FRAGMENT, f),
                ],
            };
            let mut modules = vec![];
            let mut stages = vec![];
            for (stage, bytes) in shaders {
                ensure!(bytes.len() % 4 == 0, "invalid SPIR-V length");
                let code = bytes
                    .chunks_exact(4)
                    .map(|v| u32::from_le_bytes(v.try_into().unwrap()))
                    .collect::<Vec<_>>();
                let module = d.create_shader_module(
                    &vk::ShaderModuleCreateInfo {
                        code_size: bytes.len(),
                        code: code.as_ptr(),
                        ..Default::default()
                    },
                    None,
                )?;
                modules.push(Module(d, module));
                let mut s = vk::PipelineShaderStageCreateInfo::builder()
                    .module(module)
                    .name(b"main\0")
                    .stage(stage)
                    .build();
                s.next = (&map as *const vk::ShaderDescriptorSetAndBindingMappingInfoEXT).cast();
                stages.push(s);
            }
            let mut flags = vk::PipelineCreateFlags2CreateInfo::builder()
                .flags(vk::PipelineCreateFlags2::DESCRIPTOR_HEAP_EXT);
            let pipe = match program.shader {
                Shader::Compute(_) => {
                    let info = vk::ComputePipelineCreateInfo::builder()
                        .stage(stages[0])
                        .push_next(&mut flags)
                        .build();
                    d.create_compute_pipelines(vk::PipelineCache::null(), &[info], None)?
                        .0[0]
                }
                Shader::Graphics(..) | Shader::Mesh(..) => {
                    let vertex = vk::PipelineVertexInputStateCreateInfo::default();
                    let assembly = vk::PipelineInputAssemblyStateCreateInfo::builder()
                        .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
                    let viewport = vk::PipelineViewportStateCreateInfo::builder()
                        .viewport_count(1)
                        .scissor_count(1);
                    let raster = vk::PipelineRasterizationStateCreateInfo::builder()
                        .polygon_mode(vk::PolygonMode::FILL)
                        .line_width(1.);
                    let ms = vk::PipelineMultisampleStateCreateInfo::builder()
                        .rasterization_samples(vk::SampleCountFlags::_1);
                    let depth = vk::PipelineDepthStencilStateCreateInfo::builder()
                        .depth_test_enable(true)
                        .depth_write_enable(true)
                        .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
                    let attachments = [vk::PipelineColorBlendAttachmentState::builder()
                        .color_write_mask(vk::ColorComponentFlags::all())
                        .build()];
                    let blend =
                        vk::PipelineColorBlendStateCreateInfo::builder().attachments(&attachments);
                    let dynamic = vk::PipelineDynamicStateCreateInfo::builder()
                        .dynamic_states(&[vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR]);
                    let colors = [COLOR];
                    let mut rendering = vk::PipelineRenderingCreateInfo::builder()
                        .color_attachment_formats(&colors)
                        .depth_attachment_format(DEPTH);
                    let mut info = vk::GraphicsPipelineCreateInfo::builder()
                        .stages(&stages)
                        .vertex_input_state(&vertex)
                        .input_assembly_state(&assembly)
                        .viewport_state(&viewport)
                        .rasterization_state(&raster)
                        .multisample_state(&ms)
                        .depth_stencil_state(&depth)
                        .color_blend_state(&blend)
                        .dynamic_state(&dynamic)
                        .push_next(&mut flags)
                        .push_next(&mut rendering)
                        .build();
                    if matches!(program.shader, Shader::Mesh(..)) {
                        info.vertex_input_state = std::ptr::null();
                        info.input_assembly_state = std::ptr::null();
                    }
                    d.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None)?
                        .0[0]
                }
            };
            Ok(Self(g.clone(), pipe))
        }
    }
}
