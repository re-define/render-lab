use super::*;
use vk::{AccessFlags2 as A, ImageLayout as L, PipelineStageFlags2 as S};
use vulkanalia::vk::ExtMeshShaderExtensionDeviceCommands;
fn scope(a: Access) -> (S, A, L) {
    use Access::*;
    match a {
        Indirect => (S::DRAW_INDIRECT, A::INDIRECT_COMMAND_READ, L::UNDEFINED),
        Upload => (S::HOST, A::HOST_WRITE, L::UNDEFINED),
        Mesh => (S::MESH_SHADER_EXT, A::SHADER_STORAGE_READ, L::UNDEFINED),
        Vertex => (S::VERTEX_SHADER, A::SHADER_STORAGE_READ, L::UNDEFINED),
        Fragment => (S::FRAGMENT_SHADER, A::SHADER_STORAGE_READ, L::UNDEFINED),
        ComputeRead => (S::COMPUTE_SHADER, A::SHADER_STORAGE_READ, L::UNDEFINED),
        ComputeWrite => (S::COMPUTE_SHADER, A::SHADER_STORAGE_WRITE, L::UNDEFINED),
        ComputeReadWrite => (
            S::COMPUTE_SHADER,
            A::SHADER_STORAGE_READ | A::SHADER_STORAGE_WRITE,
            L::UNDEFINED,
        ),
        ColorClear | ColorLoad => (
            S::COLOR_ATTACHMENT_OUTPUT,
            A::COLOR_ATTACHMENT_WRITE | A::COLOR_ATTACHMENT_READ,
            L::COLOR_ATTACHMENT_OPTIMAL,
        ),
        DepthClear | DepthLoad => (
            S::EARLY_FRAGMENT_TESTS | S::LATE_FRAGMENT_TESTS,
            A::DEPTH_STENCIL_ATTACHMENT_READ | A::DEPTH_STENCIL_ATTACHMENT_WRITE,
            L::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
        ),
        CopySrc => (S::COPY, A::TRANSFER_READ, L::TRANSFER_SRC_OPTIMAL),
        CopyDst => (S::COPY, A::TRANSFER_WRITE, L::UNDEFINED),
        HostRead => (S::HOST, A::HOST_READ, L::UNDEFINED),
    }
}
impl Recorder {
    unsafe fn barriers(&mut self, step: &Step, res: &[Physical]) {
        let mut images = vec![];
        let mut memory = vk::MemoryBarrier2::default();
        for b in &step.barriers {
            let (mut src, mut access, mut old) = (S::empty(), A::empty(), L::UNDEFINED);
            for &a in &b.from {
                let (s, m, l) = scope(a);
                src |= s;
                access |= m;
                old = l;
            }
            let (dst, next, layout) = scope(b.to);
            match &res[b.resource.0] {
                Physical::Image(i, ..) => images.push(
                    vk::ImageMemoryBarrier2::builder()
                        .src_stage_mask(src)
                        .src_access_mask(access)
                        .dst_stage_mask(dst)
                        .dst_access_mask(next)
                        .old_layout(old)
                        .new_layout(layout)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(i.raw)
                        .subresource_range(i.range)
                        .build(),
                ),
                Physical::Buffer(_) if !src.is_empty() => {
                    memory.src_stage_mask |= src;
                    memory.src_access_mask |= access;
                    memory.dst_stage_mask |= dst;
                    memory.dst_access_mask |= next;
                }
                _ => (),
            }
        }
        let mem = if memory.src_stage_mask.is_empty() {
            &[][..]
        } else {
            std::slice::from_ref(&memory)
        };
        self.barriers_count = [
            usize::from(!mem.is_empty() || !images.is_empty()),
            mem.len(),
            images.len(),
        ];
        if !mem.is_empty() || !images.is_empty() {
            self.gpu.dev.cmd_pipeline_barrier2(
                self.cmd,
                &vk::DependencyInfo::builder()
                    .memory_barriers(mem)
                    .image_memory_barriers(&images),
            );
        }
    }
    pub(super) unsafe fn record(
        &mut self,
        plan: &Plan,
        step: &Step,
        res: &[Physical],
        heap: &vk::BindHeapInfoEXT,
        pipes: &HashMap<(Shader, usize), Pipeline>,
        image: Option<present::Acquired>,
    ) -> Result<()> {
        let cmd = self.cmd;
        let gpu = self.gpu.clone();
        let d = &gpu.dev;
        d.reset_command_pool(self.pool, vk::CommandPoolResetFlags::empty())?;
        d.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;
        d.cmd_reset_query_pool(cmd, self.query, 0, 2);
        d.cmd_write_timestamp2(cmd, S::TOP_OF_PIPE, self.query, 0);
        self.barriers(step, res);
        let op = &plan.graph.passes[step.index].op;
        if let Some(p) = op.program() {
            let point = match p.shader {
                Shader::Graphics(..) | Shader::Mesh(..) => vk::PipelineBindPoint::GRAPHICS,
                _ => vk::PipelineBindPoint::COMPUTE,
            };
            d.cmd_bind_pipeline(cmd, point, pipes[&(p.shader, p.bindings.len())].1);
            d.cmd_bind_resource_heap_ext(cmd, heap);
            let mut data = p
                .bindings
                .iter()
                .flat_map(|(r, _)| (r.0 as u32).to_ne_bytes())
                .collect::<Vec<_>>();
            data.extend(&p.push);
            if !data.is_empty() {
                d.cmd_push_data_ext(
                    cmd,
                    &vk::PushDataInfoEXT {
                        data: vk::HostAddressRangeConstEXT {
                            address: data.as_ptr().cast(),
                            size: data.len(),
                        },
                        ..Default::default()
                    },
                );
            }
        }
        let [width, height] = plan.graph.extent;
        match *op {
            Op::Draw {
                color,
                depth,
                clear,
                count,
                ..
            } => {
                let load = if clear {
                    vk::AttachmentLoadOp::CLEAR
                } else {
                    vk::AttachmentLoadOp::LOAD
                };
                let colors = [vk::RenderingAttachmentInfo::builder()
                    .image_view(res[color.0].image().view)
                    .image_layout(L::COLOR_ATTACHMENT_OPTIMAL)
                    .load_op(load)
                    .store_op(vk::AttachmentStoreOp::STORE)
                    .clear_value(vk::ClearValue {
                        color: vk::ClearColorValue { uint32: [0; 4] },
                    })
                    .build()];
                let z = vk::RenderingAttachmentInfo::builder()
                    .image_view(res[depth.0].image().view)
                    .image_layout(L::DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
                    .load_op(load)
                    .store_op(vk::AttachmentStoreOp::STORE)
                    .clear_value(vk::ClearValue {
                        depth_stencil: vk::ClearDepthStencilValue {
                            depth: 1.,
                            stencil: 0,
                        },
                    });
                let rect = vk::Rect2D {
                    extent: vk::Extent2D { width, height },
                    ..Default::default()
                };
                d.cmd_begin_rendering(
                    cmd,
                    &vk::RenderingInfo::builder()
                        .render_area(rect)
                        .layer_count(1)
                        .color_attachments(&colors)
                        .depth_attachment(&z),
                );
                d.cmd_set_viewport(
                    cmd,
                    0,
                    &[vk::Viewport {
                        width: width as f32,
                        height: height as f32,
                        max_depth: 1.,
                        ..Default::default()
                    }],
                );
                d.cmd_set_scissor(cmd, 0, &[rect]);
                match count {
                    DrawCount::Mesh([x, y, z]) => d.cmd_draw_mesh_tasks_ext(cmd, x, y, z),
                    DrawCount::MeshIndirect { args, offset } => d.cmd_draw_mesh_tasks_indirect_ext(
                        cmd,
                        res[args.0].buffer().raw,
                        offset,
                        1,
                        12,
                    ),
                    DrawCount::Vertices(n) => d.cmd_draw(cmd, n, 1, 0, 0),
                    DrawCount::Indirect(r)
                        if matches!(op.program().unwrap().shader, Shader::Mesh(..)) =>
                    {
                        d.cmd_draw_mesh_tasks_indirect_ext(cmd, res[r.0].buffer().raw, 0, 1, 12)
                    }
                    DrawCount::Indirect(r) => {
                        d.cmd_draw_indirect(cmd, res[r.0].buffer().raw, 0, 1, 16)
                    }
                }
                d.cmd_end_rendering(cmd);
            }
            Op::Compute {
                groups: [x, y, z], ..
            } => d.cmd_dispatch(cmd, x, y, z),
            Op::ComputeIndirect { args, offset, .. } => {
                d.cmd_dispatch_indirect(cmd, res[args.0].buffer().raw, offset)
            }
            Op::ReadPixels { image, buffer } => {
                let layers = vk::ImageSubresourceLayers::builder()
                    .aspect_mask(res[image.0].image().range.aspect_mask)
                    .layer_count(1);
                let region = vk::BufferImageCopy::builder()
                    .image_subresource(*layers)
                    .image_extent(vk::Extent3D {
                        width,
                        height,
                        depth: 1,
                    })
                    .build();
                d.cmd_copy_image_to_buffer(
                    cmd,
                    res[image.0].image().raw,
                    L::TRANSFER_SRC_OPTIMAL,
                    res[buffer.0].buffer().raw,
                    &[region],
                );
            }
            Op::Present(r) => {
                if let Some(frame) = image {
                    frame.copy(d, cmd, res[r.0].buffer().raw, plan.graph.extent);
                }
            }
            Op::HostRead(_) | Op::Counters { .. } => (),
        }
        d.cmd_write_timestamp2(cmd, S::BOTTOM_OF_PIPE, self.query, 1);
        d.end_command_buffer(cmd)?;
        Ok(())
    }
}
