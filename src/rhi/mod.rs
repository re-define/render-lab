//! RHI-owned queues, independently recorded command pools and timeline-retired frame resources.
#![allow(unsafe_op_in_unsafe_fn)]
mod device;
mod pipeline;
mod present;
mod queues;
mod record;
use crate::rdg::*;
use anyhow::{Context, Result, ensure};
use device::{Buffer, Gpu, Image};
use pipeline::Pipeline;
use rayon::prelude::*;
use std::{collections::HashMap, sync::Arc, time::Instant};
use vulkanalia::{prelude::v1_4::*, vk::ExtDescriptorHeapExtensionDeviceCommands};

enum Physical {
    Buffer(Buffer),
    Image(Image, [u32; 2], bool),
}
impl Physical {
    fn image(&self) -> &Image {
        match self {
            Self::Image(i, ..) => i,
            _ => unreachable!(),
        }
    }
    fn buffer(&self) -> &Buffer {
        match self {
            Self::Buffer(b) => b,
            _ => unreachable!(),
        }
    }
}
struct Heap {
    _buffer: Buffer,
    bind: vk::BindHeapInfoEXT,
    stride: u32,
    slots: Vec<Option<(u64, u64)>>,
    base: usize,
}
impl Heap {
    fn new(g: &Arc<Gpu>, count: usize) -> Result<Self> {
        let h = g.heap;
        let align = h.resource_heap_alignment.max(h.buffer_descriptor_alignment);
        let stride = h
            .buffer_descriptor_size
            .next_multiple_of(h.buffer_descriptor_alignment);
        let reserved = (stride * count as u64).next_multiple_of(align);
        let size = reserved + h.min_resource_heap_reserved_range;
        ensure!(
            size <= h.max_resource_heap_size,
            "descriptor heap capacity exceeded"
        );
        let buffer = Buffer::new(
            g,
            size + align,
            vk::BufferUsageFlags::DESCRIPTOR_HEAP_EXT,
            false,
        )?;
        let address = buffer.address.next_multiple_of(align);
        let base = (address - buffer.address) as usize;
        let bind = vk::BindHeapInfoEXT {
            heap_range: vk::DeviceAddressRangeEXT { address, size },
            reserved_range_offset: reserved,
            reserved_range_size: h.min_resource_heap_reserved_range,
            ..Default::default()
        };
        Ok(Self {
            _buffer: buffer,
            bind,
            stride: stride.try_into()?,
            slots: vec![None; count],
            base,
        })
    }
    fn write(&mut self, g: &Gpu, r: usize, buf: &Buffer) -> Result<()> {
        let binding = (buf.address, buf.size);
        if self.slots[r] == Some(binding) {
            return Ok(());
        }
        let range = vk::DeviceAddressRangeEXT {
            address: buf.address,
            size: buf.size,
        };
        let desc = vk::ResourceDescriptorInfoEXT {
            type_: vk::DescriptorType::STORAGE_BUFFER,
            data: vk::ResourceDescriptorDataEXT {
                address_range: &range,
            },
            ..Default::default()
        };
        self._buffer.mapped(|data| {
            let offset = self.base + r * self.stride as usize;
            let dst = vk::HostAddressRangeEXT {
                address: data[offset..].as_mut_ptr().cast(),
                size: g.heap.buffer_descriptor_size as usize,
            };
            unsafe {
                g.dev.write_resource_descriptors_ext(&[desc], &[dst])?;
            }
            Ok(())
        })?;
        self.slots[r] = Some(binding);
        Ok(())
    }
}
struct Recorder {
    barriers_count: [usize; 3],
    gpu: Arc<Gpu>,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    query: vk::QueryPool,
}
impl Recorder {
    fn new(g: &Arc<Gpu>) -> Result<Self> {
        unsafe {
            let pool = g.dev.create_command_pool(
                &vk::CommandPoolCreateInfo::builder().queue_family_index(g.family),
                None,
            )?;
            let mut r = Self {
                barriers_count: [0; 3],
                gpu: g.clone(),
                pool,
                cmd: vk::CommandBuffer::null(),
                query: vk::QueryPool::null(),
            };
            r.cmd = g.dev.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0];
            r.query = g.dev.create_query_pool(
                &vk::QueryPoolCreateInfo::builder()
                    .query_type(vk::QueryType::TIMESTAMP)
                    .query_count(2),
                None,
            )?;
            Ok(r)
        }
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        unsafe {
            self.gpu.dev.destroy_query_pool(self.query, None);
            self.gpu.dev.destroy_command_pool(self.pool, None)
        }
    }
}

#[derive(Default, Clone, serde::Serialize)]
pub struct AllocationInfo {
    pub allocated_bytes: u64,
    pub memory_type: u32,
    pub heap_index: u32,
    pub device_local: bool,
}

#[derive(Default, Clone, serde::Serialize)]
pub struct FrameMemory {
    pub atlas_buffer_bytes: u64,
    pub atlas: Option<AllocationInfo>,
    /// Explicit VkDeviceMemory allocations for live RDG resources and the descriptor heap.
    pub device_local_bytes: u64,
    pub nonlocal_bytes: u64,
}

#[derive(Default, Clone, serde::Serialize)]
pub struct FrameProfile {
    pub main_ms: f64,
    pub build_ms: f64,
    pub compile_ms: f64,
    pub rhi_prepare_ms: f64,
    pub acquire_ms: f64,
    pub record_ms: f64,
    pub submit_ms: f64,
    pub present_ms: f64,
    pub retire_ms: f64,
    pub rhi_ms: f64,
    pub gpu_span_ms: Option<f64>,
    pub task_workers: usize,
    pub recording_workers: usize,
    pub overlapping_recordings: usize,
    pub dependency_edges: usize,
    pub resource_barriers: usize,
    pub barrier_calls: usize,
    pub memory_barriers: usize,
    pub image_barriers: usize,
    pub timeline_waits: usize,
    pub timeline_signals: usize,
    pub physical_queues: usize,
    pub wsi_transitions: usize,
    pub wsi_binary_pairs: usize,
}
pub struct Frame {
    pub first_profile: Arc<FrameProfile>,
    pub profile: FrameProfile,
    pub completed: Instant,
    pub sequence: u64,
    pub pixels: Vec<u8>,
    pub counters: std::collections::BTreeMap<&'static str, u64>,
    pub extent: [u32; 2],
    pub passes: Vec<PassReport>,
    pub gpu: String,
    pub memory: FrameMemory,
    pub presentation: Option<String>,
    pub submissions: usize,
}
pub struct PassReport {
    pub queue: Queue,
    pub name: String,
    /// Indices in Frame::passes, in submission order.
    pub parents: Vec<usize>,
    pub barriers: Vec<String>,
    pub ms: f64,
}
pub struct Backend {
    gpu: Arc<Gpu>,
    heap: Heap,
    resources: Vec<Physical>,
    pipes: HashMap<(Shader, usize), Pipeline>,
    recorders: Vec<Recorder>,
    frame_number: u64,
    first_profile: Option<Arc<FrameProfile>>,
    timelines: queues::Timelines,
    surface: Option<present::Surface>,
}
impl Backend {
    pub fn open() -> Result<Self> {
        let gpu = Gpu::open()?;
        let heap = Heap::new(&gpu, 16)?;
        let timelines = queues::Timelines::new(&gpu)?;
        Ok(Self {
            gpu,
            heap,
            resources: vec![],
            pipes: HashMap::new(),
            recorders: vec![],
            frame_number: 0,
            first_profile: None,
            timelines,
            surface: None,
        })
    }
    fn prepare(&mut self, plan: &Plan) -> Result<()> {
        let g = &self.gpu;
        let graph = &plan.graph;
        ensure!(
            graph
                .extent
                .iter()
                .all(|&d| d <= g.limits.max_image_dimension_2d),
            "viewport exceeds device limits"
        );
        if self.heap.slots.len() < graph.resources.len() {
            self.heap = Heap::new(g, graph.resources.len())?;
        }
        self.resources.truncate(graph.resources.len());
        for (i, (_, s)) in graph.resources.iter().enumerate() {
            let prefer_local = graph.material_atlas == Some(Resource(i));
            let matches = match (self.resources.get(i), s) {
                (Some(Physical::Buffer(b)), Storage::Buffer { bytes, .. }) => {
                    b.size == *bytes as u64 && b.prefer_local == prefer_local
                }
                (Some(Physical::Image(_, size, depth)), Storage::Color | Storage::Depth) => {
                    *size == graph.extent && *depth == matches!(s, Storage::Depth)
                }
                _ => false,
            };
            if !matches {
                let item = match s {
                    Storage::Buffer { bytes, .. } => Physical::Buffer(Buffer::new(
                        g,
                        *bytes as u64,
                        vk::BufferUsageFlags::STORAGE_BUFFER
                            | vk::BufferUsageFlags::TRANSFER_DST
                            | vk::BufferUsageFlags::TRANSFER_SRC
                            | vk::BufferUsageFlags::INDIRECT_BUFFER,
                        prefer_local,
                    )?),
                    _ => {
                        let depth = matches!(s, Storage::Depth);
                        Physical::Image(Image::new(g, graph.extent, depth)?, graph.extent, depth)
                    }
                };
                if i == self.resources.len() {
                    self.resources.push(item)
                } else {
                    self.resources[i] = item;
                }
            }
            if let Physical::Buffer(b) = &mut self.resources[i] {
                let history = graph
                    .histories
                    .get(&Resource(i))
                    .map(|&key| (key, graph.extent));
                if history.is_some() && history != b.history {
                    b.mapped(|p| {
                        p.fill(0);
                        Ok(())
                    })?;
                }
                b.history = history;
                if let Storage::Buffer {
                    initial: Some(data),
                    ..
                } = s
                {
                    let source = Arc::downgrade(data);
                    if !b.uploaded.as_ref().is_some_and(|last| last.ptr_eq(&source)) {
                        b.mapped(|p| {
                            p[..data.len()].copy_from_slice(data);
                            if !graph.append_lists.contains(&Resource(i)) {
                                p[data.len()..].fill(0);
                            }
                            Ok(())
                        })?;
                    }
                    b.uploaded = (!plan.written[i]).then_some(source);
                } else {
                    b.uploaded = None;
                }
                self.heap.write(g, i, b)?;
            }
        }
        for (table, refs) in &graph.addresses {
            let addresses = refs
                .iter()
                .map(|r| self.resources[r.0].buffer().address)
                .collect::<Vec<_>>();
            let Physical::Buffer(table) = &mut self.resources[table.0] else {
                unreachable!()
            };
            table.mapped(|data| {
                let src = bytemuck::cast_slice(&addresses);
                data[..src.len()].copy_from_slice(src);
                Ok(())
            })?;
        }
        for p in &graph.passes {
            if let Some(d) = p.op.program() {
                ensure!(
                    d.bindings.len() * 4 + d.push.len() <= g.heap.max_push_data_size as usize
                        && d.push.len() % 4 == 0,
                    "invalid push data size"
                );
                let key = (d.shader, d.bindings.len());
                if let std::collections::hash_map::Entry::Vacant(e) = self.pipes.entry(key) {
                    e.insert(Pipeline::new(g, d, self.heap.stride)?);
                }
            }
            if let Op::Compute { groups, .. } = p.op {
                ensure!(
                    groups
                        .iter()
                        .zip(g.limits.max_compute_work_group_count)
                        .all(|(&n, limit)| n <= limit),
                    "dispatch exceeds device limits"
                );
            }
        }
        while self.recorders.len() < plan.steps.len() {
            self.recorders.push(Recorder::new(g)?);
        }
        Ok(())
    }
    pub fn execute(&mut self, plan: Plan, tasks: &rayon::ThreadPool) -> Result<Frame> {
        self.execute_to(plan, tasks, None)
    }
    pub(crate) fn execute_to(
        &mut self,
        plan: Plan,
        tasks: &rayon::ThreadPool,
        window: Option<isize>,
    ) -> Result<Frame> {
        let result = self.render_frame(plan, tasks, window);
        if result.is_err() {
            // Failed recording or partial submission may leave an acquired image or
            // unsignaled timeline value. Retire submitted work before reusing its storage.
            unsafe {
                self.gpu.dev.device_wait_idle()?;
            }
            self.surface.take();
            self.timelines = queues::Timelines::new(&self.gpu)?;
            for resource in &mut self.resources {
                if let Physical::Buffer(b) = resource {
                    b.history = None;
                    b.uploaded = None;
                }
            }
        }
        result
    }
    fn render_frame(
        &mut self,
        plan: Plan,
        tasks: &rayon::ThreadPool,
        window: Option<isize>,
    ) -> Result<Frame> {
        let frame_start = Instant::now();
        let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.;
        if let Some(hwnd) = window
            && self.surface.as_ref().is_none_or(|s| s.window != hwnd)
        {
            self.surface = Some(present::Surface::new(&self.gpu, hwnd)?);
        }
        self.prepare(&plan)?;
        let output = plan
            .steps
            .iter()
            .rev()
            .find_map(|s| match plan.graph.passes[s.index].op {
                Op::HostRead(r) | Op::Present(r) => Some(r),
                _ => None,
            })
            .context("frame needs a viewport output")?;
        let pixels = plan.graph.extent[0] as usize * plan.graph.extent[1] as usize * 4;
        ensure!(
            self.resources[output.0].buffer().size >= pixels as u64,
            "output smaller than viewport"
        );
        ensure!(
            plan.graph
                .passes
                .iter()
                .any(|p| matches!(p.op, Op::Present(_)))
                == window.is_some(),
            "viewport target does not match compiled graph"
        );
        let prepare_ms = ms(frame_start);
        let acquire_start = Instant::now();
        let image = if window.is_some() {
            self.surface.as_mut().unwrap().acquire(plan.graph.extent)?
        } else {
            None
        };
        let acquire_ms = ms(acquire_start);
        let recording_start = Instant::now();
        let dirs = &self.resources;
        let heap = self.heap.bind;
        let pipelines = &self.pipes;
        let recorders = &mut self.recorders[..plan.steps.len()];
        let recordings = tasks.install(|| {
            recorders
                .par_iter_mut()
                .zip(&plan.steps)
                .map(|(rec, step)| {
                    let start = Instant::now();
                    unsafe {
                        rec.record(&plan, step, dirs, &heap, pipelines, image)?;
                    }
                    Ok((
                        rayon::current_thread_index().unwrap(),
                        start,
                        Instant::now(),
                    ))
                })
                .collect::<Result<Vec<_>>>()
        })?;
        let record_ms = ms(recording_start);
        let submit_start = Instant::now();
        let d = &self.gpu.dev;
        let (submits, timeline_waits) =
            self.timelines.submit(&self.gpu, &plan, recorders, image)?;
        let submit_ms = ms(submit_start);
        let present_start = Instant::now();
        let presented = image
            .map(|frame| self.surface.as_mut().unwrap().present(frame))
            .transpose();
        let present_ms = ms(present_start);
        let retire_start = Instant::now();
        self.timelines.retire(&self.gpu)?;
        let retire_ms = ms(retire_start);
        presented?;
        let mut rows = vec![0; plan.steps.len()];
        for (row, step) in plan.steps.iter().enumerate() {
            rows[step.index] = row;
        }
        let mut passes = vec![];
        let mut clock_origin = None;
        let mut span = [0i64; 2];
        for (r, step) in recorders.iter().zip(&plan.steps) {
            let mut pair = [0_u64; 2];
            unsafe {
                d.get_query_pool_results(
                    r.query,
                    0,
                    2,
                    bytemuck::cast_slice_mut(&mut pair),
                    8,
                    vk::QueryResultFlags::_64 | vk::QueryResultFlags::WAIT,
                )?;
            }
            if self.gpu.common_clock {
                let origin = *clock_origin.get_or_insert(pair[0]);
                let shift = 64 - self.gpu.timestamp_bits;
                let relative = |v: u64| ((v.wrapping_sub(origin) << shift) as i64) >> shift;
                span[0] = span[0].min(relative(pair[0]));
                span[1] = span[1].max(relative(pair[1]));
            }
            let ticks =
                pair[1].wrapping_sub(pair[0]) & (u64::MAX >> (64 - self.gpu.timestamp_bits));
            passes.push(PassReport {
                queue: plan.graph.passes[step.index].queue,
                name: plan.graph.passes[step.index].name.clone(),
                parents: step.parents.iter().map(|&i| rows[i]).collect(),
                barriers: step
                    .barriers
                    .iter()
                    .map(|b| format!("{} → {:?}", plan.graph.resources[b.resource.0].0, b.to))
                    .collect(),
                ms: ticks as f64 * self.gpu.limits.timestamp_period as f64 * 1e-6,
            });
        }
        let mut counters = std::collections::BTreeMap::new();
        for pass in &plan.graph.passes {
            if let Op::Counters {
                buffer,
                first,
                ref labels,
            } = pass.op
            {
                let Physical::Buffer(b) = &mut self.resources[buffer.0] else {
                    unreachable!()
                };
                b.mapped(|data| {
                    for (&name, word) in labels.iter().zip(data[first * 4..].chunks_exact(4)) {
                        *counters.entry(name).or_insert(0) +=
                            u64::from(u32::from_ne_bytes(word.try_into()?));
                    }
                    Ok(())
                })?;
            }
        }
        let mut memory = FrameMemory::default();
        for allocation in
            std::iter::once(&self.heap._buffer.allocation).chain(self.resources.iter().map(|r| {
                match r {
                    Physical::Buffer(b) => &b.allocation,
                    Physical::Image(i, ..) => &i.allocation,
                }
            }))
        {
            if allocation.device_local {
                memory.device_local_bytes += allocation.allocated_bytes;
            } else {
                memory.nonlocal_bytes += allocation.allocated_bytes;
            }
        }
        if let Some(r) = plan.graph.material_atlas {
            let atlas = self.resources[r.0].buffer();
            memory.atlas_buffer_bytes = atlas.size;
            memory.atlas = Some(atlas.allocation.clone());
        }
        let Physical::Buffer(buffer) = &mut self.resources[output.0] else {
            unreachable!()
        };
        let mut events = vec![];
        let mut workers = std::collections::BTreeSet::new();
        for (worker, start, end) in recordings {
            workers.insert(worker);
            events.extend([(start, 1i32), (end, -1)]);
        }
        events.sort_unstable();
        let mut active = 0;
        let mut peak = 0;
        for (_, delta) in events {
            active += delta;
            peak = peak.max(active);
        }
        let mut barriers = [0; 3];
        for r in recorders {
            for (total, n) in barriers.iter_mut().zip(r.barriers_count) {
                *total += n;
            }
        }
        self.frame_number += u64::from(window.is_none() || image.is_some());
        let profile = FrameProfile {
            main_ms: plan.main_ms,
            build_ms: plan.build_ms,
            compile_ms: plan.compile_ms,
            rhi_prepare_ms: prepare_ms,
            acquire_ms,
            record_ms,
            submit_ms,
            present_ms,
            retire_ms,
            rhi_ms: ms(frame_start),
            gpu_span_ms: self.gpu.common_clock.then(|| {
                (span[1] - span[0]) as f64 * self.gpu.limits.timestamp_period as f64 * 1e-6
            }),
            task_workers: tasks.current_num_threads(),
            recording_workers: workers.len(),
            overlapping_recordings: peak as usize,
            physical_queues: self.gpu.queues.len(),
            dependency_edges: plan.steps.iter().map(|s| s.parents.len()).sum(),
            resource_barriers: plan.steps.iter().map(|s| s.barriers.len()).sum(),
            barrier_calls: barriers[0],
            memory_barriers: barriers[1],
            image_barriers: barriers[2],
            timeline_waits,
            timeline_signals: submits,
            wsi_transitions: 2 * usize::from(image.is_some()),
            wsi_binary_pairs: usize::from(image.is_some()),
        };
        let first_profile = self
            .first_profile
            .get_or_insert_with(|| Arc::new(profile.clone()))
            .clone();
        Ok(Frame {
            first_profile,
            profile,
            sequence: self.frame_number,
            completed: Instant::now(),
            counters,
            submissions: submits,
            presentation: window
                .and_then(|_| self.surface.as_ref().map(|s| format!("{:?}", s.mode))),
            pixels: if window.is_none()
                || plan
                    .graph
                    .passes
                    .iter()
                    .any(|p| matches!(p.op,Op::HostRead(r) if r==output))
            {
                buffer.mapped(|p| Ok(p[..pixels].to_vec()))?
            } else {
                vec![]
            },
            extent: plan.graph.extent,
            passes,
            gpu: self.gpu.name.clone(),
            memory,
        })
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        unsafe {
            let _ = self.gpu.dev.device_wait_idle();
        }
    }
}
