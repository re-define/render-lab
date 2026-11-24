use super::*;
use vk::{AccessFlags2 as A, ImageLayout as L, PipelineStageFlags2 as S};
use vulkanalia::vk::ExtMeshShaderExtensionDeviceCommands;
pub(super) fn scope(a: Access) -> (S, A, L) {
    use Access::*;
    match a {
        Indirect => (S::DRAW_INDIRECT, A::INDIRECT_COMMAND_READ, L::UNDEFINED),
        Mesh => (S::MESH_SHADER_EXT, A::SHADER_STORAGE_READ, L::UNDEFINED),
        MeshFragment => (
            S::MESH_SHADER_EXT | S::FRAGMENT_SHADER,
            A::SHADER_STORAGE_READ,
            L::UNDEFINED,
        ),
        Vertex => (S::VERTEX_SHADER, A::SHADER_STORAGE_READ, L::UNDEFINED),
        Fragment => (S::FRAGMENT_SHADER, A::SHADER_STORAGE_READ, L::UNDEFINED),
        ComputeRead => (S::COMPUTE_SHADER, A::SHADER_STORAGE_READ, L::UNDEFINED),
        ComputeSample => (
            S::COMPUTE_SHADER,
            A::SHADER_SAMPLED_READ,
            L::SHADER_READ_ONLY_OPTIMAL,
        ),
        ComputeWrite => (S::COMPUTE_SHADER, A::SHADER_STORAGE_WRITE, L::UNDEFINED),
        SdkImageRead => (S::ALL_COMMANDS, A::MEMORY_READ, L::GENERAL),
        SdkImageWrite => (S::ALL_COMMANDS, A::MEMORY_WRITE, L::GENERAL),
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
        CopyDst => (S::COPY, A::TRANSFER_WRITE, L::TRANSFER_DST_OPTIMAL),
        HostRead => (S::HOST, A::HOST_READ, L::UNDEFINED),
        ClusterBuildInput => (
            S::ACCELERATION_STRUCTURE_BUILD_KHR,
            A::INDIRECT_COMMAND_READ | A::SHADER_READ | A::ACCELERATION_STRUCTURE_READ_KHR,
            L::UNDEFINED,
        ),
        AccelerationInput => (
            S::ACCELERATION_STRUCTURE_BUILD_KHR,
            A::SHADER_READ,
            L::UNDEFINED,
        ),
        AccelerationRead => (
            S::ACCELERATION_STRUCTURE_BUILD_KHR,
            A::ACCELERATION_STRUCTURE_READ_KHR,
            L::UNDEFINED,
        ),
        AccelerationWrite => (
            S::ACCELERATION_STRUCTURE_BUILD_KHR,
            A::ACCELERATION_STRUCTURE_WRITE_KHR,
            L::UNDEFINED,
        ),
        ComputeAcceleration => (
            S::COMPUTE_SHADER,
            A::ACCELERATION_STRUCTURE_READ_KHR,
            L::UNDEFINED,
        ),
        RayAcceleration => (
            S::RAY_TRACING_SHADER_KHR,
            A::ACCELERATION_STRUCTURE_READ_KHR,
            L::UNDEFINED,
        ),
        RayRead => (
            S::RAY_TRACING_SHADER_KHR,
            A::SHADER_STORAGE_READ,
            L::UNDEFINED,
        ),
        RayReadWrite => (
            S::RAY_TRACING_SHADER_KHR,
            A::SHADER_STORAGE_READ | A::SHADER_STORAGE_WRITE,
            L::UNDEFINED,
        ),
        RayWrite => (
            S::RAY_TRACING_SHADER_KHR,
            A::SHADER_STORAGE_WRITE,
            L::UNDEFINED,
        ),
    }
}
impl Recorder {
    unsafe fn barriers(&mut self, graph: &Graph, step: &Step, res: &[Physical]) {
        let mut images = vec![];
        let mut memory = vk::MemoryBarrier2::default();
        let pass = &graph.passes[step.index];
        let count = self.gpu.queues.len();
        let queue = queues::physical_queue(pass.queue, count);
        // Submission order alone does not establish an execution dependency.
        if pass
            .after
            .iter()
            .any(|&id| queues::physical_queue(graph.passes[id].queue, count) == queue)
        {
            memory.src_stage_mask = S::ALL_COMMANDS;
            memory.dst_stage_mask = S::ALL_COMMANDS;
        }
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
                        // Cross-queue waits target the consuming stage. Chain that wait
                        // into the layout transition itself, including read-to-read moves.
                        .src_stage_mask(if old != L::UNDEFINED && old != layout {
                            src | dst
                        } else {
                            src
                        })
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
                Physical::Buffer(_)
                | Physical::SharedBuffer(_)
                | Physical::StaticBlas { .. }
                | Physical::Acceleration(_)
                | Physical::Clusters(_)
                    if !src.is_empty() =>
                {
                    memory.src_stage_mask |= src;
                    memory.dst_stage_mask |= dst;
                    // A read followed by a write only needs execution order. Make only
                    // prior writes available, including writes inherited from an alias.
                    if b.from.iter().any(|a| a.writes()) {
                        memory.src_access_mask |= access
                            & (A::HOST_WRITE
                                | A::SHADER_STORAGE_WRITE
                                | A::TRANSFER_WRITE
                                | A::ACCELERATION_STRUCTURE_WRITE_KHR
                                | A::MEMORY_WRITE);
                        memory.dst_access_mask |= next;
                    }
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
        self.barriers(&plan.graph, step, res);
        let op = &plan.graph.passes[step.index].op;
        if gpu.raster_queries && matches!(op, Op::Draw { .. }) {
            if self.raster.is_none() {
                self.raster = Some(raster_queries::Pools::new(&gpu)?);
            }
            self.raster.as_ref().unwrap().reset(cmd);
        }
        if let Some(p) = op.program() {
            let point = match p.shader {
                Shader::Graphics(..) | Shader::Mesh(..) => vk::PipelineBindPoint::GRAPHICS,
                Shader::RayTracing(..) | Shader::ClusterShadows(..) | Shader::ClusterRays(..) => {
                    vk::PipelineBindPoint::RAY_TRACING_KHR
                }
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
            if let Op::TraceRays { tlas, .. } | Op::RayQuery { tlas, .. } = *op {
                let Physical::Acceleration(tlas) = &res[tlas.0] else {
                    unreachable!()
                };
                data.extend(tlas.address.to_ne_bytes());
            }
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
        match *op {
            Op::Draw {
                color,
                depth,
                clear,
                count,
                ..
            } => {
                let [width, height] = plan.graph.image_desc(color).unwrap().extent;
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
                if let Some(q) = &self.raster {
                    q.begin(cmd);
                }
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
                if let Some(q) = &self.raster {
                    q.end(cmd);
                }
                d.cmd_end_rendering(cmd);
            }
            Op::RayQuery {
                groups: [x, y, z], ..
            }
            | Op::Compute {
                groups: [x, y, z], ..
            } => d.cmd_dispatch(cmd, x, y, z),
            Op::ComputeIndirect { args, offset, .. } => {
                d.cmd_dispatch_indirect(cmd, res[args.0].buffer().raw, offset)
            }
            Op::BuildClusters {
                records,
                count,
                addresses,
                output,
                ..
            }
            | Op::BuildClusterBottom {
                record: records,
                addresses,
                output,
                clusters: count,
            } => {
                let Physical::Clusters(allocation) = &res[output.0] else {
                    unreachable!()
                };
                let key = pipes
                    .iter()
                    .find(|((shader, _), _)| shader.supports_clusters())
                    .context("missing cluster-capable RT pipeline")?
                    .1;
                d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::RAY_TRACING_KHR, key.1);
                let triangle_build = matches!(op, Op::BuildClusters { .. });
                allocation.record(
                    &gpu,
                    cmd,
                    res[records.0].buffer().address,
                    if triangle_build {
                        res[count.0].buffer().address
                    } else {
                        0
                    },
                    if triangle_build {
                        res[addresses.0].buffer().address
                    } else {
                        0
                    },
                );
            }
            Op::BuildBlas {
                vertices,
                indices,
                output,
                ..
            } => {
                let (acceleration, build) = match &res[output.0] {
                    Physical::StaticBlas { blas, scratch } => {
                        (&blas.acceleration, scratch.is_some())
                    }
                    Physical::Acceleration(a) => (a.as_ref(), a.needs_build),
                    _ => unreachable!(),
                };
                if build {
                    acceleration.record(
                        cmd,
                        Some((
                            res[vertices.0].buffer().address,
                            res[indices.0].buffer().address,
                        )),
                    );
                }
            }
            Op::BuildTlas { output, .. } => {
                let Physical::Acceleration(acceleration) = &res[output.0] else {
                    unreachable!()
                };
                if acceleration.needs_build {
                    acceleration.record(cmd, None);
                }
            }
            Op::TraceRays {
                ref program,
                extent,
                ..
            } => {
                pipes[&(program.shader, program.bindings.len())]
                    .2
                    .as_ref()
                    .unwrap()
                    .trace(d, cmd, extent);
            }
            Op::ReadPixels { image, buffer } | Op::WritePixels { image, buffer } => {
                let [width, height] = plan.graph.image_desc(image).unwrap().extent;
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
                let image = res[image.0].image().raw;
                let buffer = res[buffer.0].buffer().raw;
                if matches!(op, Op::ReadPixels { .. }) {
                    d.cmd_copy_image_to_buffer(
                        cmd,
                        image,
                        L::TRANSFER_SRC_OPTIMAL,
                        buffer,
                        &[region],
                    );
                } else {
                    d.cmd_copy_buffer_to_image(
                        cmd,
                        buffer,
                        image,
                        L::TRANSFER_DST_OPTIMAL,
                        &[region],
                    );
                }
            }
            Op::Dlss(dlss) => {
                // Vendor temporal intermediates are private to the SDK and reused
                // across dispatches on this same physical queue.
                let memory = [vk::MemoryBarrier2::builder()
                    .src_stage_mask(S::ALL_COMMANDS)
                    .src_access_mask(A::MEMORY_WRITE)
                    .dst_stage_mask(S::ALL_COMMANDS)
                    .dst_access_mask(A::MEMORY_READ | A::MEMORY_WRITE)
                    .build()];
                d.cmd_pipeline_barrier2(
                    cmd,
                    &vk::DependencyInfo::builder().memory_barriers(&memory),
                );
                self.barriers_count[0] += 1;
                self.barriers_count[1] += 1;
                let images = [dlss.color, dlss.depth, dlss.motion, dlss.output]
                    .map(|r| (res[r.0].image(), plan.graph.image_desc(r).unwrap()));
                self.gpu
                    .ngx
                    .as_ref()
                    .context("DLSS unavailable")?
                    .evaluate(d, cmd, images, dlss)?;
            }
            Op::Present(r) => {
                if let Some(frame) = image {
                    frame.copy(d, cmd, res[r.0].buffer().raw, plan.graph.output_extent());
                }
            }
            Op::HostRead(_) | Op::Counters { .. } => (),
        }
        d.cmd_write_timestamp2(cmd, S::BOTTOM_OF_PIPE, self.query, 1);
        d.end_command_buffer(cmd)?;
        Ok(())
    }
}
