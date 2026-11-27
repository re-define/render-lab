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
use anyhow::Result;
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

pub(super) mod denoise;
pub(super) mod diffuse_gi;
mod stages;
pub(super) use stages::{
    AddLight, Contribution, Samples, Signal, SurfaceLighting, pass, pixel_buffer,
};

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
    ) -> Result<Vec<AddLight<'a>>>;
}
pub(super) struct LightStack {
    producers: Vec<Layer>,
    pub(super) hdr: bool,
}
struct Layer {
    registration: &'static Registration,
    producer: std::sync::OnceLock<Box<dyn Producer>>,
}
impl Layer {
    fn prepare(&self, world: &World, tasks: &rayon::ThreadPool) -> Result<()> {
        if self.producer.get().is_none() {
            let producer = (self.registration.open)(world, tasks)?;
            let _ = self.producer.set(producer);
        }
        Ok(())
    }
}
impl LightStack {
    fn active<'a>(&'a self, s: &'a Snapshot) -> impl Iterator<Item = &'a dyn Producer> {
        self.producers.iter().filter_map(|layer| {
            let enabled = super::composition::layer_enabled(
                s.scene.setting(super::composition::SETTING),
                layer.registration.feature,
            );
            enabled
                .then(|| layer.producer.get().map(Box::as_ref))
                .flatten()
        })
    }
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
        mixed_geometry: bool,
    ) -> Result<Option<(Self, Camera)>> {
        let registrations = REGISTRY
            .iter()
            .filter(|r| world.first_raw_component(r.key).is_some());
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
        let hdr = builtin_lighting || mixed_geometry || super::composition::is_scene(world);
        for registration in registrations {
            let producer = Layer {
                registration,
                producer: Default::default(),
            };
            if super::composition::enabled(world, registration.feature) {
                producer.prepare(world, tasks)?;
            }
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
                    crate::render::MaterialCode {
                        display,
                        linear: Some(linear),
                    },
                    registration.inputs,
                )?;
            }
            producers.push(producer);
        }
        for material in world.entities.values().filter_map(|e| e.material.as_ref()) {
            programs.bin(&material.shading)?;
        }
        if builtin_lighting {
            programs.replace_builtin(
                crate::render::MaterialCode {
                    display: include_bytes!(concat!(
                        env!("OUT_DIR"),
                        "/mixed_material_ldr.comp.spv"
                    )),
                    linear: Some(include_bytes!(concat!(
                        env!("OUT_DIR"),
                        "/mixed_material.comp.spv"
                    ))),
                },
                crate::render::MaterialCode {
                    display: include_bytes!(concat!(env!("OUT_DIR"), "/mixed_toon_ldr.comp.spv")),
                    linear: Some(include_bytes!(concat!(
                        env!("OUT_DIR"),
                        "/mixed_toon.comp.spv"
                    ))),
                },
            );
        }
        Ok(Some((Self { producers, hdr }, camera)))
    }
}
impl Technique for LightStack {
    fn prepare_feature(
        &self,
        feature: &str,
        world: &World,
        tasks: &rayon::ThreadPool,
    ) -> Result<()> {
        if let Some(layer) = self
            .producers
            .iter()
            .find(|l| l.registration.feature == feature)
        {
            layer.prepare(world, tasks)?;
        }
        Ok(())
    }
    fn color_revision(&self, scene: &crate::render::RenderScene) -> u64 {
        super::composition::color_revision(scene)
    }
    fn needs_surface_motion(&self, s: &Snapshot) -> bool {
        self.active(s).any(|p| p.needs_motion(s))
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
            |_, _| Ok(()),
            |g, view| super::draw_visibility_overlays(g, s, view.ids, view.color),
        )
    }
}
impl LightStack {
    pub fn uses_shadow_rays(&self, s: &Snapshot) -> bool {
        self.active(s).any(|p| p.uses_shadow_rays())
    }
    pub fn layers<'a>(
        &'a self,
        s: &'a Snapshot,
        surfaces: &[PosedSurface<'_>],
    ) -> Result<Vec<AddLight<'a>>> {
        let mut layers = Vec::new();
        for producer in self.active(s) {
            layers.extend(producer.prepare(s, surfaces)?);
        }
        Ok(layers)
    }
}
pub(super) fn draw_surfaces(
    g: &mut FrameGraph,
    t: &mut crate::render::Targets,
    s: &Snapshot,
    surfaces: Vec<PosedSurface<'_>>,
) -> Result<()> {
    let mut draws = crate::render::OrdinaryDrawList::default();
    for surface in surfaces {
        let mut item = t.material(s, surface.instance, Mat4::IDENTITY)?;
        if surface.instance.emitter {
            item.params[3] = 1.;
        }
        surface.draw(g, t, s, item, &mut draws)?;
    }
    draws.emit(g, t, s)
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
        |_, _| Ok(()),
        |_, _| Ok(()),
    )
}
pub(super) fn build_modules(
    s: &Snapshot,
    geometry: impl FnOnce(&mut FrameGraph, &mut crate::render::Targets) -> Result<()>,
    layers: Vec<AddLight<'_>>,
    hdr: bool,
    scene: impl FnOnce(&mut FrameGraph, crate::render::SceneLayers) -> Result<()>,
    display: impl FnOnce(&mut FrameGraph, crate::render::DisplayLayers) -> Result<()>,
) -> Result<FrameGraph> {
    let layers = stages::order(layers, &s.materials)?;
    let sky = Cell::new(None);
    build_frame_with_lighting(
        s,
        Atlas::cached(&s.scene)?,
        geometry,
        |g, inputs| {
            let mut materials = Vec::new();
            let mut signals = stages::Products::default();
            for layer in layers {
                signals.run(layer, g, inputs)?;
            }
            if let Some((resources, Some(draw))) = signals.environment {
                sky.set(Some((draw, resources)));
            }
            let mut neutral = std::collections::HashMap::new();
            for (bin, name, inputs, optional) in s.materials.lighting() {
                let mut bindings = Vec::with_capacity(inputs.len());
                for &input in inputs {
                    let resource = signals.material_input(input);
                    let resource = match resource {
                        Some(resource) => resource,
                        // Mixed kernels detect disabled controls or an empty irradiance buffer.
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
        |g, mut inputs| {
            if let Some((draw, resources)) = sky.get() {
                draw(g, inputs.ids, inputs.color.resource, resources, hdr);
                inputs.has_backdrop = true;
            }
            scene(g, inputs)
        },
        display,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::LightingInput::{Environment, EnvironmentControls};

    #[test]
    fn rejects_a_display_only_material_in_a_linear_composition() -> Result<()> {
        let world = World::demo();
        let mut materials = MaterialPrograms::default();
        materials.register("Packed-only shader", ibl::REGISTRATION.material.display)?;
        let view = Snapshot::new(&world, Camera::default(), [16, 16], Arc::new(materials))?;
        let error = build_modules(
            &view,
            |_, _| Ok(()),
            Vec::new(),
            true,
            |_, _| Ok(()),
            |_, _| Ok(()),
        )
        .err()
        .expect("incompatible output format");
        assert!(
            error
                .to_string()
                .contains("Packed-only shader has no linear HDR material kernel")
        );
        Ok(())
    }

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
        let build = |layers| {
            build_modules(
                &view,
                |_, _| Ok(()),
                layers,
                false,
                |_, _| Ok(()),
                |_, _| Ok(()),
            )
        };
        let error = build(Vec::new()).err().expect("missing producer");
        assert!(
            error
                .to_string()
                .contains("Independent material requires a producer for Environment")
        );
        let source = AddLight::new("test environment", &[], Signal::Environment, |g, _, _| {
            Ok(Contribution::Environment(
                (
                    g.input("Custom tables", vec![0; 16]),
                    g.input("Custom controls", vec![0; 112]),
                ),
                None,
            ))
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
