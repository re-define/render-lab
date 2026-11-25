use super::*;
use crate::experiments::vegetation;
use crate::rdg::PassPhase;
use crate::render::{Atlas, Geometry, SurfaceInstance, Targets, build_frame_with_post};
use Access::{ComputeRead as R, ComputeReadWrite as RW, ComputeWrite as W, Mesh as M};
use anyhow::Context;
use bytemuck::{Pod, Zeroable};
use std::collections::BTreeMap;
macro_rules! spirv {
    ($s:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/", $s, ".spv"))
    };
}
mod bricks;
mod mega;
pub(super) mod proxy;
mod tables;
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
const PLANT_LABELS: &[&str] = &[
    "Triangle plant clusters",
    "Voxel plant clusters",
    "Plant clusters in transition",
    "DAG voxel replacements",
    "Detailed shadow instances",
    "Detailed shadow triangles",
    "Coarse shadow instances",
    "Coarse shadow triangles",
    "Visible voxel bricks",
];
const WIND_LABELS: &[&str] = &["Wind-animated plant parts", "Active wind bones"];
const PLANT: u32 = 64;
const HAS_VOXEL: u32 = 256;
const GEOMETRY_VIEW: u32 = 512;
const WIND_DEFORM: u32 = 4096;
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct View {
    vp: [f32; 16],
    eye: [f32; 4],
    screen: [u32; 4],
    settings: [f32; 4],
    foliage: [f32; 4],
    caster_rays: [[f32; 4]; 2],
    inverse_vp: [f32; 16],
}
#[derive(Clone, Copy)]
struct Work {
    items: Resource,
    args: Resource,
}
// Keep each mesh indirect command within the legacy batch size. All pages
// index the same scene-wide visible list, including the final partial page.
const DRAW_PAGE_SIZE: u32 = 1 << 20;
fn arguments(g: &mut FrameGraph, name: &str, commands: u32) -> Resource {
    let r = g.input(
        name,
        bytemuck::cast_slice(&vec![[0u32, 1, 1, 0]; commands as usize]).to_vec(),
    );
    g.prefer_local_buffer(r);
    r
}
impl Work {
    fn new(g: &mut FrameGraph, name: &str, capacity: u32, commands: u32) -> Self {
        let items = g.append_list(name, 8 * capacity as usize);
        g.prefer_local_buffer(items);
        Self {
            items,
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
    plants: Option<vegetation::Settings>,
    previous_wind: Option<Resource>,
    skip_proxies: bool,
    mega: bool,
}
struct ClusterScene {
    assets: Resource,
    instances: Resource,
    view: Resource,
    stats: Resource,
    instance_count: u32,
    capacity: u32,
    voxel_capacity: u32,
    brick_draw_capacity: u32,
    voxel_hit_ranges: Option<Resource>,
    shadow_capacity: u32,
    shadow_elements: [u64; 2],
    shadow_ranges: Vec<[u32; 4]>,
    shadow_sources: Vec<Arc<[u8]>>,
    level_widths: Vec<u32>,
    palette: Option<Resource>,
}

fn upload_streams(
    g: &mut FrameGraph,
    streams: &bvh::Streams,
    bones: Option<Resource>,
    addresses: &mut Vec<Resource>,
) -> Geometry {
    let tree = g.resident_input("Spatial BVH", streams.tree.clone());
    let groups = g.resident_input("LOD groups", streams.groups.clone());
    let clusters = g.resident_input("Meshlets", streams.clusters.clone());
    let vertices = g.resident_input("Resident vertices", streams.vertices.clone());
    let indices = g.resident_input("Meshlet index stream", streams.indices.clone());
    addresses.extend([
        tree,
        groups,
        clusters,
        vertices,
        indices,
        bones.unwrap_or(vertices),
    ]);
    Geometry {
        vertices,
        indices,
        clusters: Some(clusters),
    }
}

impl ClusterScene {
    fn upload(
        g: &mut FrameGraph,
        t: &mut Targets,
        s: &Snapshot,
        view: ClusterView,
        objects: &BTreeMap<MeshId, Vec<usize>>,
        resident_slots: bool,
        cache: &mut BTreeMap<MeshId, Arc<bvh::Packed>>,
    ) -> Result<Self> {
        let mut addresses = Vec::new();
        let mut instances = Vec::<SurfaceInstance>::new();
        let mut level_widths = Vec::<u32>::new();
        let mut capacity = 0u32;
        let mut voxel_capacity = 0u32;
        let mut brick_draw_capacity = 0u32;
        let mut hit_ranges = Vec::<[u32; 4]>::new();
        let mut hit_slots = 0u64;
        let mut raster_instance = 0u32;
        let mut shadow_capacity = 0u32;
        let mut shadow_ranges = Vec::new();
        let mut shadow_sources = Vec::new();
        let mut shadow_first = 0u32;
        if view.mega {
            shadow_sources.push(s.scene.routes.bindings.clone());
        }
        let mut shadow_elements = [1u64; 2];
        let settings = view.plants.unwrap_or_default();
        let mut voxel_replacements = 0u32;
        let palette_matrices =
            objects
                .iter()
                .try_fold(0u32, |total, (id, instances)| -> Result<_> {
                    let count = s.scene.meshes[id]
                        .plant
                        .as_ref()
                        .map_or(0, |p| p.bones.len());
                    let added = count
                        .checked_mul(instances.len())
                        .context("plant palette capacity overflow")?;
                    total
                        .checked_add(added.try_into()?)
                        .context("plant palette capacity overflow")
                })?;
        let create_palette = |g: &mut FrameGraph, name| {
            let r = g.resource(
                name,
                Storage::Buffer {
                    bytes: 64 * palette_matrices as usize,
                    initial: None,
                },
            );
            g.prefer_local_buffer(r);
            r
        };
        let palette = (palette_matrices != 0).then(|| {
            if settings.wind > 0. {
                create_palette(g, "Wind joint palettes")
            } else {
                g.input(
                    "Rest-pose identity",
                    bytemuck::bytes_of(&Mat4::IDENTITY.to_cols_array()).to_vec(),
                )
            }
        });
        let previous_palette = view
            .previous_wind
            .zip(palette)
            .map(|(camera, _)| (camera, create_palette(g, "Previous wind joint palettes")));
        #[derive(Clone, Copy, Pod, Zeroable)]
        #[repr(C)]
        struct PreviousPlant {
            model: [f32; 16],
            scale: [f32; 4],
        }
        let mut previous_models = Vec::new();
        let palette_address = palette.map(|r| t.address(r)).transpose()?;
        let mut resolve_assets = Vec::new();
        let mut proxies = Vec::new();
        let mut first_instance = 0u32;
        let mut first_bone = 0u32;
        cache.retain(|id, _| objects.contains_key(id));
        for (id, objects) in objects {
            let e = &s.scene.meshes[id];
            let mixed = !matches!(settings.mode, vegetation::Mode::Triangles)
                && e.plant
                    .as_ref()
                    .is_some_and(|p| p.voxel_cell_size.is_some());
            if !cache.get(id).is_some_and(|p| p.matches(e, mixed)) {
                cache.insert(*id, Arc::new(bvh::Packed::build(e, mixed)?));
            }
            let packed = &cache[id];
            voxel_replacements = voxel_replacements
                .checked_add(packed.voxel_replacements)
                .context("DAG replacement counter overflow")?;
            let asset_index = (addresses.len() / 6).try_into()?;
            let bones = packed
                .bones
                .as_ref()
                .map(|data| g.resident_input("Plant rest skeleton", data.clone()));
            let geometry = upload_streams(g, &packed.streams, bones, &mut addresses);
            let n: u32 = objects.len().try_into()?;
            // A winning pixel carries one compact handle. Each prototype owns a
            // contiguous interval of instances and only its physical voxel pages
            // consume handle space. The table has one entry per asset, not per tree.
            if s.buffer_atomic64 && settings.raster != vegetation::BrickRaster::Mesh {
                let stride = (packed.streams.shadow_count - packed.streams.count) * 64;
                hit_ranges.push([
                    raster_instance,
                    hit_slots as u32,
                    stride,
                    packed.streams.count,
                ]);
                hit_slots += u64::from(stride) * u64::from(n);
                raster_instance = raster_instance
                    .checked_add(n)
                    .context("cluster instance overflow")?;
            }
            if g.products.request_ray_proxies && !view.skip_proxies {
                let data = packed.proxy.get_or_init(|| proxy::prototype(e)).clone();
                if e.plant.is_some() && !settings.coarse_shadows {
                    let detail = packed.shadow_detail.get_or_init(|| proxy::Detail::new(e));
                    let batches =
                        detail.batches(g, s, objects, first_instance, geometry.vertices, data);
                    proxies.extend(batches.into_iter().map(|batch| (*id, batch)));
                } else {
                    proxies.push((*id, proxy::Batch::coarse(data, first_instance, n)));
                }
                first_instance = first_instance
                    .checked_add(n)
                    .context("ray instance overflow")?;
            }
            for (total, per) in shadow_elements
                .iter_mut()
                .zip(packed.streams.shadow_elements)
            {
                *total = total
                    .checked_add(per * u64::from(n))
                    .context("cluster geometry count overflow")?;
            }
            if view.mega {
                shadow_ranges.push([
                    shadow_first,
                    shadow_capacity,
                    packed.streams.shadow_count,
                    0,
                ]);
                shadow_capacity = packed
                    .streams
                    .shadow_count
                    .checked_mul(n)
                    .and_then(|v| shadow_capacity.checked_add(v))
                    .context("shadow page list exceeds 32-bit indexing")?;
                shadow_first = shadow_first
                    .checked_add(n)
                    .context("shadow instance overflow")?;
                shadow_sources.extend([
                    packed.streams.tree.clone(),
                    packed.streams.clusters.clone(),
                    packed.streams.vertices.clone(),
                    packed.streams.indices.clone(),
                ]);
                shadow_sources.extend(packed.bones.iter().cloned());
            }
            capacity = packed
                .streams
                .count
                .checked_mul(n)
                .and_then(|v| capacity.checked_add(v))
                .context("global cluster list exceeds 32-bit indexing")?;
            // A visible cluster enters exactly one of the triangle and brick lists.
            // Their combined upper bound has already been checked above.
            voxel_capacity += packed.streams.voxel_count * n;
            brick_draw_capacity = packed
                .streams
                .brick_draws
                .checked_mul(n)
                .and_then(|v| brick_draw_capacity.checked_add(v))
                .context("brick draw list exceeds 32-bit indexing")?;
            // Every node on a level has at least one distinct descendant cluster.
            let widths = &packed.streams.level_widths;
            if level_widths.len() < widths.len() {
                level_widths.resize(widths.len(), 0);
            }
            for (width, &asset_width) in level_widths.iter_mut().zip(widths) {
                *width = n
                    .checked_mul(asset_width)
                    .and_then(|v| width.checked_add(v))
                    .context("global BVH frontier exceeds 32-bit indexing")?;
            }
            if resident_slots {
                let mut flags = u32::from(view.visualize_lod) << 5;
                if let Some(plant) = &e.plant {
                    flags |= PLANT;
                    if settings.wind > 0. {
                        flags |= WIND_DEFORM;
                    }
                    if settings.visualize {
                        flags |= GEOMETRY_VIEW;
                    }
                    if plant.voxel_cell_size.is_some() {
                        flags |= HAS_VOXEL;
                    }
                    if s.temporal.is_some() && settings.wind > 0. && plant.displacement_bound > 0. {
                        flags |= SurfaceInstance::TEMPORAL_REACTIVE;
                    }
                }
                resolve_assets.push([
                    t.address(geometry.vertices)?,
                    t.address(geometry.indices)?,
                    t.address(geometry.clusters.unwrap())?,
                    1,
                    flags,
                    palette_address.unwrap_or(0),
                    0,
                    0,
                ]);
                continue;
            }
            g.products.instance_visits += objects.len();
            for &slot in objects {
                let i = &s.scene.instances[slot];
                let model = i.model;
                let mut obj = t.material(s, i, model)?;
                obj.params[2] =
                    cl::cut::max_scale(glam::Mat3::from_mat4(model).transpose().to_cols_array_2d());
                obj.tex[3] |= u32::from(view.visualize_lod) << 5;
                obj.routing[2] = asset_index;
                if let Some(plant) = &e.plant {
                    obj.tex[3] |= PLANT;
                    if settings.wind > 0. {
                        obj.tex[3] |= WIND_DEFORM;
                    }
                    if view.plants.is_some_and(|s| s.visualize) {
                        obj.tex[3] |= GEOMETRY_VIEW;
                    }
                    obj.routing[3] = first_bone;
                    first_bone += plant.bones.len() as u32;
                    if plant.voxel_cell_size.is_some() {
                        obj.tex[3] |= HAS_VOXEL;
                    }
                    if s.temporal.is_some() && settings.wind > 0. && plant.displacement_bound > 0. {
                        obj.tex[3] |= SurfaceInstance::TEMPORAL_REACTIVE;
                    }
                }
                let history = s.surface_history(i)?;
                let prior = history.as_ref().and_then(|h| h.previous);
                if previous_palette.is_some() {
                    let old = prior.map_or(model, |i| i.model);
                    let scale = if old == model {
                        obj.params[2]
                    } else {
                        cl::cut::max_scale(
                            glam::Mat3::from_mat4(old).transpose().to_cols_array_2d(),
                        )
                    };
                    previous_models.push(PreviousPlant {
                        model: old.to_cols_array(),
                        scale: [scale, 0., 0., 0.],
                    });
                }
                let motion = history.map(|history| {
                    let source = match (prior, previous_palette) {
                        (Some(previous), Some((_, palette))) if e.plant.is_some() => {
                            MotionSource::Wind {
                                previous_model: previous.model,
                                palette,
                            }
                        }
                        (Some(previous), _) => MotionSource::rigid(model, previous.model),
                        (None, _) => MotionSource::Unavailable,
                    };
                    (history, source)
                });
                let mut resolve = obj;
                if obj.tex[3] & PLANT != 0 {
                    resolve.routing[2] = palette_address.unwrap();
                }
                obj.routing[1] = t.register(resolve, geometry, motion)?;
                instances.push(obj);
            }
        }
        let mut counts = [0u32; 27];
        for (_, batch) in &proxies {
            let (index, instances, triangles) = batch.stats()?;
            counts[22 + index] += instances;
            counts[23 + index] = counts[23 + index]
                .checked_add(triangles)
                .context("scene shadow triangle count overflow")?;
        }
        counts[16] = capacity;
        counts[21] = voxel_replacements;
        let stats = g.input("Cluster counters", bytemuck::bytes_of(&counts).to_vec());
        g.prefer_local_buffer(stats);
        let assets = g.address_table(addresses);
        let instance_count: u32 = objects.values().map(Vec::len).sum::<usize>().try_into()?;
        let (instances, gpu_previous) = if resident_slots {
            tables::prepare(
                g,
                t,
                s,
                &resolve_assets,
                instance_count,
                previous_palette.map(|(_, p)| p),
            )?
        } else {
            (
                g.input(
                    "Cluster instances",
                    bytemuck::cast_slice(&instances).to_vec(),
                ),
                None,
            )
        };
        if let Some(palette) = palette {
            let wind_stats = g.input("Vegetation wind counters", vec![0; 8]);
            if settings.wind > 0. {
                let groups = instance_count.div_ceil(64);
                let animate_shadows = u32::from(g.products.request_ray_proxies);
                g.pass_in(
                    PassPhase::Animation,
                    "Vegetation wind palettes",
                    Op::Compute {
                        program: compute(
                            spirv!("plants.comp"),
                            &[
                                (assets, R),
                                (instances, R),
                                (view.uniform, R),
                                (palette, W),
                                (wind_stats, RW),
                            ],
                            bytemuck::bytes_of(&[instance_count, animate_shadows]).to_vec(),
                        ),
                        groups: [groups.min(32768), groups.div_ceil(32768), 1],
                    },
                );
            }
            g.pass(
                "Vegetation wind statistics",
                Op::Counters {
                    buffer: wind_stats,
                    first: 0,
                    labels: WIND_LABELS.to_vec(),
                },
            );
        }
        if !proxies.is_empty() {
            let layout =
                g.resident_input("Cluster material bindings", s.scene.routes.bindings.clone());
            for (mesh, batch) in proxies {
                batch.publish(g, mesh, instances, layout, palette)?;
            }
        }
        if let Some((camera, palette)) = previous_palette {
            let models = gpu_previous.unwrap_or_else(|| {
                g.input(
                    "Previous plant transforms",
                    bytemuck::cast_slice(&previous_models).to_vec(),
                )
            });
            let groups = instance_count.div_ceil(64);
            g.pass_in(
                PassPhase::Animation,
                "Previous vegetation wind palettes",
                Op::Compute {
                    program: compute(
                        spirv!("plants_previous.comp"),
                        &[
                            (assets, R),
                            (instances, R),
                            (camera, R),
                            (palette, W),
                            (models, R),
                        ],
                        instance_count.to_ne_bytes().to_vec(),
                    ),
                    groups: [groups.min(32768), groups.div_ceil(32768), 1],
                },
            );
        }
        let voxel_hit_ranges =
            (!hit_ranges.is_empty() && (1..u64::from(u32::MAX)).contains(&hit_slots)).then(|| {
                g.input(
                    "Voxel hit ID ranges",
                    bytemuck::cast_slice(&hit_ranges).to_vec(),
                )
            });
        if brick_draw_capacity != 0
            && matches!(
                settings.raster,
                vegetation::BrickRaster::Compute | vegetation::BrickRaster::Compare
            )
        {
            ensure!(
                s.buffer_atomic64,
                "compute voxel raster requires shaderBufferInt64Atomics and shaderInt64"
            );
            ensure!(
                voxel_hit_ranges.is_some(),
                "scene exceeds the 32-bit voxel hit ID range"
            );
        }
        Ok(Self {
            assets,
            instance_count,
            instances,
            stats,
            capacity,
            voxel_capacity,
            brick_draw_capacity,
            voxel_hit_ranges,
            shadow_capacity,
            shadow_elements,
            shadow_ranges,
            shadow_sources,
            level_widths,
            view: view.uniform,
            palette,
        })
    }

    fn cull(&self, g: &mut FrameGraph) -> Result<Work> {
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
        g.prefer_local_buffer(nodes);
        g.prefer_local_buffer(counts);
        let dispatches = (0..levels)
            .map(|level| arguments(g, &format!("BVH level {level} dispatch"), 1))
            .collect::<Vec<_>>();
        let root_groups = self.instance_count.div_ceil(64);
        g.pass_in(
            PassPhase::Culling,
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
            g.pass_in(
                PassPhase::Culling,
                &format!("GPU BVH level {level}"),
                Op::ComputeIndirect {
                    program: compute(shader, &bindings, bytemuck::bytes_of(&push).to_vec()),
                    args,
                    offset: 0,
                },
            );
        }
        let selected = Work::new(g, "LOD selected clusters", self.capacity, 1);
        g.pass_in(
            PassPhase::Culling,
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
        g: &mut FrameGraph,
        history: Resource,
        input: Work,
        out: Work,
        deferred: Option<Work>,
        bricks: Option<&bricks::BrickJobs>,
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
        let variants: (&'static [u8], &'static [u8]) = match bricks {
            None => (spirv!("occlusion.comp"), spirv!("occlusion_post.comp")),
            Some(_) => (
                spirv!("occlusion_bricks.comp"),
                spirv!("occlusion_bricks_post.comp"),
            ),
        };
        let (name, shader) = if let Some(defer) = deferred {
            bindings.extend([(defer.items, RW), (defer.args, RW)]);
            ("Main Hi-Z cull", variants.0)
        } else {
            ("Post Hi-Z retest", variants.1)
        };
        if let Some(jobs) = bricks {
            bindings.extend(jobs.outputs());
        }
        g.pass_in(
            PassPhase::Culling,
            name,
            Op::ComputeIndirect {
                program: compute(shader, &bindings, vec![]),
                args: input.args,
                offset: 0,
            },
        );
    }

    fn draw(&self, g: &mut FrameGraph, t: &mut Targets, out: Work, name: &str) {
        for page in 0..(self.capacity - self.voxel_capacity).div_ceil(DRAW_PAGE_SIZE) {
            let mut bindings = vec![
                (self.assets, M),
                (self.instances, M),
                (self.view, M),
                (out.items, M),
            ];
            let (mesh, fragment): (&'static [u8], &'static [u8]) =
                if let Some(palette) = self.palette {
                    bindings.push((palette, M));
                    (
                        spirv!("cluster_plant.mesh"),
                        spirv!("visibility_plant.frag"),
                    )
                } else {
                    (spirv!("cluster.mesh"), spirv!("visibility.frag"))
                };
            let program = Dispatch {
                shader: Shader::Mesh(mesh, fragment),
                bindings,
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
    fn build(&self, g: &mut FrameGraph, depth: Resource, h: Resource, view: Resource, name: &str) {
        for (i, &params) in self.levels.iter().enumerate() {
            let mut bindings = vec![(h, RW), (view, R)];
            let shader: &'static [u8] = if i == 0 {
                bindings.push((depth, Access::ComputeSample));
                spirv!("hiz_depth.comp")
            } else {
                spirv!("hiz.comp")
            };
            let program = compute(shader, &bindings, bytemuck::bytes_of(&params).to_vec());
            g.pass_in(
                PassPhase::Pyramids,
                &format!("{name} mip {}", i + 1),
                Op::Compute {
                    program,
                    groups: [params[0].div_ceil(16), params[1].div_ceil(16), 1],
                },
            );
        }
    }
}
impl ClusterLod {
    pub(in crate::experiments) fn draw_with(
        &self,
        g: &mut FrameGraph,
        t: &mut Targets,
        s: &Snapshot,
        ordinary: impl FnOnce(&mut FrameGraph, &mut Targets) -> Result<()>,
    ) -> Result<()> {
        let debug = s
            .scene
            .setting(VISUALIZE)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let plants = s
            .scene
            .setting(vegetation::SETTING)
            .map(vegetation::Settings::decode)
            .transpose()?;
        let pixel_error = s
            .scene
            .setting(SETTING)
            .and_then(|value| value.as_f64())
            .unwrap_or(1.) as f32;
        ensure!(
            pixel_error.is_finite() && pixel_error >= 0.,
            "invalid pixel error"
        );
        let prior_layout_ok = s
            .temporal
            .as_ref()
            .and_then(|f| f.previous_scene.as_ref())
            .is_none_or(|p| s.scene.instances.same_layout(&p.instances));
        let fast = !self.full_upload && prior_layout_ok;
        t.full_upload = !fast;
        let mut full = BTreeMap::new();
        let groups = if fast {
            &s.scene.routes.groups
        } else {
            g.products.instance_visits += s.scene.instances.len();
            for (idx, i) in s.scene.instances.iter().enumerate() {
                if Self::owns(s, i) {
                    full.entry(i.mesh).or_insert_with(Vec::new).push(idx);
                }
            }
            &full
        };
        if groups.is_empty() {
            return ordinary(g, t);
        }
        let pyramid = Pyramid::new(s.extent)?;
        let history = g.history_buffer(
            "Temporal Hi-Z and camera",
            self.history,
            (pyramid.header as usize + 17) * 4,
        );
        g.prefer_local_buffer(history);
        let restir = s
            .scene
            .setting(crate::experiments::restir::SETTING)
            .map(crate::experiments::restir::decode)
            .transpose()?;
        let mega = plants.is_some_and(|v| v.mega_shadows)
            && g.products.request_ray_proxies
            && crate::render::rays::view(s).is_none()
            && restir.is_some();
        let indirect = restir.is_some_and(|cfg| cfg.indirect());
        let (caster_rays, light_count) =
            if mega && !indirect && restir.is_some_and(|cfg| cfg.scene_lights(s)) {
                mega::caster_directions(s)
            } else {
                ([[0.; 4]; 2], -1.)
            };
        let camera = View {
            caster_rays,
            vp: s.view_projection().to_cols_array(),
            inverse_vp: s.view_projection().inverse().to_cols_array(),
            eye: s
                .camera
                .eye()
                .extend(s.extent[1] as f32 * 0.5 / 25f32.to_radians().tan())
                .into(),
            screen: [s.extent[0], s.extent[1], pyramid.header, DRAW_PAGE_SIZE],
            settings: [
                pixel_error,
                (s.camera.distance * 0.001).max(0.0001),
                plants.map_or(0., |s| s.time),
                plants.map_or(0., |s| s.wind),
            ],
            foliage: [
                plants.map_or(pixel_error, |plant| match plant.mode {
                    vegetation::Mode::Auto => plant.pixels,
                    vegetation::Mode::Triangles => pixel_error.min(0.55),
                    vegetation::Mode::Voxels => (4. * plant.pixels).max(2.),
                }),
                u32::from(mega) as f32,
                light_count,
                0.,
            ],
        };
        let view = g.input("Cluster camera", bytemuck::bytes_of(&camera).to_vec());
        let previous_wind = s.temporal.as_ref().and_then(|frame| {
            let previous = frame
                .previous_scene
                .as_ref()?
                .setting(vegetation::SETTING)?;
            vegetation::Settings::decode(previous)
                .ok()
                .filter(|old| old.wind > 0. || plants.is_some_and(|now| now.wind > 0.))
                .map(|old| {
                    let mut wind_view = camera;
                    wind_view.eye = frame.previous_camera.eye().extend(camera.eye[3]).into();
                    wind_view.settings[1] = (frame.previous_camera.distance * 0.001).max(0.0001);
                    wind_view.settings[2] = old.time;
                    wind_view.settings[3] = old.wind;
                    g.input(
                        "Previous wind settings",
                        bytemuck::bytes_of(&wind_view).to_vec(),
                    )
                })
        });
        let proxy_start = g.products.ray_surfaces.len();
        let scene = ClusterScene::upload(
            g,
            t,
            s,
            ClusterView {
                uniform: view,
                visualize_lod: debug,
                plants,
                previous_wind,
                skip_proxies: mega && !indirect,
                mega,
            },
            groups,
            fast,
            &mut self.cache.lock().unwrap(),
        )?;
        let selected = scene.cull(g)?;
        if mega {
            let (models, _) = t.scene_tables(g, s)?;
            let layout =
                g.resident_input("Shadow instance layout", s.scene.routes.bindings.clone());
            mega::publish(
                g,
                &scene,
                selected,
                proxy_start,
                [layout, models],
                plants.is_some_and(|v| v.rebuild_clusters),
            )?;
        }
        let triangle_capacity = scene.capacity - scene.voxel_capacity;
        let pages = triangle_capacity.div_ceil(DRAW_PAGE_SIZE);
        let main = Work::new(g, "Main visible clusters", triangle_capacity, pages);
        let deferred = Work::new(g, "Deferred clusters", scene.capacity, 1);
        let post = Work::new(g, "Recovered clusters", triangle_capacity, pages);
        let compare = plants.is_some_and(|p| p.raster == vegetation::BrickRaster::Compare);
        let bricks = bricks::BrickJobs::new(g, scene.brick_draw_capacity);
        scene.visibility(g, history, selected, main, Some(deferred), bricks.as_ref());
        scene.draw(g, t, main, "Main mesh visibility");
        ordinary(g, t)?;

        // A triangle-only intermediate pyramid is conservative. Main and
        // recovered brick jobs wait for the final opaque triangle depth.
        pyramid.build(g, t.depth, history, view, "Current Hi-Z");
        scene.visibility(g, history, deferred, post, None, bricks.as_ref());
        scene.draw(g, t, post, "Post mesh visibility");
        if let Some(jobs) = &bricks {
            if let Some(ranges) = scene.voxel_hit_ranges {
                let raster = bricks::SoftwareRaster::new(g, t, ranges, compare);
                raster.trace(g, &scene, jobs);
                raster.finish(g, t, &scene, jobs);
            } else {
                jobs.draw_mesh(g, t, &scene);
            }
        }
        g.pass(
            "Cluster statistics",
            Op::Counters {
                buffer: scene.stats,
                first: 0,
                labels: if scene.palette.is_some() {
                    [LABELS, PLANT_LABELS].concat()
                } else {
                    LABELS.to_vec()
                },
            },
        );
        pyramid.build(g, t.depth, history, view, "History Hi-Z");
        Ok(())
    }
    pub(in crate::experiments) fn owns(s: &Snapshot, i: &RenderInstance) -> bool {
        i.geometry_path(&s.scene.meshes[&i.mesh], true) == GeometryPath::ClusterLod
    }
    pub(in crate::experiments) fn atmosphere(
        g: &mut FrameGraph,
        s: &Snapshot,
        inputs: crate::render::SceneLayers,
    ) -> Result<()> {
        let debug = s
            .scene
            .setting(VISUALIZE)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let plants = s
            .scene
            .setting(vegetation::SETTING)
            .map(vegetation::Settings::decode)
            .transpose()?;
        if let Some(plants) = plants {
            vegetation::atmosphere(g, s, inputs, debug || plants.visualize);
        }
        Ok(())
    }
}
impl Technique for ClusterLod {
    fn build(&self, s: &Snapshot) -> Result<FrameGraph> {
        build_frame_with_post(
            s,
            Atlas::cached(&s.scene)?,
            |g, t| {
                self.draw_with(g, t, s, |g, t| {
                    let front = Forward::default();
                    if self.full_upload {
                        front.draw_subset(g, t, s, |i| !Self::owns(s, i))
                    } else {
                        let slots = &s.scene.routes.ordinary;
                        g.products.instance_visits += slots.len();
                        for surface in front.prepare_surfaces(
                            s,
                            slots.iter().map(|&i| &s.scene.instances[i]).collect(),
                        )? {
                            let material = t.material(s, surface.instance, Mat4::IDENTITY)?;
                            surface.draw(g, t, s, material)?;
                        }
                        Ok(())
                    }
                })
            },
            |g, inputs| Self::atmosphere(g, s, inputs),
            |_, _| Ok(()),
        )
    }
}
