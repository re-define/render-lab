use super::*;
use crate::render::{Geometry, SurfaceInstance, Targets, build_frame};
use Access::{ComputeRead as R, ComputeReadWrite as RW, ComputeWrite as W, Mesh as M};
use bytemuck::{Pod, Zeroable};
use std::collections::BTreeMap;
macro_rules! spirv {
    ($s:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/", $s, ".spv"))
    };
}
const LABELS: &[&str] = &[
    "Frustum instances",
    "BVH visits",
    "Cluster tests",
    "Main clusters",
    "Deferred clusters",
    "Recovered clusters",
    "L0",
    "L1",
    "L2",
    "L3",
    "L4",
    "L5",
    "L6",
    "L7",
    "L8",
    "L9+",
    "Potential clusters",
    "Peak BVH frontier",
];
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct View {
    vp: [f32; 16],
    eye: [f32; 4],
    screen: [u32; 4],
    settings: [f32; 4],
}
#[derive(Clone, Copy)]
struct Work {
    items: Resource,
    args: Resource,
}
// Keep each mesh indirect command within the legacy batch size. All pages
// index the same scene-wide visible list, including the final partial page.
const DRAW_PAGE_SIZE: u32 = 1 << 20;
fn arguments(g: &mut Graph, name: &str, commands: u32) -> Resource {
    g.input(
        name,
        bytemuck::cast_slice(&vec![[0u32, 1, 1, 0]; commands as usize]).to_vec(),
    )
}
impl Work {
    fn new(g: &mut Graph, name: &str, capacity: u32, commands: u32) -> Self {
        Self {
            items: g.append_list(name, 8 * capacity as usize),
            args: arguments(g, &format!("{name} indirect arguments"), commands),
        }
    }
}
fn compute(shader: &'static [u8], bindings: &[(Resource, Access)], push: Vec<u8>) -> Dispatch {
    Dispatch {
        shader: Shader::Compute(shader),
        bindings: bindings.to_vec(),
        push,
    }
}
struct ClusterView {
    uniform: Resource,
    visualize_lod: bool,
}
struct ClusterScene {
    assets: Resource,
    instances: Resource,
    view: Resource,
    stats: Resource,
    instance_count: u32,
    capacity: u32,
    level_widths: Vec<u32>,
}
impl ClusterScene {
    fn upload(
        g: &mut Graph,
        t: &mut Targets,
        s: &Snapshot,
        view: ClusterView,
        transforms: &BTreeMap<Id, Mat4>,
        objects: BTreeMap<Id, Vec<(Id, &Instance)>>,
        cache: &mut BTreeMap<Id, Arc<bvh::Packed>>,
    ) -> Result<Self> {
        let mut addresses = Vec::new();
        let mut instances = Vec::<SurfaceInstance>::new();
        let mut level_widths = Vec::<u32>::new();
        let mut capacity = 0u32;
        cache.retain(|id, _| objects.contains_key(id));
        for (asset_index, (id, objects)) in objects.into_iter().enumerate() {
            let e = &s.world.entities[&id];
            if !cache.get(&id).is_some_and(|p| p.matches(e)) {
                cache.insert(id, Arc::new(bvh::Packed::build(e)?));
            }
            let packed = &cache[&id];
            let mut source = |name: &str, data: &Arc<[u8]>| {
                g.resource(
                    name,
                    Storage::Buffer {
                        bytes: data.len(),
                        initial: Some(data.clone()),
                    },
                )
            };
            let tree = source("Spatial BVH", &packed.tree);
            let groups = source("LOD groups", &packed.groups);
            let clusters = source("Meshlets", &packed.clusters);
            let geometry = Geometry {
                vertices: source("Resident vertices", &packed.vertices),
                indices: source("Meshlet index stream", &packed.indices),
                clusters: Some(clusters),
            };
            addresses.extend([tree, groups, clusters, geometry.vertices, geometry.indices]);
            let n: u32 = objects.len().try_into()?;
            capacity = packed
                .count
                .checked_mul(n)
                .and_then(|v| capacity.checked_add(v))
                .ok_or_else(|| anyhow::anyhow!("global cluster list exceeds 32-bit indexing"))?;
            // Every node on a level has at least one distinct descendant cluster.
            if level_widths.len() < packed.level_widths.len() {
                level_widths.resize(packed.level_widths.len(), 0);
            }
            for (width, asset_width) in level_widths.iter_mut().zip(&packed.level_widths) {
                *width += n * asset_width;
            }
            for (id, i) in objects {
                let model = transforms[&id];
                let mut obj = t.material(s, i, model)?;
                obj.params[2] =
                    cl::cut::max_scale(glam::Mat3::from_mat4(model).transpose().to_cols_array_2d());
                obj.tex[3] |= u32::from(view.visualize_lod) << 5;
                obj.routing[2] = asset_index.try_into()?;
                obj.routing[1] = t.register(obj, geometry)?;
                instances.push(obj);
            }
        }
        let mut counts = [0u32; 18];
        counts[16] = capacity;
        Ok(Self {
            assets: g.address_table(addresses),
            instance_count: instances.len().try_into()?,
            instances: g.input(
                "Cluster instances",
                bytemuck::cast_slice(&instances).to_vec(),
            ),
            stats: g.input("Cluster counters", bytemuck::bytes_of(&counts).to_vec()),
            capacity,
            level_widths,
            view: view.uniform,
        })
    }

    fn cull(&self, g: &mut Graph) -> Result<Work> {
        // Two disjoint ranges in one global node list alternate as source and
        // destination. Each depth has its own zeroed count and dispatch record.
        let mut widths = [0u32; 2];
        for (level, &width) in self.level_widths.iter().enumerate() {
            widths[level % 2] = widths[level % 2].max(width);
        }
        let slots = widths[0]
            .checked_add(widths[1])
            .ok_or_else(|| anyhow::anyhow!("global BVH node list exceeds 32-bit indexing"))?;
        let nodes = g.resource(
            "Global BVH node list",
            Storage::Buffer {
                bytes: 8 * slots as usize,
                initial: None,
            },
        );
        let levels = self.level_widths.len();
        let counts = g.input("BVH frontier counts", vec![0; 4 * levels]);
        let dispatches = (0..levels)
            .map(|level| arguments(g, &format!("BVH level {level} dispatch"), 1))
            .collect::<Vec<_>>();
        let root_groups = self.instance_count.div_ceil(64);
        g.pass(
            "GPU instance frustum",
            Op::Compute {
                program: compute(
                    spirv!("instances.comp"),
                    &[
                        (self.assets, R),
                        (self.instances, R),
                        (self.view, R),
                        (nodes, W),
                        (dispatches[0], RW),
                        (self.stats, RW),
                        (counts, RW),
                    ],
                    self.instance_count.to_ne_bytes().to_vec(),
                ),
                groups: [root_groups.min(32768), root_groups.div_ceil(32768), 1],
            },
        );
        let clusters = Work::new(g, "Global cluster candidates", self.capacity, 1);
        let bases = [0, widths[0]];
        for (level, &args) in dispatches.iter().enumerate() {
            let next = dispatches.get(level + 1);
            let mut bindings = vec![
                (self.assets, R),
                (self.instances, R),
                (self.view, R),
                (nodes, if next.is_some() { RW } else { R }),
                (clusters.items, RW),
                (clusters.args, RW),
                (self.stats, RW),
                (counts, if next.is_some() { RW } else { R }),
            ];
            let shader: &'static [u8] = if let Some(&next) = next {
                bindings.push((next, RW));
                spirv!("bvh.comp")
            } else {
                spirv!("bvh_last.comp")
            };
            let push = [level as u32, bases[level % 2], bases[(level + 1) % 2]];
            g.pass(
                &format!("GPU BVH level {level}"),
                Op::ComputeIndirect {
                    program: compute(shader, &bindings, bytemuck::bytes_of(&push).to_vec()),
                    args,
                    offset: 0,
                },
            );
        }
        let selected = Work::new(g, "LOD selected clusters", self.capacity, 1);
        g.pass(
            "GPU cluster cull",
            Op::ComputeIndirect {
                program: compute(
                    spirv!("cluster_cull.comp"),
                    &[
                        (self.assets, R),
                        (self.instances, R),
                        (self.view, R),
                        (clusters.items, R),
                        (selected.items, RW),
                        (selected.args, RW),
                        (self.stats, RW),
                    ],
                    vec![],
                ),
                args: clusters.args,
                offset: 0,
            },
        );
        Ok(selected)
    }

    fn visibility(
        &self,
        g: &mut Graph,
        history: Resource,
        input: Work,
        out: Work,
        deferred: Option<Work>,
    ) {
        let mut bindings = vec![
            (self.assets, R),
            (self.instances, R),
            (input.items, R),
            (history, R),
            (self.view, R),
            (out.items, RW),
            (out.args, RW),
            (self.stats, RW),
        ];
        let (name, shader): (_, &'static [u8]) = if let Some(defer) = deferred {
            bindings.extend([(defer.items, RW), (defer.args, RW)]);
            ("Main Hi-Z cull", spirv!("occlusion.comp"))
        } else {
            ("Post Hi-Z retest", spirv!("occlusion_post.comp"))
        };
        g.pass(
            name,
            Op::ComputeIndirect {
                program: compute(shader, &bindings, vec![]),
                args: input.args,
                offset: 0,
            },
        );
    }

    fn draw(&self, g: &mut Graph, t: &mut Targets, out: Work, name: &str) {
        for page in 0..self.capacity.div_ceil(DRAW_PAGE_SIZE) {
            let program = Dispatch {
                shader: Shader::Mesh(spirv!("cluster.mesh"), spirv!("visibility.frag")),
                bindings: vec![
                    (self.assets, M),
                    (self.instances, M),
                    (self.view, M),
                    (out.items, M),
                ],
                push: (page * DRAW_PAGE_SIZE).to_ne_bytes().to_vec(),
            };
            g.pass(
                name,
                Op::Draw {
                    program,
                    color: t.visibility,
                    depth: t.depth,
                    clear: t.clear,
                    count: DrawCount::MeshIndirect {
                        args: out.args,
                        offset: 16 * u64::from(page),
                    },
                },
            );
            t.clear = false;
        }
    }
}
struct Pyramid {
    levels: Vec<[u32; 4]>,
    header: u32,
}
impl Pyramid {
    fn new([mut w, mut h]: [u32; 2]) -> Result<Self> {
        let mut src = 0u32;
        let mut levels = vec![];
        loop {
            let dst = src
                .checked_add(
                    w.checked_mul(h)
                        .ok_or_else(|| anyhow::anyhow!("Hi-Z size overflow"))?,
                )
                .ok_or_else(|| anyhow::anyhow!("Hi-Z size overflow"))?;
            levels.push([w, h, src, dst]);
            w = w.div_ceil(2);
            h = h.div_ceil(2);
            src = dst;
            if w == 1 && h == 1 {
                break;
            }
        }
        Ok(Self {
            levels,
            header: src + 1,
        })
    }
    fn build(&self, g: &mut Graph, t: &Targets, h: Resource, view: Resource, name: &str) {
        g.pass(
            &format!("{name} depth"),
            Op::ReadPixels {
                image: t.depth,
                buffer: h,
            },
        );
        for (i, &params) in self.levels.iter().enumerate() {
            let program = compute(
                spirv!("hiz.comp"),
                &[(h, RW), (view, R)],
                bytemuck::bytes_of(&params).to_vec(),
            );
            g.pass(
                &format!("{name} mip {}", i + 1),
                Op::Compute {
                    program,
                    groups: [params[0].div_ceil(16), params[1].div_ceil(16), 1],
                },
            );
        }
    }
}
impl Technique for ClusterLod {
    fn build(&self, s: &Snapshot) -> Result<Graph> {
        build_frame(s, |g, t, transforms| {
            let settings = s
                .world
                .entities
                .values()
                .find(|e| e.components.contains_key(SETTING));
            let pixel_error = settings
                .and_then(|e| e.components[SETTING].as_f64())
                .unwrap_or(1.) as f32;
            ensure!(
                pixel_error.is_finite() && pixel_error >= 0.,
                "invalid pixel error"
            );
            let debug = settings
                .and_then(|e| e.components.get(VISUALIZE))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let eligible = |e: &Entity, i: &Instance| {
                let m = &s.world.entities[&i.mesh];
                e.skin_binding.is_none()
                    && m.hierarchy.is_some()
                    && m.meshlets.is_some()
                    && !(i.outline && m.topology.is_some())
            };
            let mut groups = BTreeMap::<Id, Vec<(Id, &Instance)>>::new();
            for (&id, e) in &s.world.entities {
                if let Some(i) = &e.instance
                    && eligible(e, i)
                {
                    groups.entry(i.mesh).or_default().push((id, i));
                }
            }
            if groups.is_empty() {
                return Forward::default().draw_subset(g, t, s, transforms, |_, _| true);
            }
            let pyramid = Pyramid::new(s.extent)?;
            let history = g.history_buffer(
                "Temporal Hi-Z and camera",
                self.history,
                (pyramid.header as usize + 17) * 4,
            );
            let camera = View {
                vp: s.camera.view_projection(s.extent).to_cols_array(),
                eye: s
                    .camera
                    .eye()
                    .extend(s.extent[1] as f32 * 0.5 / 25f32.to_radians().tan())
                    .into(),
                screen: [s.extent[0], s.extent[1], pyramid.header, DRAW_PAGE_SIZE],
                settings: [pixel_error, (s.camera.distance * 0.001).max(0.0001), 0., 0.],
            };
            let view = g.input("Cluster camera", bytemuck::bytes_of(&camera).to_vec());
            let scene = ClusterScene::upload(
                g,
                t,
                s,
                ClusterView {
                    uniform: view,
                    visualize_lod: debug,
                },
                transforms,
                groups,
                &mut self.cache.lock().unwrap(),
            )?;
            let selected = scene.cull(g)?;
            let pages = scene.capacity.div_ceil(DRAW_PAGE_SIZE);
            let main = Work::new(g, "Main visible clusters", scene.capacity, pages);
            let deferred = Work::new(g, "Deferred clusters", scene.capacity, 1);
            let post = Work::new(g, "Recovered clusters", scene.capacity, pages);
            scene.visibility(g, history, selected, main, Some(deferred));
            scene.draw(g, t, main, "Main mesh visibility");
            Forward::default().draw_subset(g, t, s, transforms, |e, i| !eligible(e, i))?;
            pyramid.build(g, t, history, view, "Current Hi-Z");
            scene.visibility(g, history, deferred, post, None);
            scene.draw(g, t, post, "Post mesh visibility");
            g.pass(
                "Cluster statistics",
                Op::Counters {
                    buffer: scene.stats,
                    first: 0,
                    labels: LABELS.to_vec(),
                },
            );
            pyramid.build(g, t, history, view, "History Hi-Z");
            Ok(())
        })
    }
}
