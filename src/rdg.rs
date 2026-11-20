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
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Upload,
    Vertex,
    Mesh,
    Fragment,
    ComputeRead,
    ComputeWrite,
    ComputeReadWrite,
    Indirect,
    ColorClear,
    ColorLoad,
    DepthClear,
    DepthLoad,
    CopySrc,
    CopyDst,
    HostRead,
}
impl Access {
    pub fn writes(self) -> bool {
        matches!(
            self,
            Self::Upload
                | Self::ComputeWrite
                | Self::ComputeReadWrite
                | Self::ColorClear
                | Self::ColorLoad
                | Self::DepthClear
                | Self::DepthLoad
                | Self::CopyDst
        )
    }
    fn reads(self) -> bool {
        !matches!(
            self,
            Self::Upload | Self::ComputeWrite | Self::ColorClear | Self::DepthClear | Self::CopyDst
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shader {
    Graphics(&'static [u8], &'static [u8]),
    Compute(&'static [u8]),
    Mesh(&'static [u8], &'static [u8]),
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
    HostRead(Resource),
    Present(Resource),
    Counters {
        buffer: Resource,
        first: usize,
        labels: Vec<&'static str>,
    },
}
impl Op {
    pub fn program(&self) -> Option<&Dispatch> {
        match self {
            Self::Draw { program, .. }
            | Self::Compute { program, .. }
            | Self::ComputeIndirect { program, .. } => Some(program),
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
pub struct Pass {
    pub queue: Queue,
    pub name: String,
    pub op: Op,
    pub after: Vec<usize>,
}
pub struct Graph {
    pub extent: [u32; 2],
    pub(crate) material_atlas: Option<Resource>,
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
    pub(crate) build_ms: f64,
    pub(crate) compile_ms: f64,
    pub(crate) main_ms: f64,
    pub(crate) graph: Graph,
    pub(crate) steps: Vec<Step>,
    pub(crate) written: Vec<bool>,
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
    pub fn new(extent: [u32; 2]) -> Self {
        Self {
            extent,
            material_atlas: None,
            resources: vec![],
            passes: vec![],
            histories: Default::default(),
            append_lists: Default::default(),
            addresses: Default::default(),
        }
    }
    pub fn resource(&mut self, name: &str, storage: Storage) -> Resource {
        let r = Resource(self.resources.len());
        self.resources.push((name.into(), storage));
        r
    }
    pub fn input(&mut self, name: &str, data: Vec<u8>) -> Resource {
        self.resource(
            name,
            Storage::Buffer {
                bytes: data.len().max(16),
                initial: Some(data.into()),
            },
        )
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
    pub(crate) fn uses(&self, op: &Op) -> Vec<(Resource, Access)> {
        let mut uses = op.uses();
        for (r, a) in op.uses() {
            if let Some(refs) = self.addresses.get(&r) {
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
        let i = self.passes.len();
        let queue = match &op {
            Op::Draw { .. } | Op::Present(..) => Queue::Graphics,
            Op::Compute { .. } | Op::ComputeIndirect { .. } | Op::Counters { .. } => Queue::Compute,
            _ => Queue::Transfer,
        };
        self.passes.push(Pass {
            queue,
            name: name.into(),
            op,
            after: vec![],
        });
        i
    }
    pub fn present(&mut self) -> Result<Resource> {
        let (pass, resource) = self
            .passes
            .iter_mut()
            .rev()
            .find_map(|p| match p.op {
                Op::HostRead(resource) => Some((p, resource)),
                _ => None,
            })
            .context("missing viewport output")?;
        pass.op = Op::Present(resource);
        pass.queue = Queue::Graphics;
        pass.name = "Present swapchain image".into();
        Ok(resource)
    }
    pub fn compile(self) -> Result<Plan> {
        let started = std::time::Instant::now();
        ensure!(self.extent.iter().all(|&v| v > 0), "empty viewport");
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
        for r in self.histories.keys() {
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
        for (i, p) in self.passes.iter().enumerate() {
            if let Some(d) = p.op.program() {
                ensure!(
                    matches!(
                        (&p.op, d.shader),
                        (Op::Draw { .. }, Shader::Graphics(..) | Shader::Mesh(..))
                            | (
                                Op::Compute { .. } | Op::ComputeIndirect { .. },
                                Shader::Compute(_)
                            )
                    ),
                    "shader kind differs from pass"
                );
                ensure!(
                    d.bindings.iter().all(|(_, a)| match p.op {
                        Op::Draw { .. } =>
                            matches!(a, Access::Vertex | Access::Mesh | Access::Fragment),
                        _ => matches!(
                            a,
                            Access::ComputeRead | Access::ComputeWrite | Access::ComputeReadWrite
                        ),
                    }),
                    "invalid shader binding access"
                );
            }
            if let Op::Draw {
                count: DrawCount::Indirect(r),
                ..
            } = p.op
            {
                ensure!(
                    matches!(self.resources.get(r.0),Some((_,Storage::Buffer{bytes,..})) if *bytes>=16),
                    "invalid indirect draw buffer"
                );
            }
            if let Op::ComputeIndirect { args, offset, .. }
            | Op::Draw {
                count: DrawCount::MeshIndirect { args, offset },
                ..
            } = p.op
            {
                ensure!(
                    matches!(self.resources.get(args.0),Some((_,Storage::Buffer{bytes,..})) if offset.is_multiple_of(4) && offset.checked_add(12).is_some_and(|end|end<=*bytes as u64)),
                    "invalid indirect argument range"
                );
            }
            if let Op::Draw {
                ref program, count, ..
            } = p.op
            {
                ensure!(
                    matches!(
                        (program.shader, count),
                        (_, DrawCount::Indirect(_))
                            | (Shader::Mesh(..), DrawCount::MeshIndirect { .. })
                            | (Shader::Mesh(..), DrawCount::Mesh(_))
                            | (Shader::Graphics(..), DrawCount::Vertices(_))
                    ),
                    "draw arguments do not match shader"
                );
            }
            if let Op::Present(r) = p.op {
                ensure!(
                    matches!(self.resources.get(r.0), Some((_, Storage::Buffer { .. }))),
                    "presentation needs a pixel buffer"
                );
            }
            if let Op::Counters {
                buffer,
                first,
                ref labels,
            } = p.op
            {
                ensure!(
                    matches!(self.resources.get(buffer.0),Some((_,Storage::Buffer{bytes,..})) if first.checked_add(labels.len()).is_some_and(|words|words<=bytes/4)),
                    "invalid counter range"
                );
            }
            if let Op::ReadPixels { image, buffer } = p.op {
                ensure!(
                    matches!(
                        self.resources.get(image.0),
                        Some((_, Storage::Color | Storage::Depth))
                    ),
                    "image copy needs an image source"
                );
                let bytes = u64::from(self.extent[0]) * u64::from(self.extent[1]);
                ensure!(
                    matches!(self.resources.get(buffer.0),Some((_,Storage::Buffer{bytes:n,..})) if *n as u64/(if matches!(self.resources.get(image.0),Some((_,Storage::Color))){8}else{4})>=bytes),
                    "undersized readback"
                );
            }
            let mut seen = BTreeSet::new();
            for (r, a) in self.uses(&p.op) {
                let (name, s) = self.resources.get(r.0).context("unknown resource")?;
                ensure!(
                    seen.insert(r),
                    "{}: duplicate access to {name}, declare one read/write use",
                    p.name
                );
                ensure!(
                    match s {
                        Storage::Color =>
                            matches!(a, Access::ColorClear | Access::ColorLoad | Access::CopySrc),
                        Storage::Depth =>
                            matches!(a, Access::DepthClear | Access::DepthLoad | Access::CopySrc),
                        Storage::Buffer { .. } => !matches!(
                            a,
                            Access::ColorLoad
                                | Access::ColorClear
                                | Access::DepthLoad
                                | Access::DepthClear
                        ),
                    },
                    "incompatible access to {name}"
                );
                ensure!(
                    !a.reads() || defined[r.0],
                    "{} reads uninitialized {name}",
                    p.name
                );
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
        while let Some(i) = ready.pop_first() {
            let mut barriers = Vec::new();
            for (r, a) in self.uses(&self.passes[i].op) {
                let prev = &mut history[r.0];
                let image = !matches!(self.resources[r.0].1, Storage::Buffer { .. });
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
        Ok(Plan {
            build_ms: 0.,
            main_ms: 0.,
            compile_ms: started.elapsed().as_secs_f64() * 1000.,
            graph: self,
            steps,
            written: writers.iter().map(Option::is_some).collect(),
        })
    }
}
