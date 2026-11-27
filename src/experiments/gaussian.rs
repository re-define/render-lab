//! Indexed random access to exported 2DGaussian texture fields.
use crate::experiments::assembly::{Primary, Registration as SceneModule};
pub(super) const MODULE: SceneModule = SceneModule {
    root_writers: &[],
    key: SETTING,
    primary: Primary::Exclusive,
    restore: None,
    install: |world, a| {
        register(&mut a.materials)?;
        a.camera = Some(camera(world)?);
        a.technique = Some(std::sync::Arc::new(Gaussian::new(world)?));
        Ok(())
    },
};
mod bvh;
mod compact;
use super::texture;
use crate::{
    engine::Engine,
    import,
    render::FrameGraph,
    render::{AtlasCache, Camera, Forward, MaterialPrograms, Snapshot, Technique},
    scene::*,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, sync::Arc};

pub const SETTING: &str = "lab.gaussian_texture";
const DEMO: &[u8] = include_bytes!("../../assets/gaussian/FloorTiles.g2t");
const TILE: &str = "Gaussian tile";
const BVH: &str = "Gaussian BVH";
const COMPACT_TILE: &str = "Gaussian compact tile";
const COMPACT_BVH: &str = "Gaussian compact BVH";
const SCAN: &str = "Gaussian scan";
const ORIGINAL: &str = "Gaussian original";

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Tile,
    Bvh,
    CompactTile,
    CompactBvh,
    Scan,
    Original,
}
impl Mode {
    fn program(self) -> &'static str {
        match self {
            Self::Tile => TILE,
            Self::Bvh => BVH,
            Self::CompactTile => COMPACT_TILE,
            Self::CompactBvh => COMPACT_BVH,
            Self::Scan => SCAN,
            Self::Original => ORIGINAL,
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Settings {
    texture: Id,
    material: Id,
    preview: bool,
    mode: Mode,
}
impl Settings {
    fn read(w: &World) -> Result<(Id, Self)> {
        let (id, value) = w
            .first_raw_component(SETTING)
            .context("open the gaussian experiment first")?;
        Ok((id, serde_json::from_value(value.clone())?))
    }
    fn store(&self, w: &mut World, id: Id) -> Result<()> {
        let m = w
            .entities
            .get_mut(&self.material)
            .and_then(|e| e.material.as_mut())
            .context("missing Gaussian material")?;
        ensure!(
            m.texture == Some(self.texture),
            "Gaussian material texture mismatch"
        );
        m.shading = Shading::Custom(self.mode.program().into());
        w.entities
            .get_mut(&id)
            .context("missing Gaussian settings")?
            .components
            .insert(SETTING.into(), serde_json::to_value(self)?);
        Ok(())
    }
}
struct Layout {
    width: u32,
    height: u32,
    size: u32,
    count: u32,
    refs: u32,
    tiles: u32,
}
impl Layout {
    fn read(blob: &[u8]) -> Result<Self> {
        ensure!(
            blob.len() >= 32 && blob.len().is_multiple_of(4) && &blob[..4] == b"G2T1",
            "invalid G2T header"
        );
        let word = |i: usize| u32::from_le_bytes(blob[4 * i..4 * i + 4].try_into().unwrap());
        let (width, height, size, count, refs) = (word(1), word(2), word(3), word(4), word(5));
        ensure!(
            (1..=32768).contains(&width)
                && (1..=32768).contains(&height)
                && matches!(size, 8 | 16 | 32),
            "invalid Gaussian extent or tile size"
        );
        let tiles = width.div_ceil(size) * height.div_ceil(size);
        let dir = word(6);
        let ids = word(7);
        let id_words = if count <= 65536 {
            refs.div_ceil(2)
        } else {
            refs
        };
        ensure!(
            u64::from(dir) == 8 + 10 * u64::from(count)
                && u64::from(ids) == u64::from(dir) + u64::from(tiles) + 1
                && u64::from(ids) + u64::from(id_words) == (blob.len() / 4) as u64,
            "invalid G2T offsets"
        );
        let dir = dir as usize;
        let ids = ids as usize;
        ensure!(
            word(dir) == 0
                && word(ids - 1) == refs
                && (dir..ids - 1).all(|i| word(i) <= word(i + 1))
                && (0..refs as usize).all(|i| {
                    let id = if count <= 65536 {
                        (word(ids + i / 2) >> (16 * (i % 2))) & 65535
                    } else {
                        word(ids + i)
                    };
                    id < count
                }),
            "invalid Gaussian candidate list"
        );
        for i in 0..count as usize {
            let start = 8 + 10 * i;
            let v: [f32; 8] = std::array::from_fn(|j| f32::from_bits(word(start + j)));
            ensure!(
                v.iter().all(|x| x.is_finite())
                    && v[2] > 0.
                    && v[4] > 0.
                    && f64::from(v[2]) * f64::from(v[4]) > f64::from(v[3]).powi(2),
                "invalid Gaussian coefficients"
            );
        }
        Ok(Self {
            width,
            height,
            size,
            count,
            refs,
            tiles,
        })
    }
}
fn preview(layout: &Layout) -> (World, Id, Id) {
    let mut w = World::default();
    let texture = w.spawn(Entity {
        name: "Gaussian texture".into(),
        texture: Some(Arc::new(Texture {
            width: 1,
            height: 1,
            rgba: vec![255; 4],
        })),
        ..Default::default()
    });
    let material = w.spawn(Entity {
        name: "Gaussian preview".into(),
        material: Some(Material {
            texture: Some(texture),
            base_color: [1.; 4],
            metallic: 0.,
            roughness: 1.,
            ..Default::default()
        }),
        ..Default::default()
    });
    let a = layout.width as f32 / layout.width.max(layout.height) as f32;
    let b = layout.height as f32 / layout.width.max(layout.height) as f32;
    let mesh = w.spawn(Entity {
        mesh: Some(Arc::new(Mesh {
            positions: vec![[-a, b, 0.], [-a, -b, 0.], [a, -b, 0.], [a, b, 0.]],
            indices: vec![0, 1, 2, 0, 2, 3],
            uv: vec![[0., 0.], [0., 1.], [1., 1.], [1., 0.]],
            normals: vec![[0., 0., 1.]; 4],
            joints: vec![],
            weights: vec![],
        })),
        ..Default::default()
    });
    w.spawn(Entity {
        instance: Some(Instance::new(mesh, Some(material))),
        ..Default::default()
    });
    (w, texture, material)
}
pub fn scene(path: Option<&Path>) -> Result<World> {
    let blob = if let Some(p) = path {
        std::fs::read(p).with_context(|| format!("opening {}", p.display()))?
    } else {
        DEMO.to_vec()
    };
    let layout = Layout::read(&blob)?;
    let (mut w, texture, material) = if path.is_some() {
        preview(&layout)
    } else {
        let mut w = World::default();
        let gltf = Path::new("assets/scenes/modern-hall/modern_hall_core.gltf");
        import::import(&mut w, gltf)?;
        let (material, texture) = w
            .entities
            .iter()
            .find_map(|(&id, e)| {
                if e.name == "FloorTiles" {
                    Some((id, e.material.as_ref()?.texture?))
                } else {
                    None
                }
            })
            .context("missing Modern Hall floor")?;
        let t = w.entities[&texture].texture.as_ref().unwrap();
        ensure!(
            (t.width, t.height) == (layout.width, layout.height),
            "floor extent mismatch"
        );
        (w, texture, material)
    };
    w.entities.get_mut(&texture).unwrap().gaussian_payload = Some(Arc::new(blob));
    let id = w.spawn(Entity {
        name: "2DGaussian texture".into(),
        ..Default::default()
    });
    Settings {
        texture,
        material,
        preview: path.is_some(),
        mode: Mode::Tile,
    }
    .store(&mut w, id)?;
    Ok(w)
}
pub fn camera(w: &World) -> Result<Camera> {
    if Settings::read(w)?.1.preview {
        Ok(Camera {
            target: glam::Vec3::ZERO,
            distance: 2.45,
            yaw: 0.,
            pitch: 0.,
        })
    } else {
        texture::camera(w)
    }
}
pub fn register(materials: &mut MaterialPrograms) -> Result<()> {
    for (name, shader) in [
        (
            TILE,
            include_bytes!(concat!(env!("OUT_DIR"), "/gaussian_tile.comp.spv")).as_slice(),
        ),
        (
            BVH,
            include_bytes!(concat!(env!("OUT_DIR"), "/gaussian_bvh.comp.spv")).as_slice(),
        ),
        (
            COMPACT_TILE,
            include_bytes!(concat!(env!("OUT_DIR"), "/gaussian_compact_tile.comp.spv")).as_slice(),
        ),
        (
            COMPACT_BVH,
            include_bytes!(concat!(env!("OUT_DIR"), "/gaussian_compact_bvh.comp.spv")).as_slice(),
        ),
        (
            SCAN,
            include_bytes!(concat!(env!("OUT_DIR"), "/gaussian_scan.comp.spv")).as_slice(),
        ),
        (
            ORIGINAL,
            include_bytes!(concat!(env!("OUT_DIR"), "/pbr.comp.spv")).as_slice(),
        ),
    ] {
        if materials.bin(&Shading::Custom(name.into())).is_err() {
            materials.register(name, shader)?;
        }
    }
    Ok(())
}
pub struct Gaussian {
    id: Id,
    blob: Option<Arc<Vec<u8>>>,
    atlas: AtlasCache,
}
impl Gaussian {
    fn payload(w: &World, id: Id) -> Result<&Arc<Vec<u8>>> {
        w.entities
            .get(&id)
            .and_then(|e| e.gaussian_payload.as_ref())
            .context("missing Gaussian payload")
    }
    pub fn new(w: &World) -> Result<Self> {
        let (_, settings) = Settings::read(w)?;
        Self::for_settings(w, &settings)
    }
    fn for_settings(w: &World, settings: &Settings) -> Result<Self> {
        let data = Self::payload(w, settings.texture)?;
        let layout = Layout::read(data)?;
        let blob = match settings.mode {
            Mode::Original => None,
            Mode::Tile => Some(data.clone()),
            Mode::Bvh => Some(Arc::new(bvh::Tree::build(data, &layout)?.bytes)),
            Mode::CompactTile | Mode::CompactBvh => Some(Arc::new(compact::build(
                data,
                &layout,
                settings.mode == Mode::CompactBvh,
            )?)),
            Mode::Scan => {
                let mut scan = data[..32 + 40 * layout.count as usize].to_vec();
                scan[..4].copy_from_slice(b"G2S1");
                scan[12..16].fill(0);
                scan[20..32].fill(0);
                Some(Arc::new(scan))
            }
        };
        Ok(Self {
            id: settings.texture,
            blob,
            atlas: Default::default(),
        })
    }
}
impl Technique for Gaussian {
    fn build(&self, s: &Snapshot) -> Result<FrameGraph> {
        let payloads = self
            .blob
            .as_ref()
            .map(|v| (self.id, v.as_slice()))
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let atlas = self
            .atlas
            .get(&s.scene, &payloads, "Gaussian material atlas")?;
        Forward::default().build_with_atlas(s, atlas)
    }
}
pub fn command(engine: &mut Engine, arg: &str) -> Result<String> {
    let (id, mut settings) = Settings::read(&engine.world)?;
    if let Some(path) = arg.strip_prefix("load ") {
        let buf = std::fs::read(path.trim().trim_matches('"'))?;
        let new = Layout::read(&buf)?;
        let old = Layout::read(Gaussian::payload(&engine.world, settings.texture)?)?;
        ensure!(
            (new.width, new.height) == (old.width, old.height),
            "replacement must match the texture extent"
        );
        let mut world = (*engine.world).clone();
        world
            .entities
            .get_mut(&settings.texture)
            .context("missing texture")?
            .gaussian_payload = Some(Arc::new(buf));
        settings.mode = Mode::Tile;
        settings.store(&mut world, id)?;
        let technique = Gaussian::new(&world)?;
        engine.world = Arc::new(world);
        engine.technique = Arc::new(technique);
        return Ok(format!(
            "Loaded {} Gaussians, {}px tiles",
            new.count, new.size
        ));
    }
    settings.mode = match arg {
        "tile" => Mode::Tile,
        "bvh" => Mode::Bvh,
        "compact-tile" => Mode::CompactTile,
        "compact-bvh" => Mode::CompactBvh,
        "scan" => Mode::Scan,
        "original" if !settings.preview => Mode::Original,
        "original" => bail!("external G2T contains no original image"),
        "stats" => {
            let buf = Gaussian::payload(&engine.world, settings.texture)?;
            let l = Layout::read(buf)?;
            if matches!(settings.mode, Mode::CompactTile | Mode::CompactBvh) {
                let bvh = settings.mode == Mode::CompactBvh;
                let blob = compact::build(buf, &l, bvh)?;
                let word =
                    |i: usize| u32::from_le_bytes(blob[i * 4..i * 4 + 4].try_into().unwrap());
                let records = word(4) as usize * 16;
                return Ok(format!(
                    "{} · {} records ({} B) · {} {} ({} B index) + 48 B header = {} B",
                    settings.mode.program(),
                    word(4),
                    records,
                    word(5),
                    if bvh { "nodes" } else { "ID runs" },
                    blob.len() - 48 - records,
                    blob.len()
                ));
            }
            let tree = bvh::Tree::build(buf, &l)?;
            return Ok(format!(
                "{} × {} · {} Gaussians · {}px tiles ({:.1} refs/tile): {} B · BVH {} records + {} nodes ({} B nodes): {} B",
                l.width,
                l.height,
                l.count,
                l.size,
                l.refs as f64 / l.tiles as f64,
                buf.len(),
                tree.records,
                tree.nodes,
                tree.nodes * 16,
                tree.bytes.len()
            ));
        }
        _ => bail!(
            "gaussian tile/bvh/compact-tile/compact-bvh/scan/original/stats or gaussian load PATH.g2t"
        ),
    };
    let technique = Gaussian::for_settings(&engine.world, &settings)?;
    settings.store(Arc::make_mut(&mut engine.world), id)?;
    engine.technique = Arc::new(technique);
    Ok(format!("2DGaussian · {arg}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rhi::Backend;
    #[test]
    fn rejects_truncated_tables_and_invalid_gaussian_ids() {
        assert!(Layout::read(&DEMO[..DEMO.len() - 4]).is_err());
        let mut bad = DEMO.to_vec();
        let ids = u32::from_le_bytes(bad[28..32].try_into().unwrap()) as usize;
        bad[ids * 4..ids * 4 + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(Layout::read(&bad).is_err());
    }

    #[test]
    #[ignore = "requires Modern Hall and Vulkan"]
    fn compact_samplers_decode_every_pruned_texel() -> Result<()> {
        use crate::rdg::{Access, Dispatch, Op, Shader, Storage};
        let source = include_bytes!("../../assets/gaussian/FloorTiles-pruned.g2t");
        let layout = Layout::read(source)?;
        let world = scene(None)?;
        let texture = Settings::read(&world)?.1.texture;
        let reference = &world.entities[&texture].texture.as_ref().unwrap().rgba;
        let mut gpu = Backend::open()?;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        let mut decode = |payload, code| -> Result<_> {
            let mut g = FrameGraph::new([layout.width, layout.height]);
            let atlas = g.input("Gaussian payload", payload);
            let output = g.resource(
                "Decoded texels",
                Storage::Buffer {
                    bytes: reference.len(),
                    initial: None,
                },
            );
            g.pass(
                "Decode texel centers",
                Op::Compute {
                    program: Dispatch {
                        shader: Shader::Compute(code),
                        bindings: vec![
                            (atlas, Access::ComputeRead),
                            (output, Access::ComputeWrite),
                        ],
                        push: vec![],
                    },
                    groups: [layout.width.div_ceil(8), layout.height.div_ceil(8), 1],
                },
            );
            g.set_output(output);
            Ok(gpu.execute(g.compile()?, &pool)?.pixels)
        };
        let original = decode(
            DEMO.to_vec(),
            include_bytes!(concat!(env!("OUT_DIR"), "/gaussian_probe_fp32.comp.spv")),
        )?;
        let tile = decode(
            compact::build(source, &layout, false)?,
            include_bytes!(concat!(env!("OUT_DIR"), "/gaussian_probe_tile.comp.spv")),
        )?;
        let bvh = decode(
            compact::build(source, &layout, true)?,
            include_bytes!(concat!(env!("OUT_DIR"), "/gaussian_probe_bvh.comp.spv")),
        )?;
        assert_eq!(tile.len(), reference.len());
        assert_eq!(tile.len(), bvh.len());
        assert!(tile.iter().zip(&bvh).all(|(a, b)| a.abs_diff(*b) <= 1));
        let differs = tile
            .chunks_exact(4)
            .zip(bvh.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        let psnr = |image: &[u8]| {
            let sse = image
                .chunks_exact(4)
                .zip(reference.chunks_exact(4))
                .map(|(a, b)| {
                    (0..3)
                        .map(|i| u64::from(a[i].abs_diff(b[i])).pow(2))
                        .sum::<u64>()
                })
                .sum::<u64>();
            let n = f64::from(layout.width) * f64::from(layout.height);
            10. * (255f64.powi(2) * 3. * n / sse as f64).log10()
        };
        let baseline = psnr(&original);
        assert!(baseline > 30.6);
        for (name, image) in [("tile", tile), ("BVH", bvh)] {
            let db = psnr(&image);
            println!(
                "Pruned compact {name}: {db:.5} dB vs source RGB8, original FP32 {baseline:.5} dB, {differs} differing texels between indices"
            );
            assert!(db >= baseline - 0.1);
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires Modern Hall and Vulkan"]
    fn indexed_floor_matches_full_scan_in_material_resolve() -> Result<()> {
        let mut w = scene(None)?;
        let (id, mut cfg) = Settings::read(&w)?;
        let mut materials = MaterialPrograms::default();
        register(&mut materials)?;
        let mut gpu = Backend::open()?;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        let shared = Arc::new(materials);
        let mut draw = |mode| -> Result<_> {
            cfg.mode = mode;
            cfg.store(&mut w, id)?;
            let s = Snapshot::new(&w, camera(&w)?, [321, 201], shared.clone())?;
            let graph = Gaussian::new(&w)?.build(&s)?;
            gpu.execute(graph.compile()?, &pool)
        };
        let tile = draw(Mode::Tile)?;
        let bvh = draw(Mode::Bvh)?;
        let compact_tile = draw(Mode::CompactTile)?;
        let compact_bvh = draw(Mode::CompactBvh)?;
        let scan = draw(Mode::Scan)?;
        let original = draw(Mode::Original)?;
        assert!(tile.counters[TILE] > 1000);
        assert_eq!(tile.counters[TILE], scan.counters[SCAN]);
        assert_eq!(tile.counters[TILE], bvh.counters[BVH]);
        assert_eq!(tile.counters[TILE], compact_tile.counters[COMPACT_TILE]);
        assert_eq!(tile.counters[TILE], compact_bvh.counters[COMPACT_BVH]);
        assert_eq!(tile.counters[TILE], original.counters[ORIGINAL]);
        assert_eq!(tile.pixels.as_slice(), scan.pixels.as_slice());
        // Spatial leaf order can round floating-point additions differently.
        assert!(
            bvh.pixels
                .iter()
                .zip(&scan.pixels)
                .all(|(a, b)| a.abs_diff(*b) <= 1)
        );
        let layout = Layout::read(DEMO)?;
        let rest = original.memory.atlas_buffer_bytes
            - 4 * u64::from(layout.width) * u64::from(layout.height);
        assert_eq!(tile.memory.atlas_buffer_bytes, rest + DEMO.len() as u64);
        assert!(
            compact_tile
                .pixels
                .iter()
                .zip(&compact_bvh.pixels)
                .all(|(a, b)| a.abs_diff(*b) <= 1)
        );
        assert!(
            compact_tile
                .pixels
                .iter()
                .zip(&tile.pixels)
                .all(|(a, b)| a.abs_diff(*b) <= 4)
        );
        assert_eq!(
            compact_tile.memory.atlas_buffer_bytes,
            rest + compact::build(DEMO, &layout, false)?.len() as u64
        );
        assert_eq!(
            compact_bvh.memory.atlas_buffer_bytes,
            rest + compact::build(DEMO, &layout, true)?.len() as u64
        );
        assert_eq!(
            bvh.memory.atlas_buffer_bytes,
            rest + bvh::Tree::build(DEMO, &layout)?.bytes.len() as u64
        );
        assert_eq!(
            scan.memory.atlas_buffer_bytes,
            rest + 32 + 40 * u64::from(layout.count)
        );
        for frame in [&tile, &bvh, &scan, &original] {
            assert!(
                frame.memory.atlas.as_ref().unwrap().allocated_bytes
                    >= frame.memory.atlas_buffer_bytes
            );
        }
        let sse: u64 = tile
            .pixels
            .iter()
            .zip(original.pixels)
            .map(|(a, b)| u64::from(a.abs_diff(b)).pow(2))
            .sum();
        assert!(sse > 0 && sse < tile.counters[TILE] * 3 * 100);
        Ok(())
    }
}
