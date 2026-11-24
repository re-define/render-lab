//! Seeded foliage, shared plant prototypes and controls for the geometry experiment.
use crate::experiments::assembly::{Primary, Registration as SceneModule};
pub(super) const MODULE: SceneModule = SceneModule {
    key: SETTING,
    primary: Primary::Visibility,
    restore: None,
    install: |world, a| {
        configure(world, &mut a.materials)?;
        a.camera = Some(initial_camera(world)?);
        a.visibility.plants = true;
        Ok(())
    },
};
pub(super) const WIND_SYSTEM: SceneModule = SceneModule {
    key: SETTING,
    primary: Primary::Simulation,
    restore: None,
    install: |world, a| {
        if super::composition::enabled(world, "forest") {
            a.schedule.add_system(std::sync::Arc::new(Wind));
        }
        Ok(())
    },
};
use crate::rdg::PassPhase;
use crate::render::FrameGraph;
mod generate;
mod karst;
mod landscape;
mod river_forest;
use crate::{
    engine::{Engine, SimulationData, SimulationSystem, SystemAccess, SystemContext, SystemOrder},
    experiments::clusters,
    rdg::{Access, Dispatch, Op, Resource, Shader},
    render::{Camera, MaterialPrograms, Snapshot},
    scene::*,
};
use anyhow::{Context, Result, bail, ensure};
use glam::{Mat4, Quat, Vec3};
pub(in crate::experiments) use karst::scene as karst_forest;
pub use river_forest::river_forest;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const SETTING: &str = "lab.vegetation";

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[repr(u32)]
#[serde(rename_all = "lowercase")]
pub(super) enum Mode {
    #[default]
    Auto,
    Triangles,
    Voxels,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum BrickRaster {
    #[default]
    Auto,
    #[serde(alias = "linear")]
    Compute,
    Mesh,
    Compare,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub(super) struct Settings {
    pub time: f32,
    pub wind: f32,
    pub pixels: f32,
    pub mode: Mode,
    #[serde(default)]
    pub raster: BrickRaster,
    pub visualize: bool,
    #[serde(default)]
    pub coarse_shadows: bool,
    #[serde(default)]
    pub mega_shadows: bool,
    #[serde(default)]
    pub rebuild_clusters: bool,
    #[serde(default)]
    pub preset: Preset,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            time: 0.,
            wind: 1.,
            pixels: 1.45,
            mode: Mode::Auto,
            raster: BrickRaster::Auto,
            visualize: false,
            coarse_shadows: false,
            mega_shadows: false,
            rebuild_clusters: false,
            preset: Preset::Meadow,
        }
    }
}

impl Settings {
    pub fn decode(raw: &serde_json::Value) -> Result<Self> {
        let s = Self::deserialize(raw)?;
        ensure!(
            (0. ..=4096.).contains(&s.time)
                && (0. ..=2.).contains(&s.wind)
                && (0.1..=16.).contains(&s.pixels),
            "invalid vegetation time, wind or pixel threshold"
        );
        Ok(s)
    }
}

fn read(world: &World) -> Result<(Id, Settings)> {
    let (id, raw) = world
        .first_raw_component(SETTING)
        .context("open the vegetation experiment first")?;
    Ok((id, Settings::decode(raw)?))
}

pub(super) struct Wind;
impl SimulationSystem for Wind {
    fn order(&self) -> SystemOrder {
        SystemOrder {
            key: Some(SETTING),
            after: &[],
        }
    }
    fn access(&self) -> SystemAccess {
        SystemAccess {
            name: "Vegetation wind",
            reads: &[SimulationData::Component(SETTING)],
            writes: &[SimulationData::Component(SETTING)],
        }
    }
    fn run(&self, cx: &mut SystemContext<'_>, dt: f32) -> Result<()> {
        if dt == 0. {
            return Ok(());
        }
        let (id, raw) = cx
            .first_raw_component(SETTING)?
            .context("open the vegetation experiment first")?;
        let mut settings = Settings::decode(raw)?;
        settings.time = (settings.time + dt).rem_euclid(4096.);
        cx.set_component(id, SETTING, serde_json::to_value(settings)?)
    }
}

const PROGRAM: &str = "Vegetation pixels";
const GROUND: &str = "Meadow ground";
const GI_PROGRAMS: [(&str, &[u8]); 6] = [
    (
        "GI woodland soil",
        include_bytes!(concat!(env!("OUT_DIR"), "/gi_woodland_soil.comp.spv")),
    ),
    (
        "GI limestone",
        include_bytes!(concat!(env!("OUT_DIR"), "/gi_limestone.comp.spv")),
    ),
    (
        "GI foliage",
        include_bytes!(concat!(env!("OUT_DIR"), "/gi_foliage.comp.spv")),
    ),
    (
        "GI bark",
        include_bytes!(concat!(env!("OUT_DIR"), "/gi_bark.comp.spv")),
    ),
    (
        "GI river soil",
        include_bytes!(concat!(env!("OUT_DIR"), "/gi_river_soil.comp.spv")),
    ),
    (
        "GI soil",
        include_bytes!(concat!(env!("OUT_DIR"), "/gi_soil.comp.spv")),
    ),
];
const BARK: &str = "Tree bark";
const SUN_DIRECTION: Vec3 = Vec3::new(0.42, -0.73, -0.55);
pub(super) fn configure(world: &World, registry: &mut MaterialPrograms) -> Result<()> {
    for (name, code) in GI_PROGRAMS {
        if world.entities.values().any(|e| {
            e.material
                .as_ref()
                .is_some_and(|m| matches!(&m.shading, Shading::Custom(k) if k == name))
        }) {
            registry.register_lighting(name, code, crate::render::LightingInput::DIRECT_DIFFUSE)?;
        }
    }
    registry.register_scene_lit(
        PROGRAM,
        include_bytes!(concat!(env!("OUT_DIR"), "/vegetation_material.comp.spv")),
        include_bytes!(concat!(env!("OUT_DIR"), "/vegetation_scene.comp.spv")),
    )?;
    registry.register_scene_lit(
        BARK,
        include_bytes!(concat!(env!("OUT_DIR"), "/vegetation_bark.comp.spv")),
        include_bytes!(concat!(env!("OUT_DIR"), "/vegetation_bark_scene.comp.spv")),
    )?;
    if matches!(read(world)?.1.preset, Preset::River) {
        registry.register_scene_lit(
            river_forest::GROUND,
            include_bytes!(concat!(env!("OUT_DIR"), "/river_ground.comp.spv")),
            include_bytes!(concat!(env!("OUT_DIR"), "/river_ground_scene.comp.spv")),
        )?;
    } else {
        registry.register_scene_lit(
            GROUND,
            include_bytes!(concat!(env!("OUT_DIR"), "/vegetation_ground.comp.spv")),
            include_bytes!(concat!(
                env!("OUT_DIR"),
                "/vegetation_ground_scene.comp.spv"
            )),
        )?;
    }
    Ok(())
}

pub(super) fn camera(preset: &str) -> Result<Camera> {
    if matches!(preset, "river" | "basin" | "grass" | "glade") {
        return Ok(river_forest::camera(preset));
    }
    let (target, distance, yaw, pitch) = match preset {
        "overview" => ([0., 3.2, -8.], 32., 0.035, 0.13),
        "near" => ([-4.6, 4.1, 6.], 13.4, 0.30, 0.20),
        "far" => ([0., 0.9, -43.], 113., 0.11, 0.42),
        "forest" => ([0., 4., -173.], 272., 0.12, 0.54),
        "karst" => ([-1., 13., -33.], 88., 0.09, 0.65),
        "karst-grove" => ([1., 12., -39.], 65., -0.52, 0.31),
        _ => bail!(
            "vegetation camera overview/near/far/forest/river/basin/grass/glade/karst/karst-grove"
        ),
    };
    Ok(Camera {
        target: target.into(),
        distance,
        yaw,
        pitch,
    })
}

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Preset {
    #[default]
    Meadow,
    Forest,
    River,
    Karst,
}

pub(super) fn initial_camera(world: &World) -> Result<Camera> {
    camera(match read(world)?.1.preset {
        Preset::Meadow => "overview",
        Preset::Forest => "forest",
        Preset::River => "river",
        Preset::Karst => "karst",
    })
}

pub(super) fn atmosphere(
    graph: &mut FrameGraph,
    snapshot: &Snapshot,
    ids: Resource,
    depth: Resource,
    color: Resource,
    debug: bool,
) {
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct Atmosphere {
        inverse_vp: [f32; 16],
        eye: [f32; 4],
        sun: [f32; 4],
        params: [u32; 4],
        vp: [f32; 16],
    }
    let [width, height] = snapshot.extent;
    let sun = snapshot
        .scene
        .lights()
        .iter()
        .filter(|light| light.kind == LightKind::Directional)
        .map(|light| {
            (
                -light.direction,
                light.radiance.iter().sum::<f32>() * (1. - light.sky_fraction),
            )
        })
        .max_by(|a, b| a.1.total_cmp(&b.1));
    let (sun_direction, strength) = sun.unwrap_or((
        -SUN_DIRECTION.normalize(),
        if snapshot.scene.lights.present {
            0.
        } else {
            3.
        },
    ));
    let preset = snapshot
        .scene
        .setting(SETTING)
        .and_then(|s| s.get("preset"))
        .and_then(|p| p.as_str());
    let view = Atmosphere {
        inverse_vp: snapshot.view_projection().inverse().to_cols_array(),
        eye: snapshot.camera.eye().extend(0.).into(),
        sun: sun_direction.extend((strength / 3.).min(1.)).into(),
        params: [
            width,
            height,
            u32::from(debug),
            match preset {
                Some("river") => 1,
                Some("karst") => 2,
                _ => 0,
            },
        ],
        vp: snapshot.view_projection().to_cols_array(),
    };
    let view = graph.input(
        "Vegetation atmosphere view",
        bytemuck::bytes_of(&view).to_vec(),
    );
    graph.pass_in(
        PassPhase::Lighting,
        "Vegetation atmosphere",
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/vegetation_sky.comp.spv"
                ))),
                bindings: vec![
                    (ids, Access::ComputeSample),
                    (depth, Access::ComputeSample),
                    (color, Access::ComputeReadWrite),
                    (view, Access::ComputeRead),
                ],
                push: vec![],
            },
            groups: [width.div_ceil(8), height.div_ceil(8), 1],
        },
    );
}

pub fn command(engine: &mut Engine, args: &str) -> Result<String> {
    let (id, mut s) = read(&engine.world)?;
    match args.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["auto"] => s.mode = Mode::Auto,
        ["triangles"] => s.mode = Mode::Triangles,
        ["voxels"] => s.mode = Mode::Voxels,
        ["raster", mode] => {
            s.raster = match *mode {
                "auto" => BrickRaster::Auto,
                "compute" | "linear" => BrickRaster::Compute,
                "mesh" => BrickRaster::Mesh,
                "compare" => BrickRaster::Compare,
                _ => bail!("vegetation raster auto/compute/mesh/compare"),
            }
        }
        ["pixels", value] => s.pixels = value.parse()?,
        ["cache", mode @ ("on" | "off")] => s.rebuild_clusters = *mode == "off",
        ["shadows", mode @ ("mega" | "adaptive" | "coarse")] => {
            s.coarse_shadows = *mode == "coarse";
            if *mode == "mega" {
                anyhow::ensure!(
                    engine.ray_caps.clusters && engine.ray_caps.pipeline,
                    "Mega Geometry requires VK_NV_cluster_acceleration_structure and a ray pipeline"
                );
                if let Some((_, raw)) = engine.world.first_raw_component(super::restir::SETTING) {
                    super::restir::check_cluster_mode(raw, engine.ray_caps)?;
                }
            }
            s.mega_shadows = *mode == "mega";
        }
        ["wind", value] => s.wind = value.parse()?,
        ["time", value] => s.time = value.parse()?,
        ["view", "paths"] => s.visualize = true,
        ["view", "beauty"] => s.visualize = false,
        ["camera", preset] => {
            engine.camera = camera(preset)?;
            engine.post.reset();
            return Ok(format!("vegetation {args}"));
        }
        _ => bail!(
            "vegetation auto/triangles/voxels · raster auto/compute/linear/mesh/compare · shadows mega/adaptive/coarse · cache on/off · pixels P · wind 0..2 · time S · view paths/beauty · camera overview/near/far/forest/river/basin/grass/glade/karst/karst-grove"
        ),
    }
    let value = serde_json::to_value(s)?;
    Settings::decode(&value)?;
    Arc::make_mut(&mut engine.world).set_setting(id, SETTING, value);
    Ok(format!("vegetation {args}"))
}

fn material(
    world: &mut World,
    name: &str,
    color: [f32; 3],
    shader: &str,
    texture: Option<Id>,
) -> Id {
    world.spawn(Entity {
        name: name.into(),
        material: Some(Material {
            shading: Shading::Custom(shader.into()),
            base_color: Vec3::from(color).extend(1.).into(),
            roughness: 0.93,
            metallic: 0.,
            texture,
            gi_hit: crate::scene::GiHitMaterial {
                two_sided: shader == PROGRAM,
                model: if shader == PROGRAM {
                    GiHitModel::ThinFoliage
                } else {
                    GiHitModel::Diffuse
                },
                ..Default::default()
            },
        }),
        ..Default::default()
    })
}

fn mesh(world: &mut World, name: &str, geometry: Mesh, plant: Option<PlantGeometry>) -> Id {
    world.spawn(Entity {
        name: name.into(),
        mesh: Some(Arc::new(geometry)),
        components: if plant.is_none() {
            [(clusters::ORDINARY.into(), true.into())].into()
        } else {
            Default::default()
        },
        plant: plant.map(Arc::new),
        ..Default::default()
    })
}

fn spawn(world: &mut World, name: &str, parts: &[(Id, Id)], transform: Mat4) {
    for &(mesh, material) in parts {
        world.spawn(Entity {
            name: name.into(),
            local: transform,
            instance: Some(Instance::new(mesh, Some(material))),
            ..Default::default()
        });
    }
}

pub fn scene() -> Result<World> {
    generate_scene(Preset::Meadow)
}

pub fn forest() -> Result<World> {
    generate_scene(Preset::Forest)
}

type Part = (Id, Id);
fn tree_prototypes(world: &mut World) -> Vec<[Part; 2]> {
    let mut trees = vec![];
    use generate::Species::*;
    for (variant, (species, name, color, bark_color)) in [
        (
            Spruce,
            "Spruce",
            [0.096, 0.184, 0.118],
            [0.17, 0.113, 0.077],
        ),
        (Pine, "Pine", [0.124, 0.223, 0.124], [0.265, 0.148, 0.088]),
        (Birch, "Birch", [0.204, 0.306, 0.128], [0.71, 0.68, 0.59]),
        (Oak, "Oak", [0.151, 0.256, 0.101], [0.183, 0.139, 0.097]),
    ]
    .into_iter()
    .enumerate()
    {
        let (trunk, leaves, bones) = generate::tree(0x71b9 + variant as u32 * 157, species);
        let trunk_rig = generate::rig(&trunk, &bones, None);
        let leaf_rig = generate::rig(
            &leaves,
            &bones,
            Some(if species.broadleaf() { 0.62 } else { 0.56 }),
        );
        let bark = material(world, "Bark", bark_color, BARK, None);
        let foliage = material(world, name, color, PROGRAM, None);
        trees.push([
            (
                mesh(world, &format!("{name} branches"), trunk, Some(trunk_rig)),
                bark,
            ),
            (
                mesh(world, &format!("{name} canopy"), leaves, Some(leaf_rig)),
                foliage,
            ),
        ]);
    }
    trees
}

fn prototypes(world: &mut World, river: bool) -> (Vec<[Part; 2]>, Vec<Part>) {
    let trees = tree_prototypes(world);
    let mut grass = vec![];
    let colors = if river {
        [
            [0.097, 0.197, 0.069],
            [0.174, 0.258, 0.083],
            [0.098, 0.212, 0.107],
        ]
    } else {
        [
            [0.14, 0.27, 0.052],
            [0.240, 0.325, 0.064],
            [0.093, 0.233, 0.072],
        ]
    };
    for (variant, color) in colors.into_iter().enumerate() {
        let seed = 0xc78a + variant as u32 * 79;
        let (blades, bones) = if river {
            generate::river_grass(seed, variant)
        } else {
            generate::grass(seed)
        };
        let rig = generate::rig(&blades, &bones, Some(0.29));
        let color = material(world, "Grass blades", color, PROGRAM, None);
        let name = if river {
            [
                "Woodland short grass",
                "Meadow panicle grass",
                "Riverbank sedge",
            ][variant]
        } else {
            "Grass prototype"
        };
        grass.push((mesh(world, name, blades, Some(rig)), color));
    }
    (trees, grass)
}

fn environment(world: &mut World, preset: Preset) -> Result<()> {
    let karst = matches!(preset, Preset::Karst);
    world.spawn(Entity {
        name: "Late afternoon sun".into(),
        local: Light::aiming(if karst {
            Vec3::new(0.87, -0.78, 0.04)
        } else {
            SUN_DIRECTION
        })?,
        light: Some(Light {
            angular_radius_degrees: 0.27,
            ..if karst {
                Light::directional([1., 0.94, 0.80], 1.95)
            } else {
                Light::directional([1., 0.84, 0.63], 1.85)
            }
        }),
        ..Default::default()
    });
    world.spawn(Entity {
        name: "Cool sky fill".into(),
        local: Light::aiming(Vec3::new(-0.4, -0.8, 0.3))?,
        light: Some(Light {
            sky_fraction: 0.78,
            ..Light::directional([0.53, 0.70, 1.], 0.42)
        }),
        ..Default::default()
    });
    world.spawn(Entity {
        name: "Vegetation experiment".into(),
        components: [
            (
                SETTING.into(),
                serde_json::to_value(Settings {
                    preset,
                    ..Default::default()
                })?,
            ),
            (clusters::SETTING.into(), 1.25.into()),
        ]
        .into(),
        ..Default::default()
    });
    Ok(())
}

fn generate_scene(preset: Preset) -> Result<World> {
    let mut world = World::default();
    let forest = matches!(preset, Preset::Forest);
    let (trees, grass) = prototypes(&mut world, false);
    let mut rng = generate::Random::new(0xf82c_98a1);
    let mut crowns = Vec::new();
    for (x, z, variant) in [
        (-4.6, 6., 0),
        (5.6, 3.4, 2),
        (-8.0, -2.6, 1),
        (4.9, -12., 3),
        (-5.5, -15., 2),
    ] {
        crowns.push((place(&mut rng, x, z, [0., 0.], 0.95, 1.18), variant));
    }
    let (tree_cols, tree_rows, spacing) = if forest {
        (51, 53, 7.6)
    } else {
        (17, 15, 7.15)
    };
    for row in 0..tree_rows {
        for column in 0..tree_cols {
            let x = (column as f32 - (tree_cols - 1) as f32 * 0.5) * spacing;
            let z = -18. - row as f32 * spacing;
            let model = place(&mut rng, x, z, [3.15, 3.0], 0.66, 1.31);
            let p = model.w_axis;
            if (p.x - landscape::trail(p.z)).abs() < 2.9
                || (!forest && rng.next() < 0.17 + 0.13 * (0.18 * x + 0.09 * z).sin())
            {
                continue;
            }
            let group = (0.041 * x - 0.11 * z).sin();
            let family = if rng.next() + group * 0.31 > 0.52 {
                2
            } else {
                0
            };
            let variant = family + usize::from(rng.next() > 0.53);
            crowns.push((model, variant));
        }
    }
    for &(model, variant) in &crowns {
        spawn(&mut world, "Forest tree", &trees[variant], model);
    }
    let (grass_cols, grass_rows, dx, dz, start_z) = if forest {
        (132, 109, 2.8, 3.9, 18.)
    } else {
        (64, 66, 0.91, 1.03, 22.)
    };
    for row in 0..grass_rows {
        for column in 0..grass_cols {
            let x = (column as f32 - (grass_cols - 1) as f32 * 0.5) * dx;
            let z = start_z - row as f32 * dz;
            let patch = (0.24 * x + 0.13 * z).sin() * (0.17 * z - 0.07 * x).cos();
            let scale = 0.87 + 0.17 * patch;
            let model = place(
                &mut rng,
                x,
                z,
                [dx * 0.66, dz * 0.68],
                0.52 * scale,
                1.4 * scale,
            );
            let p = model.w_axis;
            if (p.x - landscape::trail(p.z)).abs() < 0.86 + 0.34 * patch
                || rng.next() < 0.12 - patch * 0.11
            {
                continue;
            }
            spawn(
                &mut world,
                "Grass tuft",
                &[grass[(row + column * 7) % grass.len()]],
                model,
            );
        }
    }
    let terrain = if forest {
        landscape::Terrain::forest()
    } else {
        landscape::Terrain::meadow()
    };
    let (surface, occlusion) = terrain.build(&crowns);
    let shade = world.spawn(Entity {
        name: "Meadow canopy occlusion".into(),
        texture: Some(Arc::new(occlusion)),
        ..Default::default()
    });
    let floor = mesh(&mut world, "Meadow terrain", surface, None);
    let soil = material(&mut world, "Meadow soil", [1.; 3], GROUND, Some(shade));
    spawn(
        &mut world,
        "Meadow terrain",
        &[(floor, soil)],
        Mat4::IDENTITY,
    );
    environment(&mut world, preset)?;
    clusters::prepare(&mut world)?;
    Ok(world)
}

fn place(rng: &mut generate::Random, x: f32, z: f32, jitter: [f32; 2], min: f32, max: f32) -> Mat4 {
    let x = x + rng.signed() * jitter[0];
    let z = z + rng.signed() * jitter[1];
    Mat4::from_scale_rotation_translation(
        Vec3::splat(min + (max - min) * rng.next()),
        Quat::from_rotation_y(rng.next() * std::f32::consts::TAU),
        Vec3::new(x, landscape::ground(x, z) - 0.032, z),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        render::{SceneExtractor, Technique},
        rhi::Backend,
    };

    #[test]
    fn forest_uses_shared_cluster_assets_and_thousands_of_plants() -> Result<()> {
        let world = forest()?;
        world.validate()?;
        let count = |name| world.entities.values().filter(|e| e.name == name).count();
        assert!(count("Forest tree") / 2 >= 2500);
        assert!(count("Grass tuft") >= 10000);
        assert_eq!(
            world
                .entities
                .values()
                .filter(|e| e.plant.is_some())
                .count(),
            11
        );
        assert!(
            initial_camera(&world)?
                .eye()
                .abs_diff_eq(camera("forest")?.eye(), 0.001)
        );
        assert_eq!(
            world
                .entities
                .values()
                .filter(|e| e.light.is_some())
                .count(),
            2
        );
        Ok(())
    }

    #[test]
    fn generation_round_trips_cluster_assets_and_rigs() -> Result<()> {
        let world = scene()?;
        let mut bytes = vec![];
        serde_json::to_writer(&mut bytes, &world)?;
        let restored: World = serde_json::from_slice(&bytes)?;
        restored.validate()?;
        assert_eq!(bytes, serde_json::to_vec(&restored)?);
        let mut old_rig = serde_json::to_value(
            restored
                .entities
                .values()
                .filter_map(|entity| entity.plant.as_ref())
                .find(|rig| rig.voxel_cell_size.is_some())
                .unwrap(),
        )?;
        let size = old_rig
            .as_object_mut()
            .unwrap()
            .remove("voxel_cell_size")
            .unwrap();
        old_rig["voxels"] = serde_json::json!({"cell_size": size, "occupied": [[0, 0, 0]]});
        let migrated: PlantGeometry = serde_json::from_value(old_rig)?;
        let encoded = serde_json::to_value(migrated)?;
        assert_eq!(encoded["voxel_cell_size"], size);
        assert!(encoded.get("voxels").is_none());
        let assets: Vec<_> = restored
            .entities
            .values()
            .filter(|e| e.plant.is_some())
            .collect();
        assert_eq!(assets.len(), 11);
        assert_eq!(
            assets
                .iter()
                .filter(|e| e.plant.as_ref().unwrap().voxel_cell_size.is_some())
                .count(),
            7
        );
        assert!(
            assets
                .iter()
                .all(|e| e.meshlets.is_some() && e.hierarchy.is_some())
        );
        assert!(
            restored
                .entities
                .values()
                .filter(|e| e.name == "Grass tuft")
                .count()
                > 1800
        );
        assert!(
            restored
                .entities
                .values()
                .filter(|e| e.name == "Forest tree")
                .count()
                / 2
                > 120
        );
        let mut config = MaterialPrograms::default();
        configure(&restored, &mut config)?;
        let view = crate::render::Snapshot::new(
            &restored,
            camera("overview")?,
            [400, 250],
            Arc::new(config),
        )?;
        let plan = clusters::ClusterLod::default().build(&view)?.compile()?;
        let passes = &plan.graph().passes;
        let wind = passes
            .iter()
            .position(|p| p.name == "Vegetation wind palettes")
            .unwrap();
        let frustum = plan
            .steps()
            .iter()
            .find(|s| passes[s.index].name == "GPU instance frustum")
            .unwrap();
        assert!(!frustum.parents.contains(&wind));
        for name in ["Main mesh visibility", "Shade Vegetation pixels"] {
            let step = plan
                .steps()
                .iter()
                .find(|s| passes[s.index].name == name)
                .unwrap();
            assert!(
                step.parents.contains(&wind),
                "{name} must read the wind palette"
            );
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires Vulkan 1.4 and mesh shaders"]
    fn renders_wind_and_voxels_through_the_shared_visibility_buffer() -> Result<()> {
        let mut world = scene()?;
        let (id, mut settings) = read(&world)?;
        let mut materials = MaterialPrograms::default();
        configure(&world, &mut materials)?;
        let materials = Arc::new(materials);
        let mut extraction = SceneExtractor::default();
        let technique = clusters::ClusterLod::default();
        let mut gpu = Backend::open()?;
        let atomic64 = gpu.buffer_atomic64();
        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
        let view = camera("overview")?;
        let mut initialized = false;
        let mut render = |settings: Settings| -> Result<_> {
            world.set_setting(id, SETTING, serde_json::to_value(settings)?);
            let mut snapshot = extraction.snapshot(&world, view, [320, 200], materials.clone())?;
            snapshot.buffer_atomic64 = atomic64;
            if initialized {
                assert!(snapshot.scene.stats.entities_visited <= 1);
                assert_eq!(snapshot.scene.stats.instances_updated, 0);
            }
            initialized = true;
            gpu.execute(technique.build(&snapshot)?.compile()?, &pool)
        };
        settings.pixels = 1.2;
        let auto = render(settings)?;
        assert!(
            auto.counters["Voxel plant clusters"] > 0
                && auto.counters["Triangle plant clusters"] > 0
        );
        assert!(auto.counters["Active wind bones"] > 1000);
        assert!(auto.counters[PROGRAM] > 1000);
        assert!(auto.counters["Plant clusters in transition"] > 0);
        assert!(auto.counters["Visible voxel bricks"] >= auto.counters["Voxel plant clusters"]);
        let pass_index = |name: &str| auto.passes.iter().position(|p| p.name == name).unwrap();
        let final_raster = if atomic64 {
            "Final voxel brick visibility trace"
        } else {
            "Final voxel brick visibility"
        };
        assert!(pass_index("Main mesh visibility") < pass_index("Current Hi-Z mip 1"));
        assert!(pass_index("Current Hi-Z mip 1") < pass_index("Post Hi-Z retest"));
        assert!(pass_index("Post Hi-Z retest") < pass_index("Post mesh visibility"));
        assert!(pass_index("Post mesh visibility") < pass_index(final_raster));
        assert!(pass_index(final_raster) < pass_index("History Hi-Z mip 1"));
        assert!(!auto.passes.iter().any(|p| p.name.ends_with("depth bins")));
        assert!(
            auto.passes[..pass_index("Post mesh visibility")]
                .iter()
                .all(|p| !p.name.contains("voxel brick visibility"))
        );

        settings.wind = 0.;
        render(settings)?;
        let fixed = render(settings)?;
        settings.time += 0.8;
        let next = render(settings)?;
        let changed = fixed
            .pixels
            .chunks_exact(4)
            .zip(next.pixels.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(
            fixed.counters["Plant clusters in transition"],
            next.counters["Plant clusters in transition"]
        );
        // Unordered mesh dispatch can vary isolated equal-depth samples. A
        // time-dependent transition mask would affect many more pixels.
        assert!(
            changed * 1000 < fixed.pixels.len() / 4,
            "fixed-camera foliage changed at {changed} pixels"
        );

        settings.mode = Mode::Triangles;
        let calm = render(settings)?;
        assert_eq!(calm.counters["Voxel plant clusters"], 0);
        assert!(
            !calm
                .passes
                .iter()
                .any(|p| p.name.contains("voxel brick visibility"))
        );
        assert_eq!(calm.counters["Active wind bones"], 0);
        settings.wind = 1.7;
        settings.time = 1.4;
        let wind = render(settings)?;
        assert!(wind.counters["Active wind bones"] > 1000);
        assert_eq!(wind.profile.resident_upload_bytes, 0);
        assert_ne!(calm.pixels, wind.pixels);

        settings.mode = Mode::Voxels;
        settings.visualize = true;
        if atomic64 {
            settings.raster = BrickRaster::Compare;
        }
        let voxels = render(settings)?;
        if atomic64 {
            settings.raster = BrickRaster::Mesh;
            let mesh = render(settings)?;
            assert_eq!(
                mesh.counters["Visible voxel bricks"],
                voxels.counters["Visible voxel bricks"]
            );
            let differences = voxels
                .pixels
                .chunks_exact(4)
                .zip(mesh.pixels.chunks_exact(4))
                .filter(|(a, b)| a != b)
                .count();
            assert!(
                differences * 1000 < voxels.pixels.len() / 4,
                "final compute and mesh images differed at {differences} pixels"
            );
            let c = &voxels.counters;
            assert_eq!(
                c["Brick candidate pixels"],
                c["Brick LOD rejects"] + c["Brick early depth rejects"] + c["Brick DDA calls"]
            );
            assert!(c["Brick early depth rejects"] > 0 && c["Compute brick changed pixels"] > 100);
            assert!(c["Brick coverage differences"] * 100 < c["Compute brick changed pixels"]);
            assert!(
                c["Brick depth differences over 1e-5"] * 100 < c["Compute brick changed pixels"]
            );
        }
        assert!(voxels.counters["Voxel plant clusters"] > 0);
        assert!(
            voxels
                .pixels
                .chunks_exact(4)
                .filter(|p| p[0] > p[1] && u16::from(p[1]) > 2 * u16::from(p[2]) && p[1] > 150)
                .count()
                > 100
        );
        Ok(())
    }
}
