//! Silver Wolf material and silhouette experiments.
use crate::experiments::assembly::{Primary, Registration as SceneModule};
pub(super) const MODULE: SceneModule = SceneModule {
    key: SETTING,
    primary: Primary::Visibility,
    restore: None,
    install: |world, a| {
        Cartoon::register(&mut a.materials)?;
        let moving = world
            .first_raw_component(super::motion::silver::PLAYER)
            .is_some();
        let mut c = camera(world, if moving { "full" } else { "face" })?;
        if moving {
            c.distance *= 1.72;
            c.yaw = 0.62;
            c.pitch = 0.4;
            c.target.y -= 0.24;
        }
        a.camera = Some(c);
        a.visibility.cartoon = true;
        Ok(())
    },
};
mod renderer;
use crate::{engine::Engine, import, render::Camera, scene::*};
use anyhow::{Context, Result, bail, ensure};
use glam::{Vec2, Vec3};
pub use renderer::Cartoon;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, sync::Arc};

pub const SETTING: &str = "lab.cartoon";
pub(super) const SNAPSHOT: crate::render::SnapshotComponent = crate::render::SnapshotComponent {
    extract: Some(|context| {
        let head = context
            .raw()
            .get("head")
            .and_then(serde_json::Value::as_u64);
        Ok(head
            .and_then(|id| context.transform(id))
            .map(|m| Arc::new(m) as crate::render::SnapshotValue))
    }),
    ..crate::render::SnapshotComponent::setting(SETTING)
};
const PROGRAM: &str = "Character cel pixels";
const SDF_SIZE: u32 = 128;

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Outline {
    Off,
    Image,
    Hull,
    Quad,
}

#[derive(Clone, Serialize, Deserialize)]
struct Settings {
    outline: Outline,
    width: f32,
    azimuth: f32,
    threshold: f32,
    face_shadow: bool,
    head: Id,
    face_material: Id,
    sdf: Id,
}
impl Settings {
    fn read(world: &World) -> Result<(Id, Self)> {
        let (id, value) = world
            .first_raw_component(SETTING)
            .context("open the toon experiment first")?;
        Ok((id, Self::decode(value)?))
    }

    fn decode(value: &serde_json::Value) -> Result<Self> {
        let settings = Self::deserialize(value)?;
        settings.validate()?;
        Ok(settings)
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            (0.25..=8.).contains(&self.width),
            "outline width must be 0.25–8 pixels"
        );
        ensure!(
            (-1.0..=1.).contains(&self.threshold),
            "cel threshold must be between -1 and 1"
        );
        ensure!(self.azimuth.is_finite(), "light angle must be finite");
        Ok(())
    }
}

/// Generate signed Euclidean distances to seven cheek/nose shadow contours
/// in the character's face UV island. Positive distance denotes the lit side.
fn face_sdf() -> Texture {
    let phases = [-0.16, 0.10, 0.29, 0.50, 0.71, 0.90, 1.16];
    let profile = [
        (0., 0.),
        (0.29, -0.02),
        (0.38, 0.02),
        (0.445, 0.04),
        (0.469, -0.07),
        (0.49, 0.015),
        (0.57, 0.045),
        (0.67, 0.02),
        (0.79, -0.04),
        (1., 0.),
    ];
    let mut rgba = vec![0; (7 * SDF_SIZE * SDF_SIZE * 4) as usize];
    for (tile, center) in phases.iter().enumerate() {
        let shape = (std::f32::consts::PI * tile as f32 / 6.).sin();
        let contour = profile.map(|(v, offset)| Vec2::new(center + shape * offset, v));
        for y in 0..SDF_SIZE {
            let v = (y as f32 + 0.5) / SDF_SIZE as f32;
            let edge = contour.windows(2).find(|s| v <= s[1].y).unwrap();
            let boundary = edge[0]
                .lerp(edge[1], (v - edge[0].y) / (edge[1].y - edge[0].y))
                .x;
            for x in 0..SDF_SIZE {
                let uv = Vec2::new((x as f32 + 0.5) / SDF_SIZE as f32, v);
                let distance = contour
                    .windows(2)
                    .map(|s| {
                        let line = s[1] - s[0];
                        let t = ((uv - s[0]).dot(line) / line.length_squared()).clamp(0., 1.);
                        uv.distance(s[0] + line * t)
                    })
                    .fold(f32::INFINITY, f32::min);
                let value = ((0.5 + (uv.x - boundary).signum() * distance).clamp(0., 1.) * 255.)
                    .round() as u8;
                let at = 4 * ((y * 7 + tile as u32) * SDF_SIZE + x) as usize;
                rgba[at..at + 4].copy_from_slice(&[value, value, value, 255]);
            }
        }
    }
    Texture {
        width: 7 * SDF_SIZE,
        height: SDF_SIZE,
        rgba,
    }
}

/// UV and normal seams can split one geometric edge into several index
/// edges. Weld positions only when their skinning influences also agree.
fn silhouette_edges(mesh: &Mesh) -> Vec<Edge> {
    let mut shared = BTreeMap::new();
    let canonical: Vec<_> = mesh
        .positions
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let joints = mesh.joints.get(i).copied().unwrap_or_default();
            let weights = mesh.weights.get(i).copied().unwrap_or_default();
            let mut influences = std::array::from_fn::<_, 4, _>(|j| {
                if weights[j] == 0. {
                    (0, 0)
                } else {
                    (joints[j], weights[j].to_bits())
                }
            });
            influences.sort_unstable();
            *shared
                .entry((p.map(|v| if v == 0. { 0 } else { v.to_bits() }), influences))
                .or_insert(i as u32)
        })
        .collect();
    let mut edges = BTreeMap::<[u32; 2], Vec<u32>>::new();
    for (face, tri) in mesh.indices.chunks_exact(3).enumerate() {
        let v = [
            canonical[tri[0] as usize],
            canonical[tri[1] as usize],
            canonical[tri[2] as usize],
        ];
        if v[0] == v[1] || v[1] == v[2] || v[2] == v[0] {
            continue;
        }
        for j in 0..3 {
            let mut ends = [v[j], v[(j + 1) % 3]];
            ends.sort_unstable();
            edges.entry(ends).or_default().push(face as u32);
        }
    }
    edges
        .into_iter()
        .map(|(vertices, faces)| Edge { vertices, faces })
        .collect()
}

pub fn scene() -> Result<World> {
    from_gltf(Path::new("assets/characters/silver-wolf/scene.gltf"))
}
pub fn from_gltf(path: &Path) -> Result<World> {
    let mut w = World::default();
    import::import(&mut w, path)?;
    let head = w
        .entities
        .iter()
        .find(|(_, e)| e.name == "Head_08")
        .context("missing Silver Wolf head joint")?
        .0
        .to_owned();
    let face_material = w
        .entities
        .iter()
        .find(|(_, e)| e.material.is_some() && e.name == "material_5")
        .context("missing Silver Wolf face material")?
        .0
        .to_owned();
    for e in w.entities.values_mut() {
        if let Some(m) = &mut e.material {
            m.shading = Shading::Custom(PROGRAM.into());
        }
        if let Some(mesh) = &e.mesh {
            e.topology = Some(Arc::new(silhouette_edges(mesh)));
        }
    }
    let sdf = w.spawn(Entity {
        name: "Silver Wolf face shadow distance fields".into(),
        texture: Some(Arc::new(face_sdf())),
        ..Default::default()
    });
    let settings = Settings {
        outline: Outline::Image,
        width: 1.5,
        azimuth: 80.,
        threshold: 0.16,
        face_shadow: true,
        head,
        face_material,
        sdf,
    };
    w.spawn(Entity {
        name: "Cartoon experiment".into(),
        components: BTreeMap::from([(SETTING.into(), serde_json::to_value(settings)?)]),
        ..Default::default()
    });
    w.validate()?;
    Ok(w)
}

pub fn camera(world: &World, view: &str) -> Result<Camera> {
    match view {
        "full" => {
            let mut c = Camera::fit(world)?;
            c.distance *= 0.74;
            Ok(Camera {
                yaw: 0.04,
                pitch: 0.01,
                ..c
            })
        }
        "face" => {
            let (_, settings) = Settings::read(world)?;
            let transforms = world.transforms()?;
            let head = transforms
                .get(&settings.head)
                .context("missing head joint")?;
            Ok(Camera {
                target: head.transform_point3(Vec3::new(0., 0.065, 0.025)),
                distance: 0.56 * head.transform_vector3(Vec3::Y).length(),
                yaw: 0.04,
                pitch: 0.015,
            })
        }
        _ => bail!("toon view face|full"),
    }
}

pub fn command(engine: &mut Engine, arg: &str) -> Result<String> {
    let (id, mut settings) = Settings::read(&engine.world)?;
    let words: Vec<_> = arg.split_whitespace().collect();
    match words.as_slice() {
        ["outline", mode] => {
            settings.outline = match *mode {
                "off" => Outline::Off,
                "image" => Outline::Image,
                "hull" => Outline::Hull,
                "quad" => Outline::Quad,
                _ => bail!("toon outline off|image|hull|quad"),
            }
        }
        ["width", n] => settings.width = n.parse()?,
        ["light", n] => settings.azimuth = n.parse::<f32>()? % 360.,
        ["threshold", n] => settings.threshold = n.parse()?,
        ["sdf", "on"] => settings.face_shadow = true,
        ["sdf", "off"] => settings.face_shadow = false,
        ["view", view] => {
            engine.camera = camera(&engine.world, view)?;
            return Ok(format!("toon view {view}"));
        }
        _ => bail!(
            "toon outline off|image|hull|quad · width PX · light DEGREES · threshold [-1,1] · sdf on|off · view face|full"
        ),
    }
    settings.validate()?;
    Arc::make_mut(&mut engine.world)
        .entities
        .get_mut(&id)
        .unwrap()
        .components
        .insert(SETTING.into(), serde_json::to_value(settings)?);
    Ok(format!("toon {arg}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silhouette_adjacency_welds_uv_seams_but_respects_skinning() {
        let mut mesh = Mesh {
            positions: vec![
                [0., 0., 0.],
                [1., 0., 0.],
                [0., 1., 0.],
                [1., 0., 0.],
                [1., 1., 0.],
                [0., 1., 0.],
            ],
            indices: vec![0, 1, 2, 3, 4, 5],
            uv: vec![],
            normals: vec![],
            joints: vec![[0; 4]; 6],
            weights: vec![[1., 0., 0., 0.]; 6],
        };
        let edges = silhouette_edges(&mesh);
        assert_eq!(edges.len(), 5);
        assert_eq!(
            edges.iter().find(|e| e.faces.len() == 2).unwrap().faces,
            [0, 1]
        );
        mesh.joints[3][0] = 1;
        assert!(silhouette_edges(&mesh).iter().all(|e| e.faces.len() == 1));
    }

    #[test]
    #[ignore = "requires the Silver Wolf glTF and Vulkan mesh shaders"]
    fn character_techniques_render_distinct_outlines_and_local_face_shadows() -> Result<()> {
        use crate::{
            render::{MaterialPrograms, SceneExtractor, Technique},
            rhi::Backend,
        };
        let mut world = scene()?;
        let (settings_id, mut settings) = Settings::read(&world)?;
        let mut registry = MaterialPrograms::default();
        Cartoon::register(&mut registry)?;
        let camera = camera(&world, "face")?;
        let materials = Arc::new(registry);
        let mut extractor = SceneExtractor::default();
        let renderer = Cartoon::default();
        let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build()?;
        let mut gpu = Backend::open()?;
        let mut draw = |cfg: &Settings| -> Result<_> {
            world
                .entities
                .get_mut(&settings_id)
                .unwrap()
                .components
                .insert(SETTING.into(), serde_json::to_value(cfg)?);
            let shot = extractor.snapshot(&world, camera, [384, 256], materials.clone())?;
            gpu.execute(renderer.build(&shot)?.compile()?, &pool)
        };
        let changed = |a: &[u8], b: &[u8]| {
            a.chunks_exact(4)
                .zip(b.chunks_exact(4))
                .filter(|(x, y)| x != y)
                .count()
        };
        settings.outline = Outline::Off;
        let base = draw(&settings)?;
        assert!(base.counters[PROGRAM] > 1000);
        let mut images = vec![base.pixels.clone()];
        for (mode, pass) in [
            (Outline::Image, "Image-space outlines"),
            (Outline::Hull, "Normal-extruded backfaces"),
            (Outline::Quad, "Degenerate edge quads"),
        ] {
            settings.outline = mode;
            let frame = draw(&settings)?;
            assert!(frame.passes.iter().any(|p| p.name == pass));
            assert!(
                images
                    .iter()
                    .all(|other| changed(other, &frame.pixels) > 100),
                "{pass}"
            );
            images.push(frame.pixels);
        }
        settings.outline = Outline::Off;
        settings.face_shadow = false;
        let ordinary = draw(&settings)?;
        let diff = changed(&base.pixels, &ordinary.pixels);
        assert!(
            (100..base.counters[PROGRAM] as usize / 3).contains(&diff),
            "face region changed {diff} pixels"
        );
        settings.face_shadow = true;
        settings.azimuth = -settings.azimuth;
        let opposite = draw(&settings)?;
        assert!(changed(&base.pixels, &opposite.pixels) > diff);
        settings.threshold = -1.;
        assert!(changed(&draw(&settings)?.pixels, &opposite.pixels) > 100);
        Ok(())
    }
}
