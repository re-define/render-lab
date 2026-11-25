//! Two procedural experiments sharing the engine's mesh visibility and material passes.
use crate::experiments::assembly::{Primary, Registration as SceneModule};
use crate::render::material_code;
pub(super) const MODULE: SceneModule = SceneModule {
    key: SETTING,
    primary: Primary::Visibility,
    restore: None,
    install: |world, a| {
        a.camera = Some(configure(
            world,
            &mut a.materials,
            &mut a.schedule,
            super::composition::enabled(world, "water"),
        )?);
        if super::composition::enabled(world, "water") {
            a.visibility.route(
                Arc::new(Effects),
                super::composition::SurfaceRoute {
                    name: "Surface effects",
                    owns: Effects::owns,
                    draw: |g, targets, s, instance, outputs| {
                        if let Some(reflector) = Effects::draw(g, targets, s, instance)? {
                            outputs.rivers.push(reflector);
                        }
                        Ok(())
                    },
                },
            );
            a.visibility.layers.scene.push((
                super::composition::SceneStage::Reflections,
                |g, s, view, products| {
                    river::reflections(g, s, view, &products.rivers);
                    Ok(())
                },
            ));
        }
        Ok(())
    },
};
use crate::render::FrameGraph;
mod fracture;
pub(in crate::experiments) mod river;
#[cfg(test)]
mod tests;
mod water;
use crate::{
    engine::{
        Engine, Schedule, SimulationData, SimulationSystem, SystemAccess, SystemContext,
        SystemOrder, TransformBindings, TransformTargets,
    },
    rdg::Shader,
    render::{
        self, Atlas, Camera, Forward, Geometry, MaterialPrograms, MotionSource, Snapshot,
        SurfaceBuffers, SurfaceInstance, Technique,
    },
    scene::{Entity, Id, Instance, LocalPose, Material, Mesh, Shading, World},
};
use anyhow::{Context, Result, bail, ensure};
use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

macro_rules! spv {
    ($name:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/", $name, ".spv"))
    };
}
use spv;

pub const SETTING: &str = "lab.surface-effects";
const WATER: &str = "Cascade water";
const ORE: &str = "Fractured mineral";
const DISSOLVE: &str = "Dissolving mineral";
const STONE: &str = "Cut stone";

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Water,
    River,
    Destruction,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Settings {
    kind: Kind,
    time: f64,
}
impl Settings {
    fn read(value: &serde_json::Value) -> Result<Self> {
        let s: Self = serde_json::from_value(value.clone())?;
        ensure!(s.time.is_finite() && s.time >= 0., "invalid effect time");
        Ok(s)
    }
}
fn settings(world: &World) -> Result<(Id, Settings)> {
    let (id, value) = world
        .first_raw_component(SETTING)
        .context("open water or destruction first")?;
    Ok((id, Settings::read(value)?))
}
fn mesh(world: &mut World, name: &str, data: Mesh) -> Id {
    world.spawn(Entity {
        name: name.into(),
        mesh: Some(Arc::new(data)),
        ..Default::default()
    })
}
fn material(world: &mut World, name: &str, program: &str, rgb: [f32; 3]) -> Id {
    world.spawn(Entity {
        name: name.into(),
        material: Some(Material {
            shading: Shading::Custom(program.into()),
            base_color: [rgb[0], rgb[1], rgb[2], 1.],
            ..Default::default()
        }),
        ..Default::default()
    })
}
fn instance(world: &mut World, name: &str, mesh: Id, material: Id, model: Mat4) -> Id {
    world.spawn(Entity {
        name: name.into(),
        instance: Some(Instance::new(mesh, Some(material))),
        local: model,
        ..Default::default()
    })
}
fn slab(world: &mut World, name: &str, cube: Id, material: Id, center: [f32; 3], size: [f32; 3]) {
    instance(
        world,
        name,
        cube,
        material,
        Mat4::from_translation(center.into()) * Mat4::from_scale(size.into()),
    );
}
fn setup(kind: Kind) -> World {
    let mut world = World::default();
    world.spawn(Entity {
        name: "Effect playback".into(),
        components: [(
            SETTING.into(),
            serde_json::json!(Settings { kind, time: 0. }),
        )]
        .into(),
        ..Default::default()
    });
    world
}
pub fn water() -> Result<World> {
    water::scene()
}
pub fn destruction() -> Result<World> {
    fracture::scene()
}

struct Playback {
    pieces: Vec<fracture::Piece>,
}
impl SimulationSystem for Playback {
    fn order(&self) -> SystemOrder {
        SystemOrder {
            key: Some(SETTING),
            after: &[],
        }
    }
    fn access(&self) -> SystemAccess {
        use SimulationData::{Component, LocalTransformsById};
        SystemAccess {
            name: "Effect playback",
            reads: if self.pieces.is_empty() {
                &[Component(SETTING)]
            } else {
                &[Component(SETTING), LocalTransformsById]
            },
            writes: if self.pieces.is_empty() {
                &[Component(SETTING)]
            } else {
                &[Component(SETTING), LocalTransformsById]
            },
        }
    }
    fn transform_targets(&self, _: &TransformBindings<'_>) -> Result<TransformTargets> {
        let joints: Vec<_> = self.pieces.iter().map(|piece| piece.id).collect();
        Ok(TransformTargets {
            reads: joints.clone(),
            writes: joints,
        })
    }
    fn run(&self, cx: &mut SystemContext<'_>, dt: f32) -> Result<()> {
        if dt == 0. {
            return Ok(());
        }
        let (id, raw) = cx
            .first_raw_component(SETTING)?
            .context("open water or destruction first")?;
        let mut s = Settings::read(raw)?;
        s.time += f64::from(dt);
        if !self.pieces.is_empty() {
            let mut transforms = Vec::with_capacity(self.pieces.len());
            for piece in &self.pieces {
                let (joint, local) = piece.pose(s.time);
                if cx.local(joint)?.is_some() {
                    transforms.push((joint, local));
                }
            }
            cx.set_pose(transforms)?;
        }
        cx.set_component(id, SETTING, serde_json::to_value(s)?)
    }
}
pub fn command(engine: &mut Engine, args: &str) -> Result<String> {
    let (id, mut s) = settings(&engine.world)?;
    s.time = match args.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["restart"] => 0.,
        ["time", v] => v.parse()?,
        _ => bail!("effect restart | effect time SECONDS"),
    };
    ensure!(
        s.time.is_finite() && s.time >= 0.,
        "expected nonnegative finite seconds"
    );
    let world = Arc::make_mut(&mut engine.world);
    if s.kind == Kind::Destruction {
        let pieces = fracture::pieces(world)?;
        LocalPose::prepare(world, pieces.iter().map(|p| p.pose(s.time)))?.apply(world);
    }
    world.set_setting(id, SETTING, serde_json::to_value(s)?);
    engine.post.reset();
    Ok(format!("Effect time {:.3} s", s.time))
}

pub(super) fn configure(
    world: &World,
    programs: &mut MaterialPrograms,
    schedule: &mut Schedule,
    active: bool,
) -> Result<Camera> {
    let (_, s) = settings(world)?;
    if s.kind != Kind::River {
        programs.register(STONE, material_code!("stone_material.comp"))?;
    }
    let camera = match s.kind {
        Kind::Water => {
            programs.register(WATER, material_code!("water_material.comp"))?;
            Camera {
                target: Vec3::new(0., 1.23, 0.55),
                distance: 10.9,
                yaw: 0.97,
                pitch: 0.39,
            }
        }
        Kind::River => {
            programs.register(river::WATER, material_code!("river_material.comp"))?;
            super::vegetation::camera("river")?
        }
        Kind::Destruction => {
            programs.register(ORE, material_code!("ore_material.comp"))?;
            programs.register(DISSOLVE, material_code!("dissolve_material.comp"))?;
            Camera {
                target: Vec3::new(0., 1.04, 0.),
                distance: 8.8,
                yaw: 0.06,
                pitch: 0.24,
            }
        }
    };
    let pieces = if s.kind == Kind::Destruction {
        fracture::pieces(world)?
    } else {
        Vec::new()
    };
    if active {
        schedule.add_system(Arc::new(Playback { pieces }));
    }
    Ok(camera)
}

pub(super) struct Effects;
impl Effects {
    pub fn owns(s: &Snapshot, i: &render::RenderInstance) -> bool {
        i.material
            .and_then(|id| s.scene.materials.get(&id))
            .is_some_and(
                |m| matches!(&m.shading,Shading::Custom(name) if name==WATER || name==river::WATER || name==DISSOLVE),
            )
    }
    pub fn draw(
        g: &mut FrameGraph,
        t: &mut render::Targets,
        s: &Snapshot,
        i: &render::RenderInstance,
    ) -> Result<Option<river::Reflector>> {
        let settings = Settings::read(
            s.scene
                .setting(SETTING)
                .context("missing effect settings")?,
        )?;
        let mesh = &s.scene.meshes[&i.mesh].mesh;
        let (rest, faces) = s.scene.mesh_cache.rigid_streams(i.mesh, mesh, g);
        let mut surface = SurfaceBuffers {
            vertices: rest,
            faces,
            count: (mesh.indices.len() / 3).try_into()?,
            model: i.model,
        };
        let mut mat = t.material(s, i, i.model)?;
        let history = s.surface_history(i)?;
        let previous = history.as_ref().and_then(|h| h.previous);
        let mut motion = previous.map_or(MotionSource::Unavailable, |prev| {
            MotionSource::rigid(i.model, prev.model)
        });
        match settings.kind {
            Kind::Water | Kind::River => {
                let old = s
                    .temporal
                    .as_ref()
                    .and_then(|f| f.previous_scene.as_ref())
                    .and_then(|scene| scene.setting(SETTING))
                    .map(Settings::read)
                    .transpose()?;
                let prior = old
                    .zip(previous)
                    .filter(|(old, _)| old.kind == settings.kind && old.time <= settings.time);
                let previous = prior.map(|(old, p)| (old.time, p.model));
                let (vertices, old_positions) = if settings.kind == Kind::River {
                    ensure!(
                        mesh.positions.len() == river::VERTICES,
                        "unexpected river grid dimensions"
                    );
                    river::deform(g, rest, settings.time, previous)
                } else {
                    ensure!(
                        mesh.positions.len() == water::VERTICES,
                        "unexpected cascade grid dimensions"
                    );
                    water::simulate(g, settings.time, previous)
                };
                surface.vertices = vertices;
                g.products.ray_surfaces.push(render::rays::Surface::new(
                    surface,
                    mat.color,
                    mat.params[0],
                    true,
                    i.material
                        .map_or(0, |m| s.scene.routes.material_slots[&m] as u32),
                ));
                motion = old_positions.map_or(MotionSource::Unavailable, MotionSource::Skinned);
            }
            Kind::Destruction => mat.params[3] = fracture::dissolve(settings.time),
        }
        mat.tex[3] |= SurfaceInstance::TEMPORAL_REACTIVE;
        let item = t.register_scene(
            s,
            i,
            mat,
            Geometry {
                vertices: surface.vertices,
                indices: faces,
                clusters: None,
            },
            history.map(|h| (h, motion)),
        )?;
        if settings.kind != Kind::Destruction {
            render::raster_plain(g, t, s, &surface, item);
        } else {
            render::raster_surface(
                g,
                t,
                s,
                &surface,
                item,
                Shader::Mesh(spv!("dissolve.mesh"), spv!("dissolve.frag")),
                bytemuck::bytes_of(&mat.params[3]),
            );
        }

        Ok((settings.kind == Kind::River).then_some(river::Reflector {
            id: item + 1,
            surface,
        }))
    }
}
impl Technique for Effects {
    fn build(&self, s: &Snapshot) -> Result<FrameGraph> {
        let rivers = std::cell::RefCell::new(Vec::new());
        render::build_frame_with_post(
            s,
            Atlas::cached(&s.scene)?,
            |g, t| {
                t.background = 0xff202c38;
                Forward::default().draw_subset(g, t, s, |i| !Self::owns(s, i))?;
                g.products.instance_visits += s.scene.instances.len();
                for i in s.scene.instances.iter().filter(|i| Self::owns(s, i)) {
                    if let Some(id) = Self::draw(g, t, s, i)? {
                        rivers.borrow_mut().push(id);
                    }
                }
                Ok(())
            },
            |g, inputs| {
                river::reflections(g, s, inputs, &rivers.borrow());
                Ok(())
            },
            |_, _| Ok(()),
        )
    }
}
