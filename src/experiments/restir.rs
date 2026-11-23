use super::light_stack::{AddLight, Channel, Contribution, Producer};
use crate::render::FrameGraph;
mod diffuse_gi;
pub(super) mod graph;
mod ray_scene;
pub(super) mod studies;
pub use studies::{forest_study, karst_study, river_shadow_study, silver_study};
mod scene;
pub use scene::scene;

use crate::{
    engine::Engine,
    render::{Camera, PosedSurface, Snapshot},
    scene::{Id, World},
};
use anyhow::{Context, Result, bail, ensure};
use glam::Vec3;
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub const SETTING: &str = "lab.restir";
const PROGRAM: &str = "Many-light diffuse";
const LIGHT_MAX: u32 = 65536;
static GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(u32)]
enum Sampling {
    Uniform,
    Ris,
    Restir,
    Reference,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(u32)]
enum Denoise {
    Off,
    Spatial,
    Svgf,
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum LightSource {
    #[default]
    Auto,
    Scene,
    Bank,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(super) struct Settings {
    #[serde(default)]
    gi: DiffuseGi,
    lights: u32,
    candidates: u32,
    reference_samples: u32,
    ev: f32,
    sampling: Sampling,
    denoise: Denoise,
    #[serde(default)]
    light_source: LightSource,
    #[serde(default)]
    history_stats: bool,
    #[serde(
        default,
        alias = "hardware_proxy_rays",
        deserialize_with = "read_ray_backend"
    )]
    rays: RayBackend,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RayBackend {
    #[default]
    Auto,
    Hardware,
    Software,
}

fn read_ray_backend<'de, D: serde::Deserializer<'de>>(d: D) -> Result<RayBackend, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Saved {
        Backend(RayBackend),
        Legacy(bool),
    }
    Ok(match Saved::deserialize(d)? {
        Saved::Backend(backend) => backend,
        Saved::Legacy(true) => RayBackend::Hardware,
        Saved::Legacy(false) => RayBackend::Auto,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum DiffuseGi {
    #[default]
    Direct,
    Single,
    Multi,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            gi: DiffuseGi::Direct,
            lights: 16384,
            candidates: 16,
            reference_samples: 256,
            ev: 0.,
            sampling: Sampling::Restir,
            denoise: Denoise::Svgf,
            light_source: LightSource::Auto,
            history_stats: false,
            rays: RayBackend::Auto,
        }
    }
}

impl Settings {
    pub(super) fn hardware_rays(self, mega: bool) -> bool {
        self.rays == RayBackend::Hardware || (self.rays == RayBackend::Auto && mega)
    }

    fn use_hardware(self, vegetation: Option<&serde_json::Value>) -> bool {
        self.hardware_rays(
            vegetation
                .and_then(|v| v.get("mega_shadows"))
                .and_then(serde_json::Value::as_bool)
                == Some(true),
        )
    }
    pub(super) fn indirect(self) -> bool {
        self.gi != DiffuseGi::Direct
    }
    pub(super) fn scene_lights(self, s: &Snapshot) -> bool {
        match self.light_source {
            LightSource::Auto => s.scene.lights.present,
            LightSource::Scene => true,
            LightSource::Bank => false,
        }
    }
    fn uses_history(self) -> bool {
        self.sampling == Sampling::Restir
            || self.denoise == Denoise::Svgf
            || self.gi != DiffuseGi::Direct
    }
    fn validate(self) -> Result<()> {
        ensure!(
            (1..=LIGHT_MAX).contains(&self.lights),
            "lights must be in 1..={LIGHT_MAX}"
        );
        ensure!(
            (1..=64).contains(&self.candidates),
            "candidates must be in 1..=64"
        );
        ensure!(
            (32..=512).contains(&self.reference_samples),
            "reference samples must be in 32..=512"
        );
        ensure!((-6. ..=6.).contains(&self.ev), "EV must be in [-6, 6]");
        Ok(())
    }
}

pub(super) fn read(world: &World) -> Result<(Id, Settings)> {
    let (id, raw) = world
        .first_raw_component(SETTING)
        .context("scene has no ReSTIR experiment settings")?;
    Ok((id, decode(raw)?))
}

pub(super) fn decode(raw: &serde_json::Value) -> Result<Settings> {
    let cfg = Settings::deserialize(raw)?;
    cfg.validate()?;
    Ok(cfg)
}

pub(super) fn camera(view: &str) -> Result<Camera> {
    let (target, distance, yaw, pitch) = match view {
        "overview" => ([0., 1.48, -4.25], 13.6, 0.075, 0.14),
        "contact" => ([0.2, 0.85, -1.6], 7.4, 0.48, 0.29),
        _ => bail!("restir camera overview/contact"),
    };
    Ok(Camera {
        target: Vec3::from_array(target),
        distance,
        yaw,
        pitch,
    })
}

pub(super) fn open_producer(world: &World, _: &rayon::ThreadPool) -> Result<Box<dyn Producer>> {
    read(world)?;
    Ok(Box::new(Restir::new()))
}
pub fn command(engine: &mut Engine, args: &str) -> Result<String> {
    let (id, mut cfg) = read(&engine.world)?;
    let words: Vec<_> = args.split_whitespace().collect();
    let mut selected = args;
    match words.as_slice() {
        ["gi", "direct"] => cfg.gi = DiffuseGi::Direct,
        ["gi", "single"] => cfg.gi = DiffuseGi::Single,
        ["gi", "multi"] => cfg.gi = DiffuseGi::Multi,
        ["uniform"] => cfg.sampling = Sampling::Uniform,
        ["ris"] => cfg.sampling = Sampling::Ris,
        ["reuse"] => cfg.sampling = Sampling::Restir,
        ["rays" | "proxy-rays", "hardware"] => cfg.rays = RayBackend::Hardware,
        ["rays" | "proxy-rays", "software"] => cfg.rays = RayBackend::Software,
        ["rays", "toggle"] => {
            let vegetation = engine.world.first_raw_component(super::vegetation::SETTING);
            (cfg.rays, selected) = if cfg.use_hardware(vegetation.map(|(_, value)| value)) {
                (RayBackend::Software, "rays software")
            } else {
                (RayBackend::Hardware, "rays hardware")
            };
        }
        ["reference"] => cfg.sampling = Sampling::Reference,
        ["denoise", "off"] => cfg.denoise = Denoise::Off,
        ["denoise", "spatial"] => cfg.denoise = Denoise::Spatial,
        ["denoise", "svgf"] => cfg.denoise = Denoise::Svgf,
        ["lights", count] => cfg.lights = count.parse()?,
        ["source", "auto"] => cfg.light_source = LightSource::Auto,
        ["source", "scene"] => cfg.light_source = LightSource::Scene,
        ["source", "bank"] => cfg.light_source = LightSource::Bank,
        ["candidates", count] => cfg.candidates = count.parse()?,
        ["samples", count] => cfg.reference_samples = count.parse()?,
        ["ev", ev] => cfg.ev = ev.parse()?,
        ["history-stats", "on"] => cfg.history_stats = true,
        ["history-stats", "off"] => cfg.history_stats = false,
        ["camera", view] => engine.camera = camera(view)?,
        _ => bail!(
            "restir gi direct/single/multi · restir rays hardware/software/toggle · restir uniform/ris/reuse/reference · restir denoise off/spatial/svgf · restir source auto/scene/bank · restir lights N · restir candidates N · restir samples N · restir ev EV · restir camera overview/contact · restir history-stats on/off"
        ),
    }
    cfg.validate()?;
    Arc::make_mut(&mut engine.world)
        .entities
        .get_mut(&id)
        .unwrap()
        .components
        .insert(SETTING.into(), serde_json::to_value(cfg)?);
    Ok(format!("restir {selected}"))
}

pub(super) struct Restir {
    lights: Arc<[u8]>,
    generation: u64,
    pub(super) shadow: graph::ShadowCache,
}
impl Restir {
    pub(super) fn new() -> Self {
        Self {
            lights: scene::lights(),
            generation: GENERATION.fetch_add(1, Ordering::Relaxed),
            shadow: Default::default(),
        }
    }
}
pub(super) const REGISTRATION: super::light_stack::Registration =
    super::light_stack::Registration {
        key: SETTING,
        feature: "restir",
        inputs: crate::render::LightingInput::DIRECT_DIFFUSE,
        material: super::light_stack::MaterialShader {
            name: PROGRAM,
            display: include_bytes!(concat!(env!("OUT_DIR"), "/restir_material.comp.spv")),
            linear: include_bytes!(concat!(env!("OUT_DIR"), "/restir_material_hdr.comp.spv")),
        },
        open: open_producer,
        camera: || camera("overview"),
    };
impl Producer for Restir {
    fn uses_shadow_rays(&self) -> bool {
        true
    }
    fn needs_motion(&self, s: &Snapshot) -> bool {
        s.scene
            .setting(SETTING)
            .and_then(|v| decode(v).ok())
            .is_some_and(Settings::uses_history)
    }

    fn prepare<'a>(
        &'a self,
        s: &'a Snapshot,
        surfaces: &[PosedSurface<'_>],
    ) -> Result<AddLight<'a>> {
        let cfg = decode(
            s.scene
                .setting(SETTING)
                .context("missing ReSTIR settings")?,
        )?;
        self.prepare_with(s, surfaces, cfg)
    }
}
impl Restir {
    fn prepare_with<'a>(
        &'a self,
        s: &'a Snapshot,
        surfaces: &[PosedSurface<'_>],
        cfg: Settings,
    ) -> Result<AddLight<'a>> {
        let rigid = (!cfg.use_hardware(s.scene.setting(super::vegetation::SETTING)))
            .then(|| self.shadow.get(&s.scene, surfaces))
            .transpose()?;
        Ok(Box::new(move |g, inputs| {
            let (direct, controls, indirect) =
                graph::add_light(g, s, self, cfg, inputs, rigid.as_deref())?;
            Ok(Contribution {
                channel: Channel::Direct,
                resources: (direct, controls),
                indirect_diffuse: Some(indirect),
                backdrop: None,
            })
        }))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{render::MaterialPrograms, rhi::Backend};

    fn snapshot() -> Result<Snapshot> {
        let mut materials = MaterialPrograms::default();
        materials.register_lighting(
            PROGRAM,
            include_bytes!(concat!(env!("OUT_DIR"), "/restir_material.comp.spv")),
            REGISTRATION.inputs,
        )?;
        Snapshot::new(
            &scene()?,
            camera("overview")?,
            [127, 79],
            Arc::new(materials),
        )
    }

    fn temporal_technique() -> Result<Arc<dyn crate::render::Technique>> {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        Ok(super::super::light_stack::LightStack::open(
            &scene()?,
            &mut MaterialPrograms::default(),
            &pool,
        )?
        .unwrap()
        .0)
    }

    #[test]
    fn declares_the_history_and_shader_dependencies() -> Result<()> {
        let mut snapshot = snapshot()?;
        let technique = Restir {
            lights: scene::lights(),
            generation: 1,
            shadow: Default::default(),
        };
        let active = temporal_technique()?;
        crate::render::post::Sequence::default().assign(Default::default(), &active, &mut snapshot);
        assert_eq!(snapshot.temporal.as_ref().unwrap().jitter, [0.; 2]);
        let plan = graph::build(&snapshot, &technique, Settings::default())?.compile()?;
        let step = |prefix| {
            plan.steps()
                .iter()
                .find(|step| plan.graph().passes[step.index].name.starts_with(prefix))
                .unwrap()
        };
        let temporal = step("ReSTIR: initial");
        let spatial = step("ReSTIR: spatial");
        let shadow = step("Trace one");
        let moments = step("SVGF:");
        let commit = step("Store results");
        let last_filter = step("À-trous denoise step 4");
        let lookup = step("Validate ReSTIR surface");
        assert!(
            lookup
                .parents
                .contains(&step("Resolve visibility motion").index)
        );
        assert!(temporal.parents.contains(&lookup.index));
        assert!(moments.parents.contains(&lookup.index));
        assert!(
            !plan
                .graph()
                .passes
                .iter()
                .any(|p| p.name == "Temporal AA resolve")
        );
        assert!(spatial.parents.contains(&temporal.index));
        assert!(shadow.parents.contains(&spatial.index));
        assert!(moments.parents.contains(&shadow.index));
        assert!(commit.parents.contains(&moments.index));
        assert!(
            step("Shade Many-light diffuse")
                .parents
                .contains(&last_filter.index)
        );
        Ok(())
    }

    #[test]
    fn motion_is_shared_on_demand_and_jitter_follows_the_post_backend() -> Result<()> {
        use crate::render::{
            SceneExtractor,
            post::{self, Mode},
        };
        let mut world = scene()?;
        let (entity, mut cfg) = read(&world)?;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        let mut programs = MaterialPrograms::default();
        let active = super::super::light_stack::LightStack::open(&world, &mut programs, &pool)?
            .unwrap()
            .0;
        let programs = Arc::new(programs);
        let mut extractor = SceneExtractor::default();
        let mut sequence = post::Sequence::default();
        let mut post = post::Settings::default();
        let mut camera = camera("overview")?;
        for (sampling, denoise, taa, cut) in [
            (Sampling::Ris, Denoise::Spatial, false, false),
            (Sampling::Restir, Denoise::Off, false, false),
            (Sampling::Ris, Denoise::Svgf, false, false),
            (Sampling::Restir, Denoise::Svgf, true, false),
            (Sampling::Restir, Denoise::Svgf, true, true),
        ] {
            cfg.sampling = sampling;
            cfg.denoise = denoise;
            world
                .entities
                .get_mut(&entity)
                .unwrap()
                .components
                .insert(SETTING.into(), serde_json::to_value(cfg)?);
            post.select(if taa { Mode::Taa } else { Mode::Off });
            if cut {
                camera.target.x += 100.;
            }
            let mut view = extractor.snapshot(&world, camera, [64, 48], programs.clone())?;
            sequence.assign(post, &active, &mut view);
            assert_eq!(view.temporal.is_some(), cfg.uses_history() || taa);
            if let Some(frame) = &view.temporal {
                assert_eq!(frame.jitter != [0.; 2], taa);
                if cut {
                    assert!(frame.previous_scene.is_none());
                }
            }
            let graph = active.build(&view)?;
            let passes = |name| graph.passes.iter().filter(|p| p.name == name).count();
            assert_eq!(
                passes("Resolve visibility motion"),
                usize::from(cfg.uses_history() || taa)
            );
            assert_eq!(
                passes("Validate ReSTIR surface correspondence"),
                usize::from(cfg.uses_history())
            );
            assert_eq!(passes("Temporal AA resolve"), usize::from(taa));
            graph.compile()?;
        }
        Ok(())
    }
    #[test]
    #[ignore = "requires Vulkan 1.4 and VK_EXT_descriptor_heap"]
    fn scene_lights_drive_restir_and_can_be_disabled() -> Result<()> {
        use crate::{
            render::SceneExtractor,
            scene::{Entity, Light},
        };
        let mut world = scene()?;
        let light = world.spawn(Entity {
            local: Light::aiming(Vec3::new(0.3, -1., -0.25))?,
            light: Some(Light::directional([1., 0.6, 0.2], 2.)),
            ..Default::default()
        });
        let mut materials = MaterialPrograms::default();
        let technique = Restir::new();
        materials.register_lighting(PROGRAM, REGISTRATION.material.display, REGISTRATION.inputs)?;
        let materials = Arc::new(materials);
        let mut extractor = SceneExtractor::default();
        let mut gpu = Backend::open()?;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
        let mut draw = |world: &World, light_source| {
            let view =
                extractor.snapshot(world, camera("overview")?, [127, 79], materials.clone())?;
            let cfg = Settings {
                light_source,
                sampling: Sampling::Reference,
                denoise: Denoise::Off,
                lights: 32,
                ..Default::default()
            };
            gpu.execute(graph::build(&view, &technique, cfg)?.compile()?, &pool)
        };
        let lit = draw(&world, LightSource::Auto)?;
        assert_eq!(lit.pixels, draw(&world, LightSource::Scene)?.pixels);
        world
            .entities
            .get_mut(&light)
            .unwrap()
            .light
            .as_mut()
            .unwrap()
            .enabled = false;
        let dark = draw(&world, LightSource::Auto)?;
        assert_eq!(dark.pixels, draw(&world, LightSource::Scene)?.pixels);
        assert!(
            lit.pixels
                .chunks_exact(4)
                .zip(dark.pixels.chunks_exact(4))
                .filter(|(bright, dim)| bright[2] > dim[2].saturating_add(8))
                .count()
                > 100
        );
        assert_ne!(dark.pixels, draw(&world, LightSource::Bank)?.pixels);
        Ok(())
    }

    #[test]
    #[ignore = "requires Vulkan 1.4 and VK_EXT_descriptor_heap"]
    fn animated_shadow_casters_reuse_rigid_bvh_and_cast_shadows() -> Result<()> {
        use crate::{
            rdg::Storage,
            render::SceneExtractor,
            scene::{Entity, Instance, Skin},
        };
        use glam::Mat4;
        let mut world = scene()?;
        let mut mesh = super::super::lighting::scenes::cube();
        mesh.joints = vec![[0; 4]; mesh.positions.len()];
        mesh.weights = vec![[1., 0., 0., 0.]; mesh.positions.len()];
        let mesh = world.spawn(Entity {
            mesh: Some(Arc::new(mesh)),
            ..Default::default()
        });
        let joint = world.spawn(Entity {
            local: Mat4::from_scale_rotation_translation(
                Vec3::new(2.4, 0.35, 2.4),
                glam::Quat::IDENTITY,
                Vec3::new(-0.15, 2., -2.),
            ),
            ..Default::default()
        });
        let skin = world.spawn(Entity {
            skin: Some(Skin {
                joints: vec![joint],
                inverse_bind: vec![Mat4::IDENTITY],
            }),
            ..Default::default()
        });
        world.spawn(Entity {
            instance: Some(Instance::new(mesh, None)),
            skin_binding: Some(skin),
            ..Default::default()
        });
        world.validate()?;
        let mut materials = MaterialPrograms::default();
        let technique = Restir::new();
        materials.register_lighting(PROGRAM, REGISTRATION.material.display, REGISTRATION.inputs)?;
        let materials = Arc::new(materials);
        let mut extractor = SceneExtractor::default();
        let camera = camera("overview")?;
        let cfg = Settings {
            lights: 32,
            sampling: Sampling::Reference,
            denoise: Denoise::Off,
            ..Default::default()
        };
        let mut build = |world: &World| {
            graph::build(
                &extractor.snapshot(world, camera, [193, 121], materials.clone())?,
                &technique,
                cfg,
            )
        };
        let before = build(&world)?;
        world.entities.get_mut(&joint).unwrap().local.w_axis.x += 0.65;
        let after = build(&world)?;
        fn source(graph: &FrameGraph, name: &str) -> Arc<[u8]> {
            let (
                _,
                Storage::Buffer {
                    initial: Some(data),
                    ..
                },
            ) = graph.resources.iter().find(|(n, _)| n == name).unwrap()
            else {
                panic!("expected initialized buffer");
            };
            data.clone()
        }
        for name in [
            "Many-light shadow BVH",
            "Shadow caster triangles",
            "Resident mesh vertices",
        ] {
            assert!(
                Arc::ptr_eq(&source(&before, name), &source(&after, name)),
                "{name}"
            );
        }
        assert!(
            after
                .passes
                .iter()
                .any(|p| p.name == "Gather shared ray triangles")
        );
        // Disable only traversal of the deformed BVH and compare lit pixels below.
        let mut without_skinned_shadows = build(&world)?;
        let (
            _,
            Storage::Buffer {
                initial: Some(controls),
                ..
            },
        ) = without_skinned_shadows
            .resources
            .iter_mut()
            .find(|(name, _)| name == "ReSTIR controls")
            .unwrap()
        else {
            unreachable!()
        };
        Arc::make_mut(controls)[40..44].fill(0);

        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
        let mut gpu = Backend::open()?;
        let before = gpu.execute(before.compile()?, &pool)?;
        let after = gpu.execute(after.compile()?, &pool)?;
        assert!(after.profile.resident_upload_bytes < before.profile.resident_upload_bytes);
        let no_shadow = gpu.execute(without_skinned_shadows.compile()?, &pool)?;
        assert!(
            after
                .pixels
                .chunks_exact(4)
                .zip(no_shadow.pixels.chunks_exact(4))
                .filter(|(with, without)| with != without)
                .count()
                > 20
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires Vulkan 1.4 and VK_EXT_descriptor_heap"]
    fn temporal_reuse_and_filter_approach_the_all_lights_reference() -> Result<()> {
        let mut snapshot = snapshot()?;
        let technique = Restir {
            lights: scene::lights(),
            generation: 1,
            shadow: Default::default(),
        };
        let restarted = Restir {
            lights: technique.lights.clone(),
            generation: 2,
            shadow: Default::default(),
        };
        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
        let mut gpu = Backend::open()?;
        let active = temporal_technique()?;
        let mut sequence = crate::render::post::Sequence::default();
        let mut draw = |technique: &Restir, cfg| {
            sequence.assign(Default::default(), &active, &mut snapshot);
            gpu.execute(graph::build(&snapshot, technique, cfg)?.compile()?, &pool)
        };
        let mut cfg = Settings {
            lights: 128,
            sampling: Sampling::Reference,
            denoise: Denoise::Off,
            ..Default::default()
        };
        let reference = draw(&technique, cfg)?;
        cfg.sampling = Sampling::Uniform;
        let uniform = draw(&technique, cfg)?;
        cfg.sampling = Sampling::Restir;
        cfg.denoise = Denoise::Svgf;
        let cold = draw(&technique, cfg)?;
        let mut warm = None;
        for _ in 0..10 {
            warm = Some(draw(&technique, cfg)?);
        }
        let warm = warm.unwrap();
        let error = |pixels: &[u8]| {
            pixels
                .iter()
                .zip(&reference.pixels)
                .map(|(&a, &b)| {
                    let d = i64::from(a) - i64::from(b);
                    d * d
                })
                .sum::<i64>()
        };
        assert!(error(&warm.pixels) < error(&uniform.pixels) / 3);
        assert_ne!(cold.pixels, warm.pixels);
        let reset = draw(&restarted, cfg)?;
        assert_eq!(
            cold.pixels, reset.pixels,
            "scene setup must reset the GPU history"
        );
        Ok(())
    }
}
