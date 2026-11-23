use super::light_stack::{AddLight, Channel, Contribution, Producer};
use crate::rdg::PassPhase;
use crate::render::FrameGraph;
mod bake;

use super::lighting::scenes::sphere;
use crate::{
    engine::Engine,
    rdg::{Access, Dispatch, Op, Resource, Shader},
    render::{Camera, PosedSurface, Snapshot},
    scene::{Entity, Instance, Material, Shading, World},
};
use anyhow::{Context, Result, bail, ensure};
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};

pub const SETTING: &str = "lab.ibl";
const PROGRAM: &str = "Environment PBR";
const MATERIAL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ibl_material.comp.spv"));
pub(super) const CAMERA: Camera = Camera {
    target: Vec3::ZERO,
    distance: 8.1,
    yaw: 0.,
    pitch: 0.01,
};
static TABLES: OnceLock<Arc<[u8]>> = OnceLock::new();

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[repr(u32)]
#[serde(rename_all = "lowercase")]
enum View {
    #[default]
    Combined,
    Diffuse,
    Specular,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(super) struct Settings {
    exposure: f32,
    yaw: f32,
    pub(super) sky: bool,
    view: View,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            exposure: 0.,
            yaw: 0.,
            sky: false,
            view: View::Combined,
        }
    }
}

pub(super) fn read(world: &World) -> Result<(crate::scene::Id, Settings)> {
    let (id, raw) = world
        .first_raw_component(SETTING)
        .context("scene has no IBL settings")?;
    Ok((id, decode(raw)?))
}

pub(super) fn decode(raw: &serde_json::Value) -> Result<Settings> {
    let settings = Settings::deserialize(raw)?;
    ensure!(
        (-8. ..=8.).contains(&settings.exposure) && settings.yaw.is_finite(),
        "invalid IBL exposure or rotation"
    );
    Ok(settings)
}

pub fn scene() -> Result<World> {
    let mut world = World::default();
    let mesh = world.spawn(Entity {
        name: "Smooth sphere geometry".into(),
        mesh: Some(Arc::new(sphere(48, 24))),
        ..Default::default()
    });
    for (row, metallic) in [0., 0.5, 1.].into_iter().enumerate() {
        for (col, roughness) in [0.06, 0.19, 0.35, 0.52, 0.73, 0.96].into_iter().enumerate() {
            let name = format!("Metallic {metallic:.1}, roughness {roughness:.2}");
            let material = world.spawn(Entity {
                name: format!("{name} material"),
                material: Some(Material {
                    shading: Shading::Custom(PROGRAM.into()),
                    base_color: [0.83, 0.38, 0.16, 1.],
                    metallic,
                    roughness,
                    ..Default::default()
                }),
                ..Default::default()
            });
            world.spawn(Entity {
                name,
                local: Mat4::from_scale_rotation_translation(
                    Vec3::splat(1.42),
                    Quat::IDENTITY,
                    Vec3::new((col as f32 - 2.5) * 1.66, (1. - row as f32) * 1.69, 0.),
                ),
                instance: Some(Instance::new(mesh, Some(material))),
                ..Default::default()
            });
        }
    }
    world.spawn(Entity {
        name: "Studio IBL settings".into(),
        components: [(SETTING.into(), serde_json::to_value(Settings::default())?)].into(),
        ..Default::default()
    });
    world.validate()?;
    Ok(world)
}

pub(super) fn open_producer(world: &World, tasks: &rayon::ThreadPool) -> Result<Box<dyn Producer>> {
    read(world)?;
    Ok(Box::new(Ibl::new(tasks)?))
}

pub fn command(engine: &mut Engine, args: &str) -> Result<String> {
    let (id, mut cfg) = read(&engine.world)?;
    let args: Vec<_> = args.split_whitespace().collect();
    match args.as_slice() {
        ["ev", ev] => {
            cfg.exposure = ev.parse()?;
            ensure!(
                (-8. ..=8.).contains(&cfg.exposure),
                "ibl ev expects [-8, 8]"
            );
        }
        ["yaw", yaw] => {
            let degrees: f32 = yaw.parse()?;
            ensure!(degrees.is_finite(), "ibl yaw expects finite degrees");
            cfg.yaw = degrees.rem_euclid(360.).to_radians();
        }
        ["view", mode] => {
            cfg.view = match *mode {
                "combined" => View::Combined,
                "diffuse" => View::Diffuse,
                "specular" => View::Specular,
                _ => bail!("ibl view combined/diffuse/specular"),
            }
        }
        ["sky", "on"] => cfg.sky = true,
        ["sky", "off"] => cfg.sky = false,
        _ => bail!(
            "ibl ev EV · ibl yaw DEGREES · ibl view combined/diffuse/specular · ibl sky on/off"
        ),
    }
    Arc::make_mut(&mut engine.world)
        .entities
        .get_mut(&id)
        .unwrap()
        .components
        .insert(SETTING.into(), serde_json::to_value(cfg)?);
    Ok(format!("ibl {}", args.join(" ")))
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct Uniform {
    inverse_vp: [f32; 16],
    eye: [f32; 4],
    controls: [f32; 4],
    screen: [u32; 4],
}

pub(super) struct Ibl(Arc<[u8]>);
impl Ibl {
    pub(super) fn new(tasks: &rayon::ThreadPool) -> Result<Self> {
        let tables = if let Some(tables) = TABLES.get() {
            tables
        } else {
            let baked = tasks.install(bake::tables)?;
            TABLES.get_or_init(|| baked)
        };
        Ok(Self(tables.clone()))
    }

    pub(super) fn resources(
        &self,
        g: &mut FrameGraph,
        s: &Snapshot,
        cfg: Settings,
    ) -> (Resource, Resource) {
        let tables = g.resident_input(
            "HDR environment, diffuse irradiance, GGX prefilter and BRDF LUT",
            self.0.clone(),
        );
        let [w, h] = s.extent;
        let uniform = Uniform {
            inverse_vp: s.view_projection().inverse().to_cols_array(),
            eye: s.camera.eye().extend(1.).to_array(),
            controls: [cfg.exposure.exp2(), cfg.yaw, cfg.view as u32 as f32, 0.],
            screen: [w, h, 0, 0],
        };
        let uniform = g.input(
            "IBL camera, exposure and rotation",
            bytemuck::bytes_of(&uniform).to_vec(),
        );
        (tables, uniform)
    }
}

fn backdrop(
    g: &mut FrameGraph,
    ids: Resource,
    color: Resource,
    (tables, uniform): (Resource, Resource),
    linear_hdr: bool,
) {
    use Access::{ComputeRead as R, ComputeReadWrite as RW};
    let [w, h] = g.extent;
    g.pass_in(
        PassPhase::Lighting,
        "Environment backdrop",
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(if linear_hdr {
                    include_bytes!(concat!(env!("OUT_DIR"), "/ibl_sky_hdr.comp.spv"))
                } else {
                    include_bytes!(concat!(env!("OUT_DIR"), "/ibl_sky.comp.spv"))
                }),
                bindings: vec![
                    (ids, Access::ComputeSample),
                    (color, RW),
                    (tables, R),
                    (uniform, R),
                ],
                push: vec![],
            },
            groups: [w.div_ceil(8), h.div_ceil(8), 1],
        },
    );
}

pub(super) const REGISTRATION: super::light_stack::Registration =
    super::light_stack::Registration {
        key: SETTING,
        feature: "ibl",
        inputs: crate::render::LightingInput::IBL,
        material: super::light_stack::MaterialShader {
            name: PROGRAM,
            display: MATERIAL,
            linear: include_bytes!(concat!(env!("OUT_DIR"), "/ibl_material_hdr.comp.spv")),
        },
        open: open_producer,
        camera: || Ok(CAMERA),
    };
impl Producer for Ibl {
    fn prepare<'a>(&'a self, s: &'a Snapshot, _: &[PosedSurface<'_>]) -> Result<AddLight<'a>> {
        let cfg = decode(
            s.scene
                .setting(SETTING)
                .context("scene has no IBL settings")?,
        )?;
        Ok(Box::new(move |g, _| {
            Ok(Contribution {
                channel: Channel::Environment,
                indirect_diffuse: None,
                resources: self.resources(g, s, cfg),
                backdrop: cfg.sky.then_some(backdrop),
            })
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{render::MaterialPrograms, rhi::Backend};

    #[test]
    #[ignore = "requires Vulkan 1.4 and VK_EXT_descriptor_heap"]
    fn environment_lights_dielectric_diffuse_and_metal_reflection() -> Result<()> {
        let mut world = scene()?;
        let mut materials = MaterialPrograms::default();
        materials.register_lighting(PROGRAM, MATERIAL, REGISTRATION.inputs)?;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
        let ibl = Ibl(pool.install(bake::tables)?);
        let mut gpu = Backend::open()?;
        let camera = CAMERA;
        let extent = [320, 200];
        let (id, mut settings) = read(&world)?;
        let mut frame = |cfg: Settings| {
            world
                .entities
                .get_mut(&id)
                .unwrap()
                .components
                .insert(SETTING.into(), serde_json::to_value(cfg)?);
            let view = Snapshot::new(&world, camera, extent, Arc::new(materials.clone()))?;
            let surfaces = crate::render::Forward::default().posed_surfaces(&view, |_| true)?;
            let layer = ibl.prepare(&view, &surfaces)?;
            gpu.execute(
                super::super::light_stack::build(&view, surfaces, vec![layer], false)?.compile()?,
                &pool,
            )
        };
        let center = |point: Vec3| {
            let clip = camera.view_projection(extent) * point.extend(1.);
            let p = (clip.truncate().truncate() / clip.w * 0.5 + 0.5)
                * glam::Vec2::new(extent[0] as f32, extent[1] as f32);
            (4 * (p.y as u32 * extent[0] + p.x as u32)) as usize
        };
        let dielectric = center(Vec3::new(-4.15, 1.69, 0.));
        let metal = center(Vec3::new(-4.15, -1.69, 0.));
        settings.view = View::Diffuse;
        let diffuse = frame(settings)?;
        assert_ne!(&diffuse.pixels[dielectric..dielectric + 3], &[0; 3]);
        assert_eq!(&diffuse.pixels[metal..metal + 3], &[0; 3]);
        settings.view = View::Specular;
        let specular = frame(settings)?;
        assert_ne!(&specular.pixels[metal..metal + 3], &[0; 3]);
        settings.yaw = std::f32::consts::FRAC_PI_2;
        let rotated = frame(settings)?;
        assert_ne!(specular.pixels, rotated.pixels);
        Ok(())
    }
}
