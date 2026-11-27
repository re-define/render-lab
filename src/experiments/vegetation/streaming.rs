//! Two neighboring logical cells use one real forest oak, one page cache and one GPU feedback stream.
use crate::{
    experiments::assembly::{Primary, Registration},
    rdg::{Access, Dispatch, DrawCount, Op, Resource, Shader, Storage},
    render::{Camera, FrameGraph, Snapshot, Technique},
    scene::{
        Entity, Instance, World,
        partition::{Cell, Layout},
    },
    streaming::{Budget, Package, Streamer},
};
use anyhow::{Context, Result, ensure};
use glam::{Mat4, Vec3};
use std::{
    collections::BTreeMap,
    io::Write,
    sync::{Arc, Mutex},
};
mod cook;
const KEY: &str = "lab.forest_stream";
const OAK: &str = "forest.oak.v1";
const DRAW_PAGE_CHUNKS: u32 = 4096;
pub(in crate::experiments) const MODULE: Registration = Registration {
    key: KEY,
    root_writers: &[],
    primary: Primary::Exclusive,
    restore: None,
    install: |world, a| {
        let (_, settings) = world
            .first_raw_component(KEY)
            .context("missing forest stream settings")?;
        let mesh = settings["prototype"]
            .as_u64()
            .context("forest stream proxy id")?;
        let cache_frames = settings["cache_frames"]
            .as_u64()
            .context("forest stream retention")?;
        let (data, package) = cook::open()?;
        a.camera = Some(Camera {
            target: Vec3::new(0., 3., 0.),
            distance: 14.,
            pitch: 0.08,
            yaw: 0.14,
        });
        a.technique = Some(Arc::new(Forest {
            mesh,
            cache_frames,
            data,
            package,
            active: Mutex::default(),
        }));
        Ok(())
    },
};
pub fn scene() -> Result<World> {
    scene_with_cache_frames(75)
}
/// Replays use exactly the interactive scene, varying only the cold-page retention window.
pub fn scene_with_cache_frames(cache_frames: u64) -> Result<World> {
    let mut w = World::default();
    // Instances use this metadata entity for extraction; heavy mesh groups live in the cooked package.
    let mesh = w.spawn(Entity {
        name: "Streamed oak".into(),
        mesh: Some(Arc::new(super::generate::empty())),
        ..Default::default()
    });
    w.spawn(Entity {
        name: "Forest streaming".into(),
        components: [(
            KEY.into(),
            serde_json::json!({"prototype":mesh,"cache_frames":cache_frames}),
        )]
        .into(),
        ..Default::default()
    });
    let dir = crate::assets::cache_dir().join("forest-stream-cells");
    std::fs::create_dir_all(&dir)?;
    let mut cells = Vec::new();
    for k in 0..2 {
        let center = Vec3::new(k as f32 * 22., 0., 0.);
        let mut rows = Vec::new();
        for z in 0..3 {
            for x in 0..4 {
                let i = x + z * 4;
                let pos = center + Vec3::new((x as f32 - 1.5) * 4.5, 0., (z as f32 - 1.) * 4.5);
                let m = Mat4::from_translation(pos)
                    * Mat4::from_rotation_y(i as f32 * 1.91)
                    * Mat4::from_scale(Vec3::splat(0.82 + 0.06 * (i % 5) as f32));
                rows.push(serde_json::json!({"name":"Oak","local":m,"instance":Instance::new(mesh,None),"components":{}}));
            }
        }
        let bytes = serde_json::to_vec(&rows)?;
        let file = format!(
            "{}.json",
            crate::assets::key_name(&crate::assets::content_key("oak-cell", 1, &bytes))
        );
        let target = dir.join(&file);
        if !target.is_file() {
            let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
            tmp.write_all(&bytes)?;
            if let Err(e) = tmp.persist_noclobber(&target)
                && !target.is_file()
            {
                return Err(e.into());
            }
        }
        cells.push(Cell {
            first: 1000 + k * 64,
            count: rows.len(),
            center,
            file: file.into(),
            save_state: false,
            // The eastern cell requires the first band of fine trunk tiles before spawning.
            assets: BTreeMap::from([(OAK.into(), k as u8)]),
        });
    }
    w.partition = Some(Arc::new(Layout::new(dir, cells, 18., 25., 32.)?));
    Ok(w)
}

struct Active {
    stream: Streamer,
    groups: Arc<[u8]>,
    chunks: Arc<[u8]>,
    parents: Arc<[u8]>,
    order: Arc<[u8]>,
    roots: Arc<[u8]>,
}
struct Forest {
    mesh: u64,
    cache_frames: u64,
    data: cook::Cooked,
    package: Package,
    active: Mutex<Option<Active>>,
}
impl Forest {
    fn activate(&self) -> Result<Active> {
        // Distinct root owners let the RHI retire the entire previous residency on exit.
        // The small CPU directory remains with the scene, so re-entry needs no synchronous I/O.
        let package = Package {
            path: self.package.path.clone(),
            parts: self
                .package
                .parts
                .iter()
                .cloned()
                .map(|mut part| {
                    part.root = Arc::from(part.root.as_ref());
                    part
                })
                .collect(),
        };
        let mut stream = Streamer::new(
            package,
            Budget {
                pool_pages: 128,
                ..Default::default()
            },
        )?;
        stream.set_cache_frames(self.cache_frames);
        let d = &self.data;
        Ok(Active {
            stream,
            groups: bytemuck::cast_slice(&d.groups).into(),
            chunks: bytemuck::cast_slice(&d.chunks).into(),
            parents: bytemuck::cast_slice(&d.parents).into(),
            order: bytemuck::cast_slice(&d.order).into(),
            roots: bytemuck::cast_slice(&d.roots).into(),
        })
    }
}
macro_rules! spv {
    ($name:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/", $name, ".spv"))
    };
}
fn compute(
    g: &mut FrameGraph,
    name: &str,
    shader: &'static [u8],
    bindings: &[(Resource, Access)],
    params: &[u32],
    groups: [u32; 3],
) {
    g.pass(
        name,
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(shader),
                bindings: bindings.to_vec(),
                push: bytemuck::cast_slice(params).to_vec(),
            },
            groups,
        },
    );
}
fn root(g: &mut FrameGraph, name: &str, data: &Arc<[u8]>) -> Resource {
    let r = g.resident_input(name, data.clone());
    g.stream_roots.insert(r);
    r
}
impl Technique for Forest {
    fn accepts_temporal_inputs(&self, _: &Snapshot) -> bool {
        false
    }
    fn build(&self, s: &Snapshot) -> Result<FrameGraph> {
        use Access::{ComputeRead as R, ComputeReadWrite as RW, ComputeWrite as W, Mesh as M};
        let mut g = FrameGraph::new(s.extent);
        let [width, height] = s.extent;
        let mut state = self.active.lock().unwrap();
        let needed = s.cell_asset_levels.contains_key(OAK);
        if !needed {
            state.take();
        } else if state.is_none() {
            *state = Some(self.activate()?);
        }
        let models: Vec<_> = s
            .scene
            .instances()
            .iter()
            .filter(|i| i.mesh.0 == self.mesh)
            .map(|i| i.model.to_cols_array())
            .collect();
        let n = models.len() as u32;
        let out = g.local_buffer(
            "Forest displayed color",
            width as usize * height as usize * 4,
        );
        let stats = g.input("Forest drawn primitives", vec![0; 24]);
        let mut metrics = [0u32; 10];
        if let Some(a) = state.as_mut() {
            let minimum = s.cell_asset_levels.get(OAK).copied().unwrap_or_default();
            for tile in 0..4 {
                a.stream.set_minimum(tile, usize::from(minimum > 0))?;
            }
            let view = a
                .stream
                .bind_view(&mut g, s.stream_adapter, !s.secondary_view)?;
            let units: Vec<_> = view
                .parts
                .iter()
                .map(|binding| {
                    let p = binding.as_ref().expect("forest parts are removed together");
                    [p.level as u32, p.first_page, p.word_offset, 0u32]
                })
                .collect();
            let ready = u8::from(units[..4].iter().all(|entry| entry[0] != 0));
            if !s.secondary_view {
                s.report_asset_residency(OAK, ready);
            }
            let bits = a.stream.request_buffer(&mut g);
            let u = g.input(
                "Resident forest units",
                bytemuck::cast_slice(&units).to_vec(),
            );
            metrics = [
                n,
                units[..cook::TILES].iter().filter(|r| r[0] > 0).count() as u32,
                units[cook::TILES..].iter().filter(|r| r[0] > 0).count() as u32,
                view.uploads as u32,
                view.stats.reads as u32,
                view.stats.rereads as u32,
                view.stats.requested_bytes.min(u32::MAX as u64) as u32,
                (view.stats.first_use_p95_ms * 1000.) as u32,
                1,
                view.stats.reread_bytes.min(u32::MAX as u64) as u32,
            ];
            if n > 0 {
                let groups = root(&mut g, "Forest DAG groups", &a.groups);
                let chunks = root(&mut g, "Forest draw chunks", &a.chunks);
                let parents = root(&mut g, "Forest DAG incoming edges", &a.parents);
                let order = root(&mut g, "Forest DAG depth order", &a.order);
                let roots = root(&mut g, "Pinned trunk and canopy roots", &a.roots);
                let models = g.input(
                    "Active oak transforms",
                    bytemuck::cast_slice(&models).to_vec(),
                );
                let n_groups = self.data.groups.len() as u32;
                let n_chunks = self.data.chunks.len() as u32;
                let capacity = n
                    .checked_mul(n_chunks)
                    .context("forest draw list overflow")?;
                let pages = capacity.div_ceil(DRAW_PAGE_CHUNKS);
                let mut params =
                    bytemuck::cast_slice(&s.view_projection().to_cols_array()).to_vec();
                params.extend(bytemuck::bytes_of(
                    &s.camera
                        .eye()
                        .extend(height as f32 / (2. * 25f32.to_radians().tan()))
                        .to_array(),
                ));
                params.extend(bytemuck::bytes_of(&[width, height, n_groups, n_chunks]));
                let f = g.input("Forest camera", params);
                let active = g.input(
                    "Complete DAG cuts",
                    vec![0; 4 * n as usize * n_groups as usize],
                );
                let jobs = g.local_buffer(
                    "Visible oak draw chunks",
                    8 * n.max(1) as usize * n_chunks as usize,
                );
                let args = g.input(
                    "Forest mesh draw count",
                    bytemuck::cast_slice(&vec![[0u32, 1, 1, 0]; pages as usize]).to_vec(),
                );
                for &(first, count) in &self.data.layers {
                    compute(
                        &mut g,
                        "Resolve forest DAG level",
                        spv!("forest_stream_cut.comp"),
                        &[
                            (f, R),
                            (models, R),
                            (groups, R),
                            (parents, R),
                            (order, R),
                            (u, R),
                            (active, RW),
                            (bits, RW),
                        ],
                        &[first, count],
                        [count.div_ceil(64), n, 1],
                    );
                }
                compute(
                    &mut g,
                    "Emit resident forest cut",
                    spv!("forest_stream_draws.comp"),
                    &[
                        (f, R),
                        (chunks, R),
                        (active, R),
                        (jobs, W),
                        (args, RW),
                        (stats, RW),
                    ],
                    &[DRAW_PAGE_CHUNKS],
                    [n_chunks.div_ceil(64), n, 1],
                );
                let vis = g.resource("Forest hit material and UV", Storage::Color);
                let depth = g.resource("Forest depth", Storage::Depth);
                for page in 0..pages {
                    g.pass(
                        "Draw resident oak clusters",
                        Op::Draw {
                            program: Dispatch {
                                shader: Shader::Mesh(
                                    spv!("forest_stream.mesh"),
                                    spv!("forest_stream.frag"),
                                ),
                                bindings: vec![
                                    (f, M),
                                    (models, M),
                                    (groups, M),
                                    (chunks, M),
                                    (u, M),
                                    (view.table, M),
                                    (roots, M),
                                    (jobs, M),
                                ],
                                push: (page * DRAW_PAGE_CHUNKS).to_le_bytes().to_vec(),
                            },
                            color: vis,
                            depth,
                            clear: page == 0,
                            count: DrawCount::MeshIndirect {
                                args,
                                offset: u64::from(page) * 16,
                            },
                        },
                    );
                }
                compute(
                    &mut g,
                    "Shade bark tiles and foliage",
                    spv!("forest_stream_resolve.comp"),
                    &[
                        (vis, Access::ComputeSample),
                        (view.table, R),
                        (u, R),
                        (out, W),
                        (bits, RW),
                        (stats, RW),
                    ],
                    &[],
                    [width.div_ceil(8), height.div_ceil(8), 1],
                );
            }
            a.stream.observe_requests(&mut g, bits, s.secondary_view);
        }
        if n == 0 || !needed {
            compute(
                &mut g,
                "Forest sky",
                spv!("forest_stream_sky.comp"),
                &[(out, W)],
                &[width, height],
                [width.div_ceil(8), height.div_ceil(8), 1],
            );
        }
        let counts = g.input(
            "Forest stream residency",
            bytemuck::bytes_of(&metrics).to_vec(),
        );
        g.pass(
            "Read forest residency",
            Op::Counters {
                buffer: counts,
                first: 0,
                labels: vec![
                    "forest oak instances",
                    "resident bark tiles",
                    "resident canopy groups",
                    "forest page uploads",
                    "forest reads",
                    "forest rereads",
                    "forest requested bytes",
                    "forest first use p95 us",
                    "oak cache active",
                    "forest reread bytes",
                ],
            },
        );
        g.pass(
            "Read selected oak cut",
            Op::Counters {
                buffer: stats,
                first: 0,
                labels: vec![
                    "trunk triangles",
                    "canopy triangles",
                    "coarse bark pixels",
                    "fine bark pixels",
                    "forest drawn chunks",
                    "forest nonempty draws",
                ],
            },
        );
        g.set_output(out);
        Ok(g)
    }
}
