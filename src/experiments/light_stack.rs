//! Independent light producers attach named inputs to material bins after visibility.
use super::{ibl, restir};
use crate::render::FrameGraph;
use crate::{
    rdg::{Access, Resource},
    render::{
        Atlas, Camera, Forward, FrameLighting, LightingInput, MaterialInputs, MaterialPrograms,
        PosedSurface, ResolveInputs, Snapshot, Technique, build_frame_with_lighting,
    },
    scene::{Shading, World},
};
use anyhow::{Result, ensure};
use glam::Mat4;
use std::cell::Cell;
#[cfg(test)]
use std::sync::Arc;

type DeviceCheck = fn(&World, crate::rhi::RayCapabilities) -> Result<()>;

pub(super) struct Registration {
    pub key: &'static str,
    pub feature: &'static str,
    pub material: MaterialShader,
    pub inputs: &'static [LightingInput],
    pub open: fn(&World, &rayon::ThreadPool) -> Result<Box<dyn Producer>>,
    pub camera: fn() -> Result<Camera>,
    pub validate_device: Option<DeviceCheck>,
}
pub(super) struct MaterialShader {
    pub name: &'static str,
    pub display: &'static [u8],
    pub linear: &'static [u8],
}

pub(super) type Backdrop = fn(&mut FrameGraph, Resource, Resource, (Resource, Resource), bool);

pub(super) const REGISTRY: &[Registration] = &[restir::REGISTRATION, ibl::REGISTRATION];

#[derive(Clone, Copy)]
pub(super) enum Channel {
    Direct,
    Environment,
}
pub(super) struct Contribution {
    pub channel: Channel,
    pub indirect_diffuse: Option<Resource>,
    pub resources: (Resource, Resource),
    pub backdrop: Option<Backdrop>,
}

pub(super) type AddLight<'a> =
    Box<dyn FnOnce(&mut FrameGraph, ResolveInputs) -> Result<Contribution> + 'a>;

pub(super) trait Producer: Send + Sync {
    fn uses_shadow_rays(&self) -> bool {
        false
    }
    fn needs_motion(&self, _: &Snapshot) -> bool {
        false
    }
    fn prepare<'a>(
        &'a self,
        s: &'a Snapshot,
        surfaces: &[PosedSurface<'_>],
    ) -> Result<AddLight<'a>>;
}
pub(super) struct LightStack {
    producers: Vec<Box<dyn Producer>>,
    hdr: bool,
}
impl LightStack {
    #[cfg(test)]
    pub fn open(
        world: &World,
        programs: &mut MaterialPrograms,
        tasks: &rayon::ThreadPool,
    ) -> Result<Option<(Arc<dyn Technique>, Camera)>> {
        Ok(Self::open_modules(world, programs, tasks, false)?
            .map(|(stack, camera)| (Arc::new(stack) as Arc<dyn Technique>, camera)))
    }
    pub fn open_modules(
        world: &World,
        programs: &mut MaterialPrograms,
        tasks: &rayon::ThreadPool,
        ldr: bool,
    ) -> Result<Option<(Self, Camera)>> {
        let registrations = REGISTRY.iter().filter(|r| {
            world.first_raw_component(r.key).is_some()
                && super::composition::enabled(world, r.feature)
        });
        let camera = if let Some(first) = registrations.clone().next() {
            (first.camera)()?
        } else if super::composition::is_scene(world) {
            Camera::default()
        } else {
            return Ok(None);
        };
        let mut producers = Vec::new();
        let builtin_lighting = world.entities.values().any(|e| {
            e.material
                .as_ref()
                .is_some_and(|m| matches!(m.shading, Shading::Pbr | Shading::Toon))
                || e.instance.as_ref().is_some_and(|i| i.material.is_none())
        });
        let hdr = builtin_lighting && !ldr;
        for registration in registrations {
            let producer = (registration.open)(world, tasks)?;
            let MaterialShader {
                name,
                display,
                linear,
            } = registration.material;
            if world
                .entities
                .values()
                .filter_map(|e| e.material.as_ref())
                .any(|m| matches!(&m.shading,Shading::Custom(s) if s==name))
            {
                programs.register_lighting(
                    name,
                    if hdr { linear } else { display },
                    registration.inputs,
                )?;
            }
            producers.push(producer);
        }
        for material in world.entities.values().filter_map(|e| e.material.as_ref()) {
            programs.bin(&material.shading)?;
        }
        if builtin_lighting {
            if ldr {
                programs.replace_builtin(
                    include_bytes!(concat!(env!("OUT_DIR"), "/mixed_material_ldr.comp.spv")),
                    include_bytes!(concat!(env!("OUT_DIR"), "/mixed_toon_ldr.comp.spv")),
                );
            } else {
                programs.replace_builtin(
                    include_bytes!(concat!(env!("OUT_DIR"), "/mixed_material.comp.spv")),
                    include_bytes!(concat!(env!("OUT_DIR"), "/mixed_toon.comp.spv")),
                );
            }
        }
        Ok(Some((Self { producers, hdr }, camera)))
    }
}
impl Technique for LightStack {
    fn needs_surface_motion(&self, s: &Snapshot) -> bool {
        self.producers.iter().any(|p| p.needs_motion(s))
    }
    fn build(&self, s: &Snapshot) -> Result<FrameGraph> {
        let surfaces = Forward::default().posed_surfaces(s, |_| true)?;
        let layers = self.layers(s, &surfaces)?;
        build_modules(
            s,
            |g, t| {
                g.products.instance_visits += surfaces.len();
                draw_surfaces(g, t, s, surfaces)
            },
            layers,
            self.hdr,
            |_, _, _, _| Ok(()),
        )
    }
}
impl LightStack {
    pub fn uses_shadow_rays(&self) -> bool {
        self.producers.iter().any(|p| p.uses_shadow_rays())
    }
    pub fn layers<'a>(
        &'a self,
        s: &'a Snapshot,
        surfaces: &[PosedSurface<'_>],
    ) -> Result<Vec<AddLight<'a>>> {
        self.producers
            .iter()
            .map(|p| p.prepare(s, surfaces))
            .collect()
    }
}
pub(super) fn draw_surfaces(
    g: &mut FrameGraph,
    t: &mut crate::render::Targets,
    s: &Snapshot,
    surfaces: Vec<PosedSurface<'_>>,
) -> Result<()> {
    for surface in surfaces {
        let mut item = t.material(s, surface.instance, Mat4::IDENTITY)?;
        if surface.instance.emitter {
            item.params[3] = 1.;
        }
        surface.draw(g, t, s, item)?;
    }
    Ok(())
}
#[cfg(test)]
pub(super) fn build(
    s: &Snapshot,
    surfaces: Vec<PosedSurface<'_>>,
    layers: Vec<AddLight<'_>>,
    hdr: bool,
) -> Result<FrameGraph> {
    build_modules(
        s,
        |g, t| draw_surfaces(g, t, s, surfaces),
        layers,
        hdr,
        |_, _, _, _| Ok(()),
    )
}
pub(super) fn build_modules(
    s: &Snapshot,
    geometry: impl FnOnce(&mut FrameGraph, &mut crate::render::Targets) -> Result<()>,
    layers: Vec<AddLight<'_>>,
    hdr: bool,
    post: impl FnOnce(&mut FrameGraph, Resource, Resource, Resource) -> Result<()>,
) -> Result<FrameGraph> {
    let sky = Cell::new(None);
    build_frame_with_lighting(
        s,
        Atlas::cached(&s.scene)?,
        geometry,
        |g, inputs| {
            let mut materials = Vec::new();
            let mut direct = None;
            let mut environment = None;
            let mut indirect = None;
            for add in layers {
                let light = add(g, inputs)?;
                let slot = match light.channel {
                    Channel::Direct => &mut direct,
                    Channel::Environment => &mut environment,
                };
                ensure!(
                    slot.replace(light.resources).is_none(),
                    "duplicate producer for lighting channel"
                );
                if let Some(draw) = light.backdrop {
                    ensure!(
                        sky.replace(Some((draw, light.resources))).is_none(),
                        "duplicate backdrop producer"
                    );
                }
                if let Some(gi) = light.indirect_diffuse {
                    ensure!(
                        indirect.replace(gi).is_none(),
                        "duplicate indirect diffuse producer"
                    );
                }
            }
            let mut neutral = std::collections::HashMap::new();
            for (bin, name, inputs, optional) in s.materials.lighting() {
                let mut bindings = Vec::with_capacity(inputs.len());
                for &input in inputs {
                    let resource = match input {
                        LightingInput::Environment => environment.map(|pair| pair.0),
                        LightingInput::EnvironmentControls => environment.map(|pair| pair.1),
                        LightingInput::Direct => direct.map(|pair| pair.0),
                        LightingInput::DirectControls => direct.map(|pair| pair.1),
                        LightingInput::IndirectDiffuse => indirect,
                    };
                    let resource = match resource {
                        Some(resource) => resource,
                        // The mixed kernels branch on zero controls before accessing absent data.
                        None if optional => *neutral.entry(input).or_insert_with(|| {
                            g.input(&format!("Disabled {input:?}"), vec![0; 112])
                        }),
                        None => anyhow::bail!("{name} requires a producer for {input:?}"),
                    };
                    bindings.push((resource, Access::ComputeRead));
                }
                materials.push(MaterialInputs { bin, bindings });
            }
            Ok(FrameLighting {
                materials,
                linear_hdr: hdr,
            })
        },
        |g, ids, depth, color| {
            post(g, ids, depth, color)?;
            if let Some((draw, resources)) = sky.get() {
                draw(g, ids, color, resources, hdr);
            }
            Ok(())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::LightingInput::{Environment, EnvironmentControls};

    #[test]
    fn custom_bin_uses_its_declared_resources_in_shader_order() -> Result<()> {
        let world = World::demo();
        let mut programs = MaterialPrograms::default();
        let shader = ibl::REGISTRATION.material.display;
        programs.register_lighting(
            "Independent material",
            shader,
            &[Environment, EnvironmentControls],
        )?;
        let view = Snapshot::new(&world, Camera::fit(&world)?, [32, 32], Arc::new(programs))?;
        let build =
            |layers| build_modules(&view, |_, _| Ok(()), layers, false, |_, _, _, _| Ok(()));
        let error = build(Vec::new()).err().expect("missing producer");
        assert!(
            error
                .to_string()
                .contains("Independent material requires a producer for Environment")
        );
        let source: AddLight<'_> = Box::new(|g, _| {
            Ok(Contribution {
                channel: Channel::Environment,
                resources: (
                    g.input("Custom tables", vec![0; 16]),
                    g.input("Custom controls", vec![0; 112]),
                ),
                indirect_diffuse: None,
                backdrop: None,
            })
        });
        let graph = build(vec![source])?;
        let pass = graph
            .passes
            .iter()
            .find(|p| p.name == "Shade Independent material")
            .expect("registered bin");
        let bindings = &pass.op.program().unwrap().bindings;
        let tail: Vec<_> = bindings[bindings.len() - 2..]
            .iter()
            .map(|(r, _)| graph.resources[r.0].0.as_str())
            .collect();
        assert_eq!(tail, ["Custom tables", "Custom controls"]);
        Ok(())
    }
}
