//! RHI-owned queues, independently recorded command pools and timeline-retired frame resources.
#![allow(unsafe_op_in_unsafe_fn)]
mod allocator;
mod clusters;
mod device;
mod frame;
pub use frame::{AllocationInfo, Frame, FrameMemory, FrameProfile, PassReport, RasterCounts};
mod memory;
mod raster_queries;
use memory::Class;
pub use memory::{AllocationActivity, Class as MemoryClass, ManagedMemory, MemoryBreakdown};
mod purposes;
use purposes::Purpose;
pub use purposes::PurposeMemory;
mod ngx;
mod pipeline;
mod present;
mod queues;
mod ray;
mod record;
mod slot;
pub mod timeline;
mod transient;
use crate::rdg::*;
use crate::render::FramePlan;
use anyhow::{Context, Result, ensure};
use device::{Buffer, Gpu, Image};
use pipeline::Pipeline;
use rayon::prelude::*;
use slot::FrameSlot;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Weak},
    time::Instant,
};
use vulkanalia::{prelude::v1_4::*, vk::ExtDescriptorHeapExtensionDeviceCommands};

enum Physical {
    Vacant,
    Buffer(Buffer),
    SharedBuffer(Arc<Buffer>),
    Image(Image, ImageDesc),
    Acceleration(Box<ray::Acceleration>),
    StaticBlas {
        blas: Arc<ray::StaticBlas>,
        scratch: Option<Buffer>,
    },
    Clusters(Box<clusters::Allocation>),
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
            Self::SharedBuffer(b) => b,
            _ => unreachable!(),
        }
    }
}
struct Heap {
    _buffer: Buffer,
    bind: vk::BindHeapInfoEXT,
    stride: u32,
    slots: Vec<Option<HeapBinding>>,
    base: usize,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum HeapBinding {
    Buffer(u64, u64),
    Image(vk::Image),
}
impl Heap {
    fn new(g: &Arc<Gpu>, count: usize, slot: usize) -> Result<Self> {
        let h = g.heap;
        let descriptor_align = h
            .buffer_descriptor_alignment
            .max(h.image_descriptor_alignment);
        let align = h.resource_heap_alignment.max(descriptor_align);
        let stride = h
            .buffer_descriptor_size
            .max(h.image_descriptor_size)
            .next_multiple_of(descriptor_align);
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
            Purpose {
                slot: Some(slot),
                ..Class::Infrastructure.named("Resource descriptor heap")
            },
        )?;
        let address = buffer.address.next_multiple_of(align);
        let base = (address - buffer.address) as usize;
        let bind = vk::BindHeapInfoEXT {
            heap_range: vk::DeviceAddressRangeEXT { address, size },
            reserved_range_offset: if h.min_resource_heap_reserved_range == 0 {
                0
            } else {
                reserved
            },
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
    fn write(&mut self, g: &Gpu, r: usize, buf: &Buffer, bytes: u64) -> Result<()> {
        debug_assert!(bytes <= buf.size);
        let binding = HeapBinding::Buffer(buf.address, bytes);
        if self.slots[r] == Some(binding) {
            return Ok(());
        }
        let range = vk::DeviceAddressRangeEXT {
            address: buf.address,
            size: bytes,
        };
        let desc = vk::ResourceDescriptorInfoEXT {
            type_: vk::DescriptorType::STORAGE_BUFFER,
            data: vk::ResourceDescriptorDataEXT {
                address_range: &range,
            },
            ..Default::default()
        };
        self.write_descriptor(g, r, binding, desc, g.heap.buffer_descriptor_size)
    }

    fn write_image(&mut self, g: &Gpu, r: usize, image: &Image) -> Result<()> {
        let binding = HeapBinding::Image(image.raw);
        if self.slots[r] == Some(binding) {
            return Ok(());
        }
        let view = vk::ImageViewCreateInfo::builder()
            .image(image.raw)
            .view_type(vk::ImageViewType::_2D)
            .format(image.format)
            .subresource_range(image.range)
            .build();
        let image = vk::ImageDescriptorInfoEXT {
            view: &view,
            layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            ..Default::default()
        };
        self.write_descriptor(
            g,
            r,
            binding,
            vk::ResourceDescriptorInfoEXT {
                type_: vk::DescriptorType::SAMPLED_IMAGE,
                data: vk::ResourceDescriptorDataEXT { image: &image },
                ..Default::default()
            },
            g.heap.image_descriptor_size,
        )
    }

    fn write_descriptor(
        &mut self,
        g: &Gpu,
        r: usize,
        binding: HeapBinding,
        desc: vk::ResourceDescriptorInfoEXT,
        size: u64,
    ) -> Result<()> {
        self._buffer.mapped(|data| {
            let offset = self.base + r * self.stride as usize;
            let dst = vk::HostAddressRangeEXT {
                address: data[offset..].as_mut_ptr().cast(),
                size: size as usize,
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
    raster: Option<raster_queries::Pools>,
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
                raster: None,
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

/// Enabled device features available when choosing a graph implementation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RayCapabilities {
    pub pipeline: bool,
    pub inline: bool,
    pub clusters: bool,
}

pub struct Backend {
    gpu: Arc<Gpu>,
    slots: [FrameSlot; 2],
    pending: VecDeque<Submitted>,
    completed: VecDeque<Frame>,
    pipes: HashMap<(Shader, usize), Pipeline>,
    resident: HashMap<(usize, u32), ResidentBuffer>,
    static_blas: ray::StaticCache,
    histories: HashMap<(u64, usize, [u32; 2]), Arc<Buffer>>,
    frame_number: u64,
    first_profile: Option<Arc<FrameProfile>>,
    timelines: queues::Timelines,
    surface: Option<present::Surface>,
}
struct ResidentBuffer {
    source: Weak<[u8]>,
    buffer: Arc<Buffer>,
}
struct Submitted {
    plan: FramePlan,
    slot: usize,
    output: Resource,
    retire: Vec<u64>,
    batches: Vec<Arc<frame::QueueBatch>>,
    start: Instant,
    profile: FrameProfile,
    managed: ManagedMemory,
    recordings: Vec<(usize, Instant, Instant)>,
    presentation: Option<String>,
}
impl Backend {
    pub(crate) fn dlss_unavailable(&self) -> Option<String> {
        self.gpu.dlss_unavailable.clone()
    }
    pub fn ray_capabilities(&self) -> RayCapabilities {
        self.gpu.ray_caps
    }
    pub fn buffer_atomic64(&self) -> bool {
        self.gpu.buffer_atomic64
    }
    pub fn open() -> Result<Self> {
        Self::open_device(false)
    }
    /// Count native fragment invocations and depth-passing samples per draw.
    pub fn open_with_raster_queries() -> Result<Self> {
        Self::open_device(true)
    }
    fn open_device(raster_queries: bool) -> Result<Self> {
        let gpu = Gpu::open(raster_queries)?;
        if let Some(value) = std::env::var_os("RENDER_LAB_GPU_BUDGET_MIB") {
            let mib: u64 = value
                .to_str()
                .context("invalid GPU budget")?
                .parse()
                .context("GPU budget must be an integer in MiB")?;
            gpu.allocator.lock().unwrap().stats.set_budget(Some(
                mib.checked_mul(1 << 20).context("GPU budget overflow")?,
            ))?;
        }
        let slots = [FrameSlot::new(&gpu, 0)?, FrameSlot::new(&gpu, 1)?];
        let timelines = queues::Timelines::new(&gpu)?;
        Ok(Self {
            gpu,
            slots,
            pending: VecDeque::new(),
            completed: VecDeque::new(),
            pipes: HashMap::new(),
            resident: HashMap::new(),
            static_blas: Default::default(),
            histories: HashMap::new(),
            frame_number: 0,
            first_profile: None,
            timelines,
            surface: None,
        })
    }
    /// Limit device-local allocations owned by this backend. Host-visible resources can fall back to a compatible nonlocal heap.
    pub fn set_device_local_budget(&mut self, limit: Option<u64>) -> Result<()> {
        self.gpu.allocator.lock().unwrap().stats.set_budget(limit)
    }

    fn prepare(
        &mut self,
        plan: &FramePlan,
        index: usize,
        reuse: &transient::Reuse,
    ) -> Result<(u64, u64, u64)> {
        let g = &self.gpu;
        let graph = &plan.graph;
        let slot = &mut self.slots[index];
        ensure!(
            graph
                .extent
                .iter()
                .chain(&graph.output_extent())
                .all(|&d| d <= g.limits.max_image_dimension_2d),
            "viewport exceeds device limits"
        );
        let ray_vertices: std::collections::BTreeSet<_> = graph
            .passes
            .iter()
            .flat_map(|p| match p.op {
                Op::BuildBlas {
                    vertices, indices, ..
                } => vec![vertices, indices],
                Op::BuildClusters {
                    records,
                    count,
                    vertices,
                    indices,
                    addresses,
                    ..
                } => vec![records, count, vertices, indices, addresses],
                Op::BuildClusterBottom {
                    record, addresses, ..
                } => vec![record, addresses],
                _ => vec![],
            })
            .collect();
        for pass in &graph.passes {
            match &pass.op {
                Op::TraceRays {
                    extent, program, ..
                } => {
                    ensure!(
                        g.ray_caps.pipeline,
                        "RT pass requires VK_KHR_ray_tracing_pipeline"
                    );
                    g.ray
                        .as_ref()
                        .context("RT experiment requires VK_KHR_ray_tracing_pipeline")?
                        .validate_dispatch(*extent, program.shader, &g.limits)
                        .with_context(|| format!("ray pass {}", pass.name))?;
                }
                Op::RayQuery { .. } => ensure!(
                    g.ray_caps.inline,
                    "compute ray queries require VK_KHR_ray_query"
                ),
                Op::Dlss(_) => ensure!(
                    g.ngx.is_some(),
                    "{}",
                    g.dlss_unavailable.as_deref().unwrap_or("DLSS unavailable")
                ),
                _ => {}
            }
        }
        ensure!(
            ray_vertices.is_empty() || g.ray.is_some(),
            "acceleration structure builds require VK_KHR_acceleration_structure"
        );
        if slot.heap.slots.len() < graph.resources.len() {
            // A descriptor heap reservation stays bound even after timeline retirement.
            // Release every recording, including passes absent from the new graph,
            // before the allocator can reuse the old heap's address for either slot.
            for recorder in &slot.recorders {
                unsafe {
                    g.dev
                        .reset_command_pool(recorder.pool, vk::CommandPoolResetFlags::empty())?;
                }
            }
            slot.heap = Heap::new(g, graph.resources.len(), index)?;
        }
        slot.bind_layout(graph);
        self.resident
            .retain(|_, resource| resource.source.strong_count() != 0);
        self.histories.retain(|(key, _, extent), _| {
            *extent == graph.extent && graph.histories.values().any(|&current| current == *key)
        });
        let mut resident_upload_bytes = 0;
        let mut dynamic_bytes = 0;
        let mut table_bytes = 0;
        let mut pooled = HashMap::<usize, Arc<Buffer>>::new();
        self.static_blas.prune();
        let resident_source = |r: Resource| match &graph.resources[r.0].1 {
            Storage::Buffer { initial, .. }
                if graph.resident_inputs.contains(&r) && !plan.written[r.0] =>
            {
                initial.as_ref()
            }
            _ => None,
        };
        let static_inputs = graph
            .passes
            .iter()
            .filter_map(|p| {
                let Op::BuildBlas {
                    vertices,
                    topology,
                    output,
                    ..
                } = p.op
                else {
                    return None;
                };
                Some((
                    output,
                    [resident_source(vertices)?, resident_source(topology)?],
                ))
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut claimed = std::collections::BTreeSet::new();
        for (i, (name, s)) in graph.resources.iter().enumerate() {
            let resource = Resource(i);
            let tag = Purpose {
                name,
                class: Class::Transient,
                owner: graph.scratch_owners.get(&resource).copied(),
                slot: Some(index),
            };
            let prefer_local = graph.local_buffers.contains(&resource);
            let mut usage = vk::BufferUsageFlags::STORAGE_BUFFER
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::INDIRECT_BUFFER;
            if ray_vertices.contains(&Resource(i)) {
                usage |= vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR;
            }
            let shared = if let Some(cache) = graph.cluster_caches.get(&resource) {
                let Physical::Clusters(owner) = &slot.resources[cache.output.0] else {
                    anyhow::bail!("CLAS version table requires its preceding triangle allocation")
                };
                Some(
                    owner
                        .versions
                        .clone()
                        .context("missing CLAS version table")?,
                )
            } else if let Some(&key) = graph.histories.get(&resource) {
                let Storage::Buffer { bytes, .. } = *s else {
                    unreachable!()
                };
                let key = (key, bytes, graph.extent);
                let entry = match self.histories.entry(key) {
                    std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let mut buffer = Buffer::new(
                            g,
                            bytes as u64,
                            usage,
                            prefer_local,
                            Class::History.named(name),
                        )?;
                        buffer.mapped(|data| {
                            data.fill(0);
                            Ok(())
                        })?;
                        entry.insert(Arc::new(buffer))
                    }
                };
                Some(entry.clone())
            } else if let Some(&leader) = reuse.groups.get(&resource) {
                let Storage::Buffer { bytes, .. } = *s else {
                    unreachable!()
                };
                let buffer = match pooled.entry(leader.0) {
                    std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let old = slot.resources.get(leader.0).and_then(|item| match item {
                            Physical::SharedBuffer(b)
                                if b.class == Class::Transient
                                    && b.size == bytes as u64
                                    && b.usage == usage
                                    && b.prefer_local == prefer_local
                                    && claimed.insert(b.address) =>
                            {
                                Some(b.clone())
                            }
                            _ => None,
                        });
                        let b = match old {
                            Some(b) => b,
                            None => Arc::new(Buffer::new(
                                g,
                                bytes as u64,
                                usage,
                                prefer_local,
                                Purpose {
                                    name: &graph.resources[leader.0].0,
                                    ..tag
                                },
                            )?),
                        };
                        entry.insert(b)
                    }
                };
                Some(buffer.clone())
            } else if graph.resident_inputs.contains(&resource) {
                let Storage::Buffer {
                    bytes,
                    initial: Some(source),
                } = s
                else {
                    unreachable!()
                };
                let key = (source.as_ptr() as usize, usage.bits());
                let entry = match self.resident.entry(key) {
                    std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let mut buffer =
                            Buffer::new(g, *bytes as u64, usage, true, Class::Asset.named(name))?;
                        buffer.mapped(|data| {
                            data[..source.len()].copy_from_slice(source);
                            data[source.len()..].fill(0);
                            Ok(())
                        })?;
                        resident_upload_bytes += source.len() as u64;
                        entry.insert(ResidentBuffer {
                            source: Arc::downgrade(source),
                            buffer: Arc::new(buffer),
                        })
                    }
                };
                Some(entry.buffer.clone())
            } else {
                None
            };
            if let Some(buffer) = shared {
                slot.heap.write(g, i, &buffer, buffer.size)?;
                slot.resources[i] = Physical::SharedBuffer(buffer);
                continue;
            }
            if let Storage::Acceleration(shape) = s
                && let Some(&sources) = static_inputs.get(&resource)
            {
                slot.resources[i] = self.static_blas.bind(g, *shape, sources, tag)?;
                slot.heap.slots[i] = None;
                continue;
            }
            let growable = graph.scratch_owners.contains_key(&resource);
            let matches = match (&slot.resources[i], s) {
                (Physical::Buffer(b), Storage::Buffer { bytes, .. }) => {
                    let fits = if growable {
                        b.size >= *bytes as u64
                    } else {
                        b.size == *bytes as u64
                    };
                    fits && b.prefer_local == prefer_local && b.usage == usage
                }
                (Physical::Clusters(c), Storage::Acceleration(shape)) => c.shape == *shape,
                (Physical::Acceleration(a), Storage::Acceleration(shape)) => a.shape == *shape,
                (Physical::Image(image, desc), _) => {
                    s.image(graph.extent) == Some(*desc)
                        && image.usage == Image::usage_for(desc.format, plan.image_usage[i])
                }
                _ => false,
            };
            if !matches {
                slot.heap.slots[i] = None;
                let item = match s {
                    Storage::Buffer { bytes, .. } if growable => {
                        Physical::Buffer(slot::allocate_scratch(g, *bytes as u64, usage, tag)?)
                    }
                    Storage::Buffer { bytes, .. } => {
                        Physical::Buffer(Buffer::new(g, *bytes as u64, usage, prefer_local, tag)?)
                    }
                    Storage::Color | Storage::Depth | Storage::Image(_) => {
                        let desc = s.image(graph.extent).unwrap();
                        ensure!(
                            desc.extent
                                .iter()
                                .all(|&dim| dim != 0 && dim <= g.limits.max_image_dimension_2d),
                            "image extent exceeds device limits"
                        );
                        Physical::Image(
                            Image::new(
                                g,
                                desc,
                                Image::usage_for(desc.format, plan.image_usage[i]),
                                tag,
                            )?,
                            desc,
                        )
                    }
                    Storage::Acceleration(
                        shape @ (AccelerationShape::TriangleClusters { .. }
                        | AccelerationShape::ClusterBottom { .. }),
                    ) => Physical::Clusters(Box::new(clusters::Allocation::new(g, *shape, tag)?)),
                    Storage::Acceleration(shape) => {
                        Physical::Acceleration(Box::new(ray::Acceleration::new(g, *shape, tag)?))
                    }
                };
                slot.resources[i] = item;
            }
            if let Physical::Buffer(b) = &mut slot.resources[i] {
                if let Some(table) = graph.tables.get(&resource) {
                    ensure!(!plan.written[i], "snapshot table is immutable");
                    if !b.table.as_ref().is_some_and(|t| t.identical(table)) {
                        let old = b.table.take();
                        let n = b.mapped(|p| Ok(table.upload(old.as_ref(), p)))? as u64;
                        table_bytes += n;
                        dynamic_bytes += n;
                        b.table = Some(table.clone());
                    }
                    b.uploaded = None;
                } else if let Storage::Buffer {
                    initial: Some(data),
                    ..
                } = s
                {
                    b.table = None;
                    let source = Arc::downgrade(data);
                    if !b.uploaded.as_ref().is_some_and(|last| last.ptr_eq(&source)) {
                        b.mapped(|p| {
                            dynamic_bytes += data.len() as u64;
                            p[..data.len()].copy_from_slice(data);
                            if !graph.append_lists.contains(&Resource(i)) {
                                p[data.len()..].fill(0);
                            }
                            Ok(())
                        })?;
                    }
                    b.uploaded = (!plan.written[i]).then_some(source);
                } else {
                    b.table = None;
                    b.uploaded = None;
                }
                let Storage::Buffer { bytes, .. } = s else {
                    unreachable!()
                };
                b.use_bytes(*bytes as u64);
                slot.heap.write(g, i, b, *bytes as u64)?;
            }
            if let Physical::Image(image, ..) = &slot.resources[i]
                && image.usage.contains(vk::ImageUsageFlags::SAMPLED)
            {
                slot.heap.write_image(g, i, image)?;
            }
        }
        for cache in graph.cluster_caches.values() {
            let Physical::Clusters(owner) = &mut slot.resources[cache.output.0] else {
                unreachable!()
            };
            let words = owner.cache_parameters(&cache.sources, cache.force_rebuild)?;
            let Physical::Buffer(params) = &mut slot.resources[cache.parameters.0] else {
                unreachable!()
            };
            params.mapped(|data| {
                data[..16].copy_from_slice(bytemuck::bytes_of(&words));
                Ok(())
            })?;
            dynamic_bytes += 16;
        }
        for (table, refs) in &graph.addresses {
            let addresses = refs
                .iter()
                .map(|r| slot.resources[r.0].buffer().address)
                .collect::<Vec<_>>();
            let Physical::Buffer(table) = &mut slot.resources[table.0] else {
                unreachable!()
            };
            table.mapped(|data| {
                let src = bytemuck::cast_slice(&addresses);
                dynamic_bytes += src.len() as u64;
                data[..src.len()].copy_from_slice(src);
                Ok(())
            })?;
        }
        for step in &plan.steps {
            let p = &graph.passes[step.index];
            match p.op {
                Op::BuildBlas {
                    vertices,
                    topology,
                    output,
                    ..
                } => {
                    if let Physical::Acceleration(blas) = &mut slot.resources[output.0] {
                        blas.prepare_blas(resident_source(vertices), resident_source(topology));
                    }
                }
                Op::BuildTlas {
                    ref instances,
                    output,
                } => {
                    let mut rebuild = false;
                    let mut records = Vec::with_capacity(instances.len());
                    for instance in instances {
                        let (address, changed) = match &slot.resources[instance.blas.0] {
                            Physical::Acceleration(blas) => (blas.address, blas.needs_build),
                            Physical::StaticBlas { blas, scratch } => {
                                (blas.acceleration.address, scratch.is_some())
                            }
                            Physical::Clusters(blas) => (blas.address, true),
                            _ => unreachable!(),
                        };
                        let mut record = [0u32; 16];
                        record[..12].copy_from_slice(&instance.transform.map(f32::to_bits));
                        record[12] = 0xff00_0000 | instance.first_triangle;
                        record[14] = address as u32;
                        record[15] = (address >> 32) as u32;
                        records.push(record);
                        rebuild |= changed;
                    }
                    let Physical::Acceleration(tlas) = &mut slot.resources[output.0] else {
                        unreachable!()
                    };
                    tlas.prepare_tlas(records, rebuild)?;
                }
                _ => (),
            }
            if let Some(d) = p.op.program() {
                let ray_bytes = if matches!(p.op, Op::TraceRays { .. } | Op::RayQuery { .. }) {
                    8
                } else {
                    0
                };
                ensure!(
                    d.bindings.len() * 4 + d.push.len() + ray_bytes
                        <= g.heap.max_push_data_size as usize
                        && d.push.len() % 4 == 0,
                    "invalid push data size"
                );
                let key = (d.shader, d.bindings.len());
                if let std::collections::hash_map::Entry::Vacant(e) = self.pipes.entry(key) {
                    e.insert(Pipeline::new(g, d, slot.heap.stride)?);
                }
            }
            if let Op::Compute { groups, .. } | Op::RayQuery { groups, .. } = p.op {
                ensure!(
                    groups
                        .iter()
                        .zip(g.limits.max_compute_work_group_count)
                        .all(|(&n, limit)| n <= limit),
                    "dispatch exceeds device limits"
                );
            }
        }
        while slot.recorders.len() < plan.steps.len() {
            slot.recorders.push(Recorder::new(g)?);
        }
        Ok((resident_upload_bytes, dynamic_bytes, table_bytes))
    }
    pub fn execute(&mut self, plan: FramePlan, tasks: &rayon::ThreadPool) -> Result<Frame> {
        self.execute_to(plan, tasks, None)
    }
    pub(crate) fn execute_to(
        &mut self,
        plan: FramePlan,
        tasks: &rayon::ThreadPool,
        window: Option<isize>,
    ) -> Result<Frame> {
        let result = self
            .submit_frame(plan, tasks, window, 0)
            .and_then(|frame| self.finish_frame(frame));
        if result
            .as_ref()
            .is_err_and(|e| !e.is::<memory::BudgetExceeded>())
        {
            self.recover()?;
        }
        result
    }

    pub(crate) fn enqueue(
        &mut self,
        plan: FramePlan,
        tasks: &rayon::ThreadPool,
        window: isize,
    ) -> Result<Option<Frame>> {
        if self.pending.len() == self.slots.len() {
            let completed = self.retire_submitted().expect("full frame queue")?;
            self.completed.push_back(completed);
        }
        let slot = (0..self.slots.len())
            .find(|idx| self.pending.iter().all(|frame| frame.slot != *idx))
            .unwrap();
        match self.submit_frame(plan, tasks, Some(window), slot) {
            Ok(mut frame) => {
                frame.profile.frames_in_flight = self.pending.len() + 1;
                self.pending.push_back(frame);
                Ok(self.completed.pop_front())
            }
            Err(error) => {
                if !error.is::<memory::BudgetExceeded>() {
                    self.recover()?;
                }
                Err(error)
            }
        }
    }

    pub(crate) fn retire_next(&mut self) -> Option<Result<Frame>> {
        self.completed
            .pop_front()
            .map(Ok)
            .or_else(|| self.retire_submitted())
    }

    fn retire_submitted(&mut self) -> Option<Result<Frame>> {
        self.pending.pop_front().map(|frame| {
            let result = self.finish_frame(frame);
            if result.is_err()
                && let Err(error) = self.recover()
            {
                return Err(error);
            }
            result
        })
    }

    pub(crate) fn has_pending(&self) -> bool {
        !self.completed.is_empty() || !self.pending.is_empty()
    }

    fn recover(&mut self) -> Result<()> {
        unsafe {
            self.gpu.dev.device_wait_idle()?;
        }
        self.pending.clear();
        self.surface.take();
        self.timelines = queues::Timelines::new(&self.gpu)?;
        self.static_blas = Default::default();
        self.histories.clear();
        for slot in &mut self.slots {
            slot.clear();
        }
        Ok(())
    }

    /// Called before acquisition or any GPU submission for the failing frame.
    fn release_idle_resources(&mut self) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if self.pending.iter().all(|pending| pending.slot != index) {
                slot.clear();
            }
        }
        self.resident
            .retain(|_, asset| Arc::strong_count(&asset.buffer) > 1);
    }

    fn prepare_with_budget(
        &mut self,
        plan: &FramePlan,
        index: usize,
        reuse: &transient::Reuse,
    ) -> Result<(u64, u64, u64)> {
        match self.prepare(plan, index, reuse) {
            Err(error) if error.is::<memory::BudgetExceeded>() => {
                self.release_idle_resources();
                let retry = self.prepare(plan, index, reuse);
                if retry.is_err() {
                    self.release_idle_resources();
                }
                retry
            }
            result => result,
        }
    }

    fn submit_frame(
        &mut self,
        mut plan: FramePlan,
        tasks: &rayon::ThreadPool,
        window: Option<isize>,
        index: usize,
    ) -> Result<Submitted> {
        let frame_start = Instant::now();
        let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.;
        if let Some(hwnd) = window
            && self.surface.as_ref().is_none_or(|s| s.window != hwnd)
        {
            self.surface = Some(present::Surface::new(&self.gpu, hwnd)?);
        }
        let reuse = transient::Reuse::plan(&mut plan, self.gpu.queues.len());
        let (resident_upload_bytes, dynamic_upload_bytes, scene_table_upload_bytes) =
            self.prepare_with_budget(&plan, index, &reuse)?;
        let slot = &mut self.slots[index];
        let output = plan.output;
        let pixels =
            plan.graph.output_extent()[0] as usize * plan.graph.output_extent()[1] as usize * 4;
        ensure!(
            slot.resources[output.0].buffer().size >= pixels as u64,
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
            self.surface
                .as_mut()
                .unwrap()
                .acquire(plan.graph.output_extent(), index)?
        } else {
            None
        };
        let acquire_ms = ms(acquire_start);
        let recording_start = Instant::now();
        let dirs = &slot.resources;
        let heap = slot.heap.bind;
        let pipelines = &self.pipes;
        let recorders = &mut slot.recorders[..plan.steps.len()];
        let vendor = plan
            .steps
            .iter()
            .position(|step| matches!(plan.graph.passes[step.index].op, Op::Dlss(_)));
        let mut recordings = tasks.install(|| {
            recorders
                .par_iter_mut()
                .zip(&plan.steps)
                .enumerate()
                .filter(|(index, _)| Some(*index) != vendor)
                .map(|(_, (rec, step))| {
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
        // Keep all NGX calls for a device on the RHI thread.
        if let Some(index) = vendor {
            let start = Instant::now();
            unsafe {
                recorders[index].record(
                    &plan,
                    &plan.steps[index],
                    dirs,
                    &heap,
                    pipelines,
                    image,
                )?;
            }
            recordings.push((tasks.current_num_threads(), start, Instant::now()));
        }
        let record_ms = ms(recording_start);
        let submit_start = Instant::now();
        let batches = self
            .timelines
            .submit(&self.gpu, &plan, recorders, &slot.resources, image)?;
        let mut starts = batches
            .iter()
            .enumerate()
            .filter(|(i, b)| *i == 0 || !Arc::ptr_eq(b, &batches[i - 1]));
        let (submits, timeline_waits) = starts
            .by_ref()
            .fold((0, 0), |(n, w), (_, b)| (n + 1, w + b.waits.len()));
        for (step, batch) in plan.steps.iter().zip(&batches) {
            if let Op::BuildBlas { output, .. } = plan.graph.passes[step.index].op
                && let Physical::StaticBlas {
                    blas,
                    scratch: Some(_),
                } = &slot.resources[output.0]
            {
                let _ = blas.ready.set((batch.queue, batch.signal));
            }
        }
        let submit_ms = ms(submit_start);
        let present_start = Instant::now();
        let presented = image
            .map(|frame| self.surface.as_mut().unwrap().present(frame))
            .transpose();
        let present_ms = ms(present_start);
        presented?;
        let profile = FrameProfile {
            main_ms: plan.main_ms,
            extract_ms: plan.extract_ms,
            build_ms: plan.build_ms,
            compile_ms: plan.compile_ms,
            rhi_prepare_ms: prepare_ms,
            acquire_ms,
            record_ms,
            submit_ms,
            present_ms,
            task_workers: tasks.current_num_threads(),
            timeline_waits,
            timeline_signals: submits,
            wsi_transitions: 2 * usize::from(image.is_some()),
            wsi_binary_pairs: usize::from(image.is_some()),
            frames_in_flight: 1,
            resident_upload_bytes,
            transient_reuse_bytes: reuse.saved_bytes,
            dynamic_upload_bytes,
            scene_table_upload_bytes,
            geometry_instance_visits: plan.instance_visits,
            extracted_entities: plan.extraction.entities_visited,
            updated_instances: plan.extraction.instances_updated,
            acceleration_builds: slot.resources.iter().filter(|resource| {
                matches!(resource, Physical::Clusters(_) | Physical::StaticBlas{scratch:Some(_),..}) || matches!(resource, Physical::Acceleration(acceleration) if acceleration.needs_build && !acceleration.update)
            }).count(),
            acceleration_updates: slot.resources.iter().filter(|r| matches!(r, Physical::Acceleration(a) if a.needs_build && a.update)).count(),
            ..Default::default()
        };
        Ok(Submitted {
            plan,
            slot: index,
            output,
            managed: self.gpu.allocator.lock().unwrap().snapshot(),
            retire: self.timelines.targets(),
            batches,
            start: frame_start,
            profile,
            recordings,
            presentation: window
                .and_then(|_| self.surface.as_ref().map(|s| format!("{:?}", s.mode))),
        })
    }

    fn finish_frame(&mut self, work: Submitted) -> Result<Frame> {
        let Submitted {
            plan,
            slot,
            output,
            retire,
            batches,
            start,
            mut profile,
            managed,
            recordings,
            presentation,
        } = work;
        let retire_start = Instant::now();
        self.timelines.retire(&self.gpu, &retire)?;
        profile.retire_ms = retire_start.elapsed().as_secs_f64() * 1000.;
        let slot = &mut self.slots[slot];
        let recorders = &slot.recorders[..plan.steps.len()];
        let d = &self.gpu.dev;
        let pixels =
            plan.graph.output_extent()[0] as usize * plan.graph.output_extent()[1] as usize * 4;
        let mut rows = vec![0; plan.steps.len()];
        for (row, step) in plan.steps.iter().enumerate() {
            rows[step.index] = row;
        }
        let mut passes = vec![];
        let mut clock_origin = None;
        let mut span = [0i64; 2];
        let period_ms = f64::from(self.gpu.limits.timestamp_period) * 1e-6;
        let aligned = self.gpu.common_clock || self.gpu.queues.len() == 1;
        for ((r, step), batch) in recorders.iter().zip(&plan.steps).zip(batches) {
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
            let interval = if aligned {
                let origin = *clock_origin.get_or_insert(pair[0]);
                let shift = 64 - self.gpu.timestamp_bits;
                let relative = |v: u64| ((v.wrapping_sub(origin) << shift) as i64) >> shift;
                span[0] = span[0].min(relative(pair[0]));
                span[1] = span[1].max(relative(pair[1]));
                Some([
                    relative(pair[0]) as f64 * period_ms,
                    relative(pair[1]) as f64 * period_ms,
                ])
            } else {
                None
            };
            let ticks =
                pair[1].wrapping_sub(pair[0]) & (u64::MAX >> (64 - self.gpu.timestamp_bits));
            passes.push(PassReport {
                batch,
                interval,
                raster: if matches!(plan.graph.passes[step.index].op, Op::Draw { .. }) {
                    r.raster.as_ref().map(|q| q.counts()).transpose()?
                } else {
                    None
                },
                phase: plan.graph.passes[step.index].phase,
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
                let Physical::Buffer(b) = &mut slot.resources[buffer.0] else {
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
        let mut memory = FrameMemory {
            managed,
            ..Default::default()
        };
        let mut account = |allocation: &AllocationInfo| {
            if allocation.device_local {
                memory.device_local_bytes += allocation.allocated_bytes;
            } else {
                memory.nonlocal_bytes += allocation.allocated_bytes;
            }
        };
        account(&slot.heap._buffer.allocation);
        let mut shared = std::collections::BTreeSet::new();
        for r in &slot.resources {
            match r {
                Physical::Vacant => unreachable!("submitted frame contains an unbound resource"),
                Physical::Buffer(b) => account(&b.allocation),
                Physical::SharedBuffer(b) => {
                    if shared.insert(b.address) {
                        account(&b.allocation);
                    }
                }
                Physical::Image(i, ..) => account(&i.allocation),
                Physical::Clusters(a) => a.allocations().for_each(&mut account),
                Physical::StaticBlas { blas, scratch } => {
                    if shared.insert(blas.acceleration.address) {
                        blas.acceleration.allocations().for_each(&mut account);
                    }
                    if let Some(scratch) = scratch {
                        account(&scratch.allocation);
                    }
                }
                Physical::Acceleration(a) => a.allocations().for_each(&mut account),
            }
        }
        for resource in &mut slot.resources {
            if let Physical::StaticBlas { scratch, .. } = resource {
                scratch.take();
            }
        }
        if let Some(r) = plan.material_atlas {
            let atlas = slot.resources[r.0].buffer();
            memory.atlas_buffer_bytes = atlas.size;
            memory.atlas = Some(atlas.allocation.clone());
        }
        let Physical::Buffer(buffer) = &mut slot.resources[output.0] else {
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
        self.frame_number += u64::from(presentation.is_none() || profile.wsi_binary_pairs != 0);
        profile.rhi_ms = start.elapsed().as_secs_f64() * 1000.;
        profile.gpu_span_ms = aligned.then(|| (span[1] - span[0]) as f64 * period_ms);
        for p in &mut passes {
            if let Some(interval) = &mut p.interval {
                for t in interval {
                    *t -= span[0] as f64 * period_ms;
                }
            }
        }
        profile.recording_workers = workers.len();
        profile.overlapping_recordings = peak as usize;
        profile.physical_queues = self.gpu.queues.len();
        profile.dependency_edges = plan.steps.iter().map(|s| s.parents.len()).sum();
        profile.resource_barriers = plan.steps.iter().map(|s| s.barriers.len()).sum();
        profile.barrier_calls = barriers[0];
        profile.memory_barriers = barriers[1];
        profile.image_barriers = barriers[2];
        let submits = profile.timeline_signals;
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
            presentation: presentation.clone(),
            pixels: if presentation.is_none()
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
            extent: plan.graph.output_extent(),
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
