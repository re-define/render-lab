mod validate;
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeSet, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Resource(pub usize);
#[derive(Clone, Debug)]
pub enum Storage {
    Buffer {
        bytes: usize,
        initial: Option<Arc<[u8]>>,
    },
    Color,
    Depth,
    Image(ImageDesc),
    Acceleration(AccelerationShape),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccelerationShape {
    Triangles {
        count: u32,
        vertices: u32,
    },
    Instances {
        count: u32,
    },
    TriangleClusters {
        count: u32,
        vertices: u32,
        triangles: u32,
    },
    ClusterBottom {
        count: u32,
    },
}
impl AccelerationShape {
    pub(crate) fn count(self) -> u32 {
        match self {
            Self::TriangleClusters { count, .. }
            | Self::ClusterBottom { count }
            | Self::Triangles { count, .. }
            | Self::Instances { count } => count,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct RayInstance {
    pub blas: Resource,
    pub transform: [f32; 12],
    pub first_triangle: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Visibility,
    Depth,
    Rgba32Float,
    Rg16Float,
}

impl ImageFormat {
    fn bytes_per_pixel(self) -> u64 {
        match self {
            Self::Depth | Self::Rg16Float => 4,
            Self::Visibility => 8,
            Self::Rgba32Float => 16,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageDesc {
    pub extent: [u32; 2],
    pub format: ImageFormat,
}

impl Storage {
    pub(crate) fn image(&self, extent: [u32; 2]) -> Option<ImageDesc> {
        match self {
            Self::Color => Some(ImageDesc {
                extent,
                format: ImageFormat::Visibility,
            }),
            Self::Depth => Some(ImageDesc {
                extent,
                format: ImageFormat::Depth,
            }),
            Self::Image(image) => Some(*image),
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Upload,
    Vertex,
    Mesh,
    MeshFragment,
    Fragment,
    ComputeRead,
    ComputeSample,
    ComputeWrite,
    ComputeReadWrite,
    SdkImageRead,
    SdkImageWrite,
    Indirect,
    ColorClear,
    ColorLoad,
    DepthClear,
    DepthLoad,
    CopySrc,
    CopyDst,
    HostRead,
    AccelerationInput,
    ClusterBuildInput,
    AccelerationRead,
    AccelerationWrite,
    RayAcceleration,
    ComputeAcceleration,
    RayRead,
    RayWrite,
    RayReadWrite,
}
impl Access {
    pub fn writes(self) -> bool {
        matches!(
            self,
            Self::Upload
                | Self::ComputeWrite
                | Self::ComputeReadWrite
                | Self::SdkImageWrite
                | Self::ColorClear
                | Self::ColorLoad
                | Self::DepthClear
                | Self::DepthLoad
                | Self::CopyDst
                | Self::AccelerationWrite
                | Self::RayWrite
                | Self::RayReadWrite
        )
    }
    fn reads(self) -> bool {
        !matches!(
            self,
            Self::Upload
                | Self::ComputeWrite
                | Self::SdkImageWrite
                | Self::ColorClear
                | Self::DepthClear
                | Self::CopyDst
                | Self::AccelerationWrite
                | Self::RayWrite
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shader {
    Graphics(&'static [u8], &'static [u8]),
    Compute(&'static [u8]),
    Mesh(&'static [u8], &'static [u8]),
    ClusterShadows(&'static [u8], &'static [u8], &'static [u8]),
    /// Generation, miss, cluster coverage any-hit, and material closest-hit.
    ClusterRays(&'static [u8], &'static [u8], &'static [u8], &'static [u8]),
    RayTracing(&'static [u8], &'static [u8], &'static [u8]),
}
impl Shader {
    pub(crate) fn supports_clusters(self) -> bool {
        matches!(self, Self::ClusterShadows(..) | Self::ClusterRays(..))
    }
}
/// Binding n maps to the heap slot in word n of push data. User bytes follow the slots.
#[derive(Clone, Debug)]
pub struct Dispatch {
    pub shader: Shader,
    pub bindings: Vec<(Resource, Access)>,
    pub push: Vec<u8>,
}
#[derive(Clone, Copy, Debug)]
pub enum DrawCount {
    Vertices(u32),
    Mesh([u32; 3]),
    Indirect(Resource),
    MeshIndirect { args: Resource, offset: u64 },
}
#[derive(Clone, Debug)]
pub enum Op {
    Draw {
        program: Dispatch,
        color: Resource,
        depth: Resource,
        clear: bool,
        count: DrawCount,
    },
    Compute {
        program: Dispatch,
        groups: [u32; 3],
    },
    ComputeIndirect {
        program: Dispatch,
        args: Resource,
        offset: u64,
    },
    ReadPixels {
        image: Resource,
        buffer: Resource,
    },
    WritePixels {
        buffer: Resource,
        image: Resource,
    },
    Dlss(Dlss),
    BuildClusters {
        records: Resource,
        count: Resource,
        vertices: Resource,
        indices: Resource,
        addresses: Resource,
        output: Resource,
    },
    BuildClusterBottom {
        record: Resource,
        addresses: Resource,
        clusters: Resource,
        output: Resource,
    },
    BuildBlas {
        vertices: Resource,
        indices: Resource,
        topology: Resource,
        output: Resource,
    },
    BuildTlas {
        instances: Vec<RayInstance>,
        output: Resource,
    },
    RayQuery {
        groups: [u32; 3],
        program: Dispatch,
        tlas: Resource,
        blases: Vec<Resource>,
    },
    TraceRays {
        extent: [u32; 2],
        program: Dispatch,
        tlas: Resource,
        blases: Vec<Resource>,
    },
    HostRead(Resource),
    Present(Resource),
    Counters {
        buffer: Resource,
        first: usize,
        labels: Vec<&'static str>,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct Dlss {
    pub color: Resource,
    pub depth: Resource,
    pub motion: Resource,
    pub output: Resource,
    pub jitter: [f32; 2],
    pub hdr: bool,
    pub reset: bool,
}
impl Op {
    pub fn program(&self) -> Option<&Dispatch> {
        match self {
            Self::Draw { program, .. }
            | Self::Compute { program, .. }
            | Self::ComputeIndirect { program, .. }
            | Self::RayQuery { program, .. }
            | Self::TraceRays { program, .. } => Some(program),
            _ => None,
        }
    }
    fn uses(&self) -> Vec<(Resource, Access)> {
        let mut out = self.program().map_or_else(Vec::new, |p| p.bindings.clone());
        match *self {
            Self::Draw {
                color,
                depth,
                clear,
                count,
                ..
            } => {
                out.push((
                    color,
                    if clear {
                        Access::ColorClear
                    } else {
                        Access::ColorLoad
                    },
                ));
                out.push((
                    depth,
                    if clear {
                        Access::DepthClear
                    } else {
                        Access::DepthLoad
                    },
                ));
                if let DrawCount::Indirect(r) | DrawCount::MeshIndirect { args: r, .. } = count {
                    out.push((r, Access::Indirect));
                }
            }
            Self::ComputeIndirect { args, .. } => out.push((args, Access::Indirect)),
            Self::ReadPixels { image, buffer } => {
                out.extend([(image, Access::CopySrc), (buffer, Access::CopyDst)])
            }
            Self::WritePixels { buffer, image } => {
                out.extend([(buffer, Access::CopySrc), (image, Access::CopyDst)])
            }
            Self::Dlss(dlss) => out.extend([
                (dlss.color, Access::SdkImageRead),
                (dlss.depth, Access::SdkImageRead),
                (dlss.motion, Access::SdkImageRead),
                (dlss.output, Access::SdkImageWrite),
            ]),
            Self::BuildClusters {
                records,
                count,
                vertices,
                indices,
                addresses,
                output,
            } => out.extend([
                (records, Access::ClusterBuildInput),
                (count, Access::ClusterBuildInput),
                (vertices, Access::AccelerationInput),
                (indices, Access::AccelerationInput),
                (addresses, Access::ClusterBuildInput),
                (output, Access::AccelerationWrite),
            ]),
            Self::BuildClusterBottom {
                record,
                addresses,
                clusters,
                output,
            } => out.extend([
                (record, Access::ClusterBuildInput),
                (addresses, Access::ClusterBuildInput),
                (clusters, Access::AccelerationRead),
                (output, Access::AccelerationWrite),
            ]),
            Self::BuildBlas {
                vertices,
                indices,
                output,
                ..
            } => out.extend([
                (vertices, Access::AccelerationInput),
                (indices, Access::AccelerationInput),
                (output, Access::AccelerationWrite),
            ]),
            Self::BuildTlas {
                ref instances,
                output,
            } => {
                out.extend(
                    instances
                        .iter()
                        .map(|i| i.blas)
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .map(|r| (r, Access::AccelerationRead)),
                );
                out.push((output, Access::AccelerationWrite));
            }
            Self::TraceRays {
                tlas, ref blases, ..
            } => {
                out.push((tlas, Access::RayAcceleration));
                out.extend(blases.iter().map(|r| (*r, Access::RayAcceleration)));
            }
            Self::RayQuery {
                tlas, ref blases, ..
            } => {
                out.push((tlas, Access::ComputeAcceleration));
                out.extend(blases.iter().map(|r| (*r, Access::ComputeAcceleration)));
            }
            Self::Present(r) => out.push((r, Access::CopySrc)),
            Self::HostRead(r) | Self::Counters { buffer: r, .. } => out.push((r, Access::HostRead)),
            _ => (),
        }
        out
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Queue {
    Graphics,
    Compute,
    Transfer,
}
/// Stable profiling category, independent of queue, command type and display name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PassPhase {
    Culling,
    Pyramids,
    Raster,
    Bins,
    Shade,
    Lighting,
    Denoise,
    Motion,
    Animation,
    Post,
    Overlay,
    RayTracing,
    Transfer,
    Counters,
    Present,
    Compute,
}
impl PassPhase {
    pub fn key(self) -> &'static str {
        match self {
            Self::Culling => "culling",
            Self::Pyramids => "pyramids",
            Self::Raster => "raster",
            Self::Bins => "bins",
            Self::Shade => "shade",
            Self::Lighting => "lighting",
            Self::Denoise => "denoise",
            Self::Motion => "motion",
            Self::Animation => "animation",
            Self::Post => "post",
            Self::Overlay => "overlay",
            Self::RayTracing => "ray_tracing",
            Self::Transfer => "transfer",
            Self::Counters => "counters",
            Self::Present => "present",
            Self::Compute => "compute",
        }
    }
}
pub struct Pass {
    pub phase: PassPhase,
    pub queue: Queue,
    pub name: String,
    pub op: Op,
    pub after: Vec<usize>,
}
pub(crate) mod rows;
use rows::Rows;
pub(crate) struct ClusterCache {
    pub output: Resource,
    pub parameters: Resource,
    pub sources: Vec<Arc<[u8]>>,
    pub force_rebuild: bool,
}
pub struct Graph {
    pub(crate) cluster_caches: std::collections::BTreeMap<Resource, ClusterCache>,
    address_values_only: BTreeSet<Resource>,
    cluster_children: std::collections::BTreeMap<Resource, Resource>,
    pub(crate) tables: std::collections::BTreeMap<Resource, Rows>,
    pub extent: [u32; 2],
    pub(crate) display_extent: Option<[u32; 2]>,
    pub(crate) local_buffers: BTreeSet<Resource>,
    /// A frame-local work buffer keeps its allocation by name and owner across retired frames.
    pub(crate) scratch_owners: std::collections::BTreeMap<Resource, u64>,
    pub(crate) resident_inputs: BTreeSet<Resource>,
    pub resources: Vec<(String, Storage)>,
    pub passes: Vec<Pass>,
    pub(crate) addresses: std::collections::BTreeMap<Resource, Vec<Resource>>,
    pub(crate) append_lists: BTreeSet<Resource>,
    pub(crate) histories: std::collections::BTreeMap<Resource, u64>,
}
#[derive(Debug)]
pub struct Barrier {
    pub resource: Resource,
    pub from: Vec<Access>,
    pub to: Access,
}
pub struct Step {
    pub index: usize,
    pub parents: Vec<usize>,
    pub barriers: Vec<Barrier>,
}
pub struct Plan {
    pub(crate) compile_ms: f64,
    pub(crate) graph: Graph,
    pub(crate) steps: Vec<Step>,
    pub(crate) written: Vec<bool>,
    pub(crate) image_usage: Vec<ImageUsage>,
    /// Expanded descriptor and address-table uses in original pass order.
    pub(crate) pass_uses: Vec<Vec<(Resource, Access)>>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct ImageUsage {
    pub sampled: bool,
    pub copy_source: bool,
    pub copy_destination: bool,
    pub storage: bool,
}
impl Plan {
    pub fn graph(&self) -> &Graph {
        &self.graph
    }
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }
}
impl Graph {
    pub fn output_extent(&self) -> [u32; 2] {
        self.display_extent.unwrap_or(self.extent)
    }

    pub(crate) fn image_desc(&self, resource: Resource) -> Option<ImageDesc> {
        self.resources.get(resource.0)?.1.image(self.extent)
    }
    pub fn new(extent: [u32; 2]) -> Self {
        Self {
            extent,
            address_values_only: BTreeSet::new(),
            cluster_children: Default::default(),
            cluster_caches: Default::default(),
            tables: Default::default(),
            display_extent: None,
            local_buffers: Default::default(),
            scratch_owners: Default::default(),
            resident_inputs: Default::default(),
            resources: vec![],
            passes: vec![],
            histories: Default::default(),
            append_lists: Default::default(),
            addresses: Default::default(),
        }
    }
    pub(crate) fn host_readable_buffer(&self, resource: Resource) -> bool {
        matches!(
            self.resources.get(resource.0),
            Some((_, Storage::Buffer { .. }))
        ) && !self.histories.contains_key(&resource)
            && !self.resident_inputs.contains(&resource)
            && !self.cluster_caches.contains_key(&resource)
            && !self.scratch_owners.contains_key(&resource)
    }

    pub(crate) fn table(&mut self, name: &str, rows: &Rows) -> Resource {
        let r = self.resource(
            name,
            Storage::Buffer {
                bytes: rows.bytes().max(16),
                initial: Some(Arc::from([])),
            },
        );
        self.tables.insert(r, rows.clone());
        self.prefer_local_buffer(r);
        r
    }
    pub fn resource(&mut self, name: &str, storage: Storage) -> Resource {
        let r = Resource(self.resources.len());
        self.resources.push((name.into(), storage));
        r
    }
    pub fn input(&mut self, name: &str, data: Vec<u8>) -> Resource {
        self.frame_input(name, data.into())
    }
    pub(crate) fn frame_input(&mut self, name: &str, data: Arc<[u8]>) -> Resource {
        self.resource(
            name,
            Storage::Buffer {
                bytes: data.len().max(16),
                initial: Some(data),
            },
        )
    }
    /// Only the declared byte range is visible to shaders. The contents start undefined each frame.
    pub(crate) fn scratch_buffer(&mut self, name: &str, owner: u64, bytes: usize) -> Resource {
        let r = self.resource(
            name,
            Storage::Buffer {
                bytes: bytes.max(16),
                initial: None,
            },
        );
        self.scratch_owners.insert(r, owner);
        self.prefer_local_buffer(r);
        r
    }
    /// Prefer mapped device-local memory for buffers that receive many GPU accesses.
    pub(crate) fn prefer_local_buffer(&mut self, buffer: Resource) {
        self.local_buffers.insert(buffer);
    }
    pub(crate) fn resident_input(&mut self, name: &str, data: Arc<[u8]>) -> Resource {
        let resource = self.resource(
            name,
            Storage::Buffer {
                bytes: data.len().max(16),
                initial: Some(data),
            },
        );
        self.resident_inputs.insert(resource);
        resource
    }
    /// The first four words are reset each frame. A consumer may only read
    /// entries emitted by the producer up to the count in that header.
    pub fn append_list(&mut self, name: &str, payload_bytes: usize) -> Resource {
        let r = self.resource(
            name,
            Storage::Buffer {
                bytes: 16 + payload_bytes,
                initial: Some(vec![0; 16].into()),
            },
        );
        self.append_lists.insert(r);
        r
    }
    pub fn address_table(&mut self, refs: Vec<Resource>) -> Resource {
        let r = self.input("Geometry addresses", vec![0; 8 * refs.len().max(1)]);
        self.addresses.insert(r, refs);
        r
    }
    /// This pass copies address values into native indirect records. Consumers declare the eventual accesses.
    pub(crate) fn build_addresses(&mut self, refs: Vec<Resource>) -> Resource {
        let table = self.address_table(refs);
        self.address_values_only.insert(table);
        table
    }
    fn uses(&self, op: &Op) -> Vec<(Resource, Access)> {
        let mut uses = op.uses();
        for index in 0..uses.len() {
            let (r, a) = uses[index];
            if a == Access::RayAcceleration
                && let Some(&child) = self.cluster_children.get(&r)
                && !uses.contains(&(child, a))
            {
                uses.push((child, a));
            }
            if !self.address_values_only.contains(&r)
                && let Some(refs) = self.addresses.get(&r)
            {
                for &src in refs {
                    if !uses.contains(&(src, a)) {
                        uses.push((src, a));
                    }
                }
            }
        }
        uses
    }
    pub fn history_buffer(&mut self, name: &str, key: u64, bytes: usize) -> Resource {
        let r = self.resource(
            name,
            Storage::Buffer {
                bytes,
                initial: None,
            },
        );
        self.histories.insert(r, key);
        r
    }
    pub fn pass(&mut self, name: &str, op: Op) -> usize {
        let phase = match &op {
            Op::Draw { .. } => PassPhase::Raster,
            Op::Compute { .. } | Op::ComputeIndirect { .. } => PassPhase::Compute,
            Op::BuildClusters { .. }
            | Op::BuildClusterBottom { .. }
            | Op::BuildBlas { .. }
            | Op::BuildTlas { .. }
            | Op::RayQuery { .. }
            | Op::TraceRays { .. } => PassPhase::RayTracing,
            Op::Counters { .. } => PassPhase::Counters,
            Op::Present(_) => PassPhase::Present,
            Op::Dlss(_) => PassPhase::Post,
            Op::HostRead(_) | Op::WritePixels { .. } | Op::ReadPixels { .. } => PassPhase::Transfer,
        };
        self.pass_in(phase, name, op)
    }
    pub fn pass_in(&mut self, phase: PassPhase, name: &str, op: Op) -> usize {
        let i = self.passes.len();
        if let Op::BuildClusterBottom {
            output, clusters, ..
        } = op
        {
            self.cluster_children.insert(output, clusters);
        }
        let queue = match &op {
            Op::Draw { .. } | Op::Present(..) | Op::Dlss(_) => Queue::Graphics,
            Op::Compute { .. }
            | Op::ComputeIndirect { .. }
            | Op::Counters { .. }
            | Op::BuildClusters { .. }
            | Op::BuildClusterBottom { .. }
            | Op::BuildBlas { .. }
            | Op::BuildTlas { .. }
            | Op::RayQuery { .. }
            | Op::TraceRays { .. } => Queue::Compute,
            _ => Queue::Transfer,
        };
        self.passes.push(Pass {
            phase,
            queue,
            name: name.into(),
            op,
            after: vec![],
        });
        i
    }
    pub fn compile(self) -> Result<Plan> {
        let started = std::time::Instant::now();
        ensure!(
            self.extent
                .iter()
                .chain(&self.output_extent())
                .all(|&v| v > 0),
            "empty viewport"
        );
        ensure!(
            self.histories
                .values()
                .copied()
                .collect::<BTreeSet<_>>()
                .len()
                == self.histories.len(),
            "a history key may be declared only once per graph"
        );
        ensure!(
            self.local_buffers
                .iter()
                .all(|r| { matches!(self.resources.get(r.0), Some((_, Storage::Buffer { .. }))) }),
            "only buffers can request device-local placement"
        );
        let mut deps = vec![BTreeSet::new(); self.passes.len()];
        let mut writers = vec![None; self.resources.len()];
        let mut readers = vec![BTreeSet::new(); self.resources.len()];
        let mut defined = self
            .resources
            .iter()
            .map(|(_, s)| {
                matches!(
                    s,
                    Storage::Buffer {
                        initial: Some(_),
                        ..
                    }
                )
            })
            .collect::<Vec<_>>();
        for r in self.histories.keys().chain(self.cluster_caches.keys()) {
            defined[r.0] = true;
        }
        for refs in self.addresses.values() {
            for r in refs {
                ensure!(
                    matches!(self.resources.get(r.0), Some((_, Storage::Buffer { .. }))),
                    "invalid geometry address"
                );
            }
        }
        for (_, s) in &self.resources {
            if let Storage::Buffer { bytes, initial } = s {
                ensure!(
                    *bytes > 0 && initial.as_ref().is_none_or(|v| v.len() <= *bytes),
                    "invalid buffer size"
                );
            }
        }
        let pass_uses: Vec<_> = self.passes.iter().map(|pass| self.uses(&pass.op)).collect();
        let mut tlas_sources = std::collections::BTreeMap::new();
        let mut last_image = vec![None; self.resources.len()];
        let mut image_usage = vec![ImageUsage::default(); self.resources.len()];
        for (i, p) in self.passes.iter().enumerate() {
            self.validate_pass(p, &defined, &mut tlas_sources)?;
            let mut seen = BTreeSet::new();
            for &(r, a) in &pass_uses[i] {
                let (name, s) = self.resources.get(r.0).context("unknown resource")?;
                ensure!(
                    seen.insert(r),
                    "{}: duplicate access to {name}, declare one read/write use",
                    p.name
                );
                ensure!(s.permits(a), "incompatible access to {name}");
                ensure!(
                    !a.writes() || !self.tables.contains_key(&r),
                    "write to read-only scene table {name}"
                );
                ensure!(
                    !a.reads() || defined[r.0],
                    "{} reads uninitialized {name}",
                    p.name
                );
                if s.image(self.extent).is_some() {
                    image_usage[r.0].sampled |=
                        matches!(a, Access::ComputeSample | Access::SdkImageRead);
                    image_usage[r.0].copy_source |= a == Access::CopySrc;
                    image_usage[r.0].copy_destination |=
                        matches!(a, Access::CopyDst | Access::SdkImageWrite);
                    image_usage[r.0].storage |= a == Access::SdkImageWrite;
                    if last_image[r.0]
                        .replace(a)
                        .is_some_and(|previous| previous != a)
                    {
                        deps[i].extend(readers[r.0].iter().copied());
                    }
                }
                if (!a.writes() || readers[r.0].is_empty())
                    && let Some(w) = writers[r.0]
                {
                    deps[i].insert(w);
                }
                if a.writes() {
                    deps[i].append(&mut readers[r.0]);
                    writers[r.0] = Some(i);
                    defined[r.0] = true;
                } else {
                    readers[r.0].insert(i);
                }
            }
            for &j in &p.after {
                ensure!(j < self.passes.len(), "unknown pass");
                deps[i].insert(j);
            }
        }
        let mut history = self
            .resources
            .iter()
            .map(|(_, s)| {
                if matches!(
                    s,
                    Storage::Buffer {
                        initial: Some(_),
                        ..
                    }
                ) {
                    vec![Access::Upload]
                } else {
                    vec![]
                }
            })
            .collect::<Vec<_>>();
        for r in self.histories.keys() {
            history[r.0] = vec![Access::Upload, Access::CopyDst, Access::ComputeWrite];
        }
        // Each version table belongs to its retired frame-slot CLAS allocation.
        for (versions, cache) in &self.cluster_caches {
            history[versions.0] = vec![Access::Upload, Access::ComputeWrite];
            history[cache.output.0] = vec![Access::AccelerationWrite, Access::RayAcceleration];
        }
        let mut dependents = vec![Vec::new(); deps.len()];
        let mut remaining = Vec::with_capacity(deps.len());
        let mut ready = BTreeSet::new();
        for (id, parents) in deps.iter().enumerate() {
            remaining.push(parents.len());
            if parents.is_empty() {
                ready.insert(id);
            }
            for &parent in parents {
                dependents[parent].push(id);
            }
        }
        let mut steps = Vec::with_capacity(deps.len());
        let mut previous_queue = None;
        while !ready.is_empty() {
            let i = ready
                .iter()
                .copied()
                .find(|&i| Some(self.passes[i].queue) == previous_queue)
                .unwrap_or_else(|| *ready.first().unwrap());
            ready.remove(&i);
            previous_queue = Some(self.passes[i].queue);
            let mut barriers = Vec::new();
            for &(r, a) in &pass_uses[i] {
                let prev = &mut history[r.0];
                let image = self.image_desc(r).is_some();
                // Preserve the writer for fan-out to readers at different stages.
                let hazard = !prev.is_empty()
                    && (a.writes() || !prev.contains(&a) && prev.iter().any(|p| p.writes()));
                if hazard || image && prev.last() != Some(&a) {
                    barriers.push(Barrier {
                        resource: r,
                        from: prev.clone(),
                        to: a,
                    });
                }
                if a.writes() || image {
                    prev.clear();
                }
                if !prev.contains(&a) {
                    prev.push(a);
                }
            }
            for &child in &dependents[i] {
                remaining[child] -= 1;
                if remaining[child] == 0 {
                    ready.insert(child);
                }
            }
            steps.push(Step {
                index: i,
                parents: deps[i].iter().copied().collect(),
                barriers,
            });
        }
        ensure!(steps.len() == self.passes.len(), "cyclic pass dependencies");
        ensure!(
            self.resident_inputs.iter().all(|r| writers[r.0].is_none()),
            "resident input may not be written by a pass"
        );
        Ok(Plan {
            compile_ms: started.elapsed().as_secs_f64() * 1000.,
            graph: self,
            steps,
            written: writers.iter().map(Option::is_some).collect(),
            image_usage,
            pass_uses,
        })
    }
}
