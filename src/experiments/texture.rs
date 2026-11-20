//! Random-access neural base colors in the ordinary visibility/PBR material pass.
use crate::{
    engine::Engine,
    import,
    rdg::Graph,
    render::{
        Atlas, Camera, Forward, MaterialPrograms, Snapshot, Technique, build_frame_with_post,
    },
    scene::{Entity, Id, Shading, World},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};

pub const SETTING: &str = "lab.texture_compression";
const NEURAL: &str = "Neural base color";
const ORIGINAL: &str = "Original base color";
const ASSETS: [(&str, &[u8]); 3] = [
    (
        "Wood",
        include_bytes!("../../assets/texture/modern-hall/Wood.fgm"),
    ),
    (
        "FloorTiles",
        include_bytes!("../../assets/texture/modern-hall/FloorTiles.fgm"),
    ),
    (
        "Wallpaper",
        include_bytes!("../../assets/texture/modern-hall/Wallpaper.fgm"),
    ),
];
const PREFIX_BYTES: usize = 16 + 2 * (16 * 8 + 16 + 48 * 16 + 48);

#[derive(Clone, Serialize, Deserialize)]
struct Settings {
    textures: [Id; 3],
    neural: bool,
}
impl Settings {
    fn read(w: &World) -> Result<(Id, Self)> {
        let (id, raw) = w
            .entities
            .iter()
            .find_map(|(&id, e)| Some((id, e.components.get(SETTING)?)))
            .context("open the texture experiment first")?;
        Ok((id, serde_json::from_value(raw.clone())?))
    }
    fn bind(&self, w: &mut World) {
        let program = if self.neural { NEURAL } else { ORIGINAL };
        for material in w.entities.values_mut().filter_map(|e| e.material.as_mut()) {
            if material
                .texture
                .is_some_and(|id| self.textures.contains(&id))
            {
                material.shading = Shading::Custom(program.into());
            }
        }
    }
}

pub fn scene() -> Result<World> {
    let path = Path::new("assets/scenes/modern-hall/modern_hall_core.gltf");
    let mut w = World::default();
    import::import(&mut w, path)?;
    let mut textures = [0; 3];
    for (id, (name, _)) in textures.iter_mut().zip(ASSETS) {
        *id = w
            .entities
            .values()
            .find_map(|e| {
                if e.name == name {
                    e.material.as_ref()?.texture
                } else {
                    None
                }
            })
            .with_context(|| format!("missing Modern Hall material {name}"))?;
    }
    let settings = Settings {
        textures,
        neural: true,
    };
    settings.bind(&mut w);
    w.spawn(Entity {
        name: "Modern Hall texture compression".into(),
        components: BTreeMap::from([(SETTING.into(), serde_json::to_value(settings)?)]),
        ..Default::default()
    });
    Ok(w)
}

pub fn camera(w: &World) -> Result<Camera> {
    let id = w
        .entities
        .iter()
        .find_map(|(&id, e)| (e.name == "Camera").then_some(id))
        .context("missing Modern Hall camera")?;
    let transform = w.transforms()?[&id];
    let back = transform.z_axis.truncate().normalize();
    let distance = 4.;
    Ok(Camera {
        target: transform.w_axis.truncate() - distance * back,
        distance,
        yaw: back.x.atan2(back.z),
        pitch: back.y.asin(),
    })
}

pub fn register(materials: &mut MaterialPrograms) -> Result<()> {
    for (name, code) in [
        (
            NEURAL,
            include_bytes!(concat!(env!("OUT_DIR"), "/texture_grid.comp.spv")).as_slice(),
        ),
        (
            ORIGINAL,
            include_bytes!(concat!(env!("OUT_DIR"), "/pbr.comp.spv")).as_slice(),
        ),
    ] {
        if materials.bin(&Shading::Custom(name.into())).is_err() {
            materials.register(name, code)?;
        }
    }
    Ok(())
}

pub struct TextureCompression {
    encoded: BTreeMap<Id, &'static [u8]>,
    atlas: MaterialAtlas,
}
impl TextureCompression {
    pub fn new(w: &World) -> Result<Self> {
        let (_, settings) = Settings::read(w)?;
        Self::for_settings(w, &settings)
    }
    fn for_settings(w: &World, settings: &Settings) -> Result<Self> {
        let mut encoded = BTreeMap::new();
        for (id, (name, blob)) in settings.textures.into_iter().zip(ASSETS) {
            let t = w
                .entities
                .get(&id)
                .and_then(|e| e.texture.as_ref())
                .context("missing source texture")?;
            let header = blob.get(..16).context("truncated feature grid")?;
            let word = |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
            let grid_width = t.width.div_ceil(4);
            ensure!(
                &header[..4] == b"FGM1"
                    && [word(1), word(2), word(3)] == [t.width, t.height, grid_width]
                    && blob.len() as u64
                        == PREFIX_BYTES as u64
                            + 8 * u64::from(grid_width) * u64::from(t.height.div_ceil(4)),
                "incompatible feature grid for {name}"
            );
            if settings.neural {
                encoded.insert(id, blob);
            }
        }
        Ok(Self {
            encoded,
            atlas: MaterialAtlas::default(),
        })
    }
}
impl Technique for TextureCompression {
    fn build(&self, s: &Snapshot) -> Result<Graph> {
        self.atlas
            .frame(s, &self.encoded, "Texture feature grids and MLP weights")
    }
}
#[derive(Default)]
pub(crate) struct MaterialAtlas(Mutex<Option<Arc<Atlas>>>);
impl MaterialAtlas {
    pub(crate) fn frame(
        &self,
        s: &Snapshot,
        payloads: &BTreeMap<Id, &[u8]>,
        label: &'static str,
    ) -> Result<Graph> {
        let atlas = {
            let mut cache = self.0.lock().unwrap();
            if !cache.as_ref().is_some_and(|a| a.matches(&s.world)) {
                *cache = Some(Arc::new(Atlas::with_payloads(&s.world, payloads, label)?));
            }
            cache.as_ref().unwrap().clone()
        };
        build_frame_with_post(
            s,
            atlas,
            |g, t, transforms| Forward::default().draw_subset(g, t, s, transforms, |_, _| true),
            |_, _, _, _| Ok(()),
        )
    }
}
pub fn command(engine: &mut Engine, arg: &str) -> Result<String> {
    let (id, mut settings) = Settings::read(&engine.world)?;
    let mut original = 0;
    for texture in settings.textures {
        original += engine
            .world
            .entities
            .get(&texture)
            .and_then(|e| e.texture.as_ref())
            .context("missing source texture")?
            .rgba
            .len();
    }
    let packed: usize = ASSETS.iter().map(|(_, blob)| blob.len()).sum();
    match arg {
        "original" => settings.neural = false,
        "neural" => settings.neural = true,
        "stats" => {
            return Ok(format!(
                "RGBA8 {original} B · {} B grids + {} B FP16 MLPs + {} B headers = {packed} B · {:.2}x",
                packed - PREFIX_BYTES * ASSETS.len(),
                (PREFIX_BYTES - 16) * ASSETS.len(),
                16 * ASSETS.len(),
                original as f64 / packed as f64
            ));
        }
        _ => bail!("texture neural/original/stats"),
    }
    let technique = TextureCompression::for_settings(&engine.world, &settings)?;
    let w = Arc::make_mut(&mut engine.world);
    settings.bind(w);
    w.entities
        .get_mut(&id)
        .unwrap()
        .components
        .insert(SETTING.into(), serde_json::to_value(&settings)?);
    engine.technique = Arc::new(technique);
    Ok(format!("Modern Hall · {arg}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{rdg::Storage, rhi::Backend};

    #[test]
    #[ignore = "requires the Modern Hall glTF and Vulkan"]
    fn neural_material_decodes_only_the_compact_atlas() -> Result<()> {
        let mut w = scene()?;
        let (id, mut settings) = Settings::read(&w)?;
        let mut materials = MaterialPrograms::default();
        register(&mut materials)?;
        let mut snapshot = Snapshot {
            camera: camera(&w)?,
            materials: Arc::new(materials),
            world: Arc::new(w.clone()),
            extent: [320, 200],
        };
        let mut gpu = Backend::open()?;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        let mut render = |neural: bool, prefer_local: bool| -> Result<_> {
            settings.neural = neural;
            settings.bind(&mut w);
            w.entities
                .get_mut(&id)
                .unwrap()
                .components
                .insert(SETTING.into(), serde_json::to_value(&settings)?);
            snapshot.world = Arc::new(w.clone());
            let mut graph = TextureCompression::new(&w)?.build(&snapshot)?;
            if !prefer_local {
                // Exercise the RHI's original placement policy for the same buffer at the same slot.
                graph.material_atlas = None;
            }
            let size = if neural {
                assert!(
                    !graph
                        .resources
                        .iter()
                        .any(|(name, _)| name == "Base color texels")
                );
                ASSETS.iter().map(|(_, bytes)| bytes.len()).sum::<usize>()
            } else {
                settings
                    .textures
                    .iter()
                    .map(|id| w.entities[id].texture.as_ref().unwrap().rgba.len())
                    .sum()
            };
            // The four-byte white texel is shared by constant-color materials.
            let label = if neural {
                "Texture feature grids and MLP weights"
            } else {
                "Base color texels"
            };
            assert!(graph.resources.iter().any(|(name, r)| name == label
                && matches!(r, Storage::Buffer { bytes, .. } if *bytes == size + 4)));
            gpu.execute(graph.compile()?, &pool)
        };
        let legacy = render(false, false)?;
        let original = render(false, true)?;
        let neural = render(true, true)?;
        assert_eq!(legacy.pixels, original.pixels);
        for frame in [&original, &neural] {
            assert!(
                frame.memory.atlas.as_ref().unwrap().allocated_bytes
                    >= frame.memory.atlas_buffer_bytes
            );
        }
        println!(
            "Original placement {} local + {} nonlocal B; same RGBA8 frame with preferred local atlas {} local + {} nonlocal B",
            legacy.memory.device_local_bytes,
            legacy.memory.nonlocal_bytes,
            original.memory.device_local_bytes,
            original.memory.nonlocal_bytes
        );
        let covered = neural.counters[NEURAL];
        assert!(covered > 1000);
        assert_eq!(covered, original.counters[ORIGINAL]);
        assert_eq!(neural.counters[ORIGINAL], 0);
        let sse: u64 = original
            .pixels
            .iter()
            .zip(&neural.pixels)
            .map(|(a, b)| u64::from(a.abs_diff(*b)).pow(2))
            .sum();
        assert!(
            sse > 0 && sse < 3 * covered * 16,
            "visible RGB RMSE exceeded 4 levels: {sse}"
        );
        Ok(())
    }
}
