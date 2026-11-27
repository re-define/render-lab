//! Assembly of independently configured raster geometry, lighting and overlays.
use super::{
    cartoon, clusters, effects,
    light_stack::{self, LightStack},
    motion, restir, vegetation,
};
use crate::{
    engine::Engine,
    render::FrameGraph,
    render::{
        Camera, Forward, MaterialPrograms, PosedSurface, RenderInstance, Snapshot, Targets,
        Technique,
    },
    scene::*,
};
use anyhow::{Context, Result, bail, ensure};
mod scene;
use glam::Vec3;
pub use scene::scene;
use std::sync::Arc;
pub(super) const SETTING: &str = "lab.composition";
const LAYER: &str = "lab.composition.layer";
pub(crate) const HIDDEN: &str = "lab.render.hidden";

pub(super) fn is_scene(world: &World) -> bool {
    world.first_raw_component(SETTING).is_some()
}

pub(super) fn enabled(world: &World, layer: &str) -> bool {
    layer_enabled(world.first_raw_component(SETTING).map(|(_, v)| v), layer)
}

pub(super) fn snapshot_layer_enabled(
    context: &mut crate::render::SnapshotContext<'_>,
    layer: &str,
) -> bool {
    layer_enabled(context.first_setting(SETTING), layer)
}

pub(super) fn color_revision(scene: &crate::render::RenderScene) -> u64 {
    let layout = scene
        .setting(SETTING)
        .and_then(|cfg| cfg["color_revision"].as_u64())
        .unwrap_or(0);
    scene
        .setting(restir::SETTING)
        .map_or(layout, |raw| restir::color_revision(raw, layout))
}

pub(super) fn light_revision(scene: &crate::render::RenderScene, layer: &str) -> u64 {
    scene
        .setting(SETTING)
        .and_then(|cfg| cfg["light_revisions"][layer].as_u64())
        .unwrap_or(0)
}

pub(super) fn layer_enabled(settings: Option<&serde_json::Value>, layer: &str) -> bool {
    settings
        .and_then(|v| v.get(layer))
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}
pub(super) type Configured = (Arc<dyn Technique>, Camera);
pub(super) struct SurfaceRoute {
    pub name: &'static str,
    pub owns: fn(&Snapshot, &RenderInstance) -> bool,
    pub draw: fn(
        &mut FrameGraph,
        &mut Targets,
        &Snapshot,
        &RenderInstance,
        &mut GeometryOutputs,
    ) -> Result<()>,
}
type OrdinaryDraw = dyn Fn(&mut FrameGraph, &mut Targets, &Snapshot, Vec<PosedSurface<'_>>) -> Result<()>
    + Send
    + Sync;

#[derive(Default)]
pub(super) struct GeometryModules {
    cluster: Option<Arc<clusters::ClusterLod>>,
    routes: Vec<SurfaceRoute>,
    ordinary: Option<Box<OrdinaryDraw>>,
    providers: Vec<Arc<dyn Technique>>,
    pub layers: Contributors,
}
impl GeometryModules {
    pub fn clustered(&mut self) {
        if self.cluster.is_none() {
            let clusters = Arc::new(clusters::ClusterLod::default());
            self.providers.push(clusters.clone());
            self.cluster = Some(clusters);
        }
    }
    pub fn route(&mut self, technique: Arc<dyn Technique>, route: SurfaceRoute) {
        self.providers.push(technique);
        self.routes.push(route);
    }
    pub fn ordinary(
        &mut self,
        technique: Arc<dyn Technique>,
        draw: Box<OrdinaryDraw>,
    ) -> Result<()> {
        ensure!(
            self.ordinary.is_none(),
            "multiple renderers for ordinary surfaces"
        );
        self.providers.push(technique);
        self.ordinary = Some(draw);
        Ok(())
    }
}
/// Visibility produces these optional records before scene-color contributors run.
#[derive(Default)]
pub(super) struct GeometryOutputs {
    pub rivers: Vec<effects::river::Reflector>,
}
#[derive(Clone, Copy, PartialOrd, Ord, PartialEq, Eq)]
pub(super) enum SceneStage {
    Reflections,
    Atmosphere,
}
type ScenePass =
    fn(&mut FrameGraph, &Snapshot, crate::render::SceneLayers, &GeometryOutputs) -> Result<()>;
type DisplayPass = fn(&mut FrameGraph, &Snapshot, crate::render::DisplayLayers) -> Result<()>;
#[derive(Default)]
pub(super) struct Contributors {
    pub scene: Vec<(SceneStage, ScenePass)>,
    pub display: Vec<DisplayPass>,
}

pub(super) fn assemble(
    world: &World,
    tasks: &rayon::ThreadPool,
    materials: &mut MaterialPrograms,
    geometry: GeometryModules,
    camera: Option<Camera>,
) -> Result<Configured> {
    let GeometryModules {
        cluster,
        routes,
        ordinary,
        mut layers,
        mut providers,
    } = geometry;
    layers.scene.sort_by_key(|(stage, _)| *stage);
    let lights = LightStack::open_modules(world, materials, tasks, !providers.is_empty())?;
    let (lights, light_camera) = lights.map_or((None, None), |(l, c)| (Some(l), Some(c)));
    let camera = if let Some(c) = restir::studies::camera(world)? {
        c
    } else if is_scene(world) {
        Camera {
            target: Vec3::new(0., 1.15, 0.),
            distance: 10.8,
            yaw: 0.32,
            pitch: 0.25,
        }
    } else if let Some(c) = camera.or(light_camera) {
        c
    } else {
        Camera::fit(world)?
    };
    let technique: Arc<dyn Technique> = match (providers.len(), lights) {
        (0, Some(lights)) => Arc::new(lights),
        (0, None) => Arc::new(Forward::default()),
        (1, None) => providers.pop().unwrap(),
        (_, lights) => Arc::new(Modules {
            cluster,
            routes,
            ordinary,
            lights,
            layers,
        }),
    };
    Ok((technique, camera))
}
struct Modules {
    cluster: Option<Arc<clusters::ClusterLod>>,
    routes: Vec<SurfaceRoute>,
    ordinary: Option<Box<OrdinaryDraw>>,
    lights: Option<LightStack>,
    layers: Contributors,
}
impl Technique for Modules {
    fn color_revision(&self, scene: &crate::render::RenderScene) -> u64 {
        color_revision(scene)
    }
    fn prepare_feature(
        &self,
        feature: &str,
        world: &World,
        tasks: &rayon::ThreadPool,
    ) -> Result<()> {
        if let Some(stack) = &self.lights {
            stack.prepare_feature(feature, world, tasks)?;
        }
        Ok(())
    }
    fn needs_surface_motion(&self, s: &Snapshot) -> bool {
        self.lights
            .as_ref()
            .is_some_and(|l| l.needs_surface_motion(s))
    }
    fn build(&self, s: &Snapshot) -> Result<FrameGraph> {
        let candidates: Vec<_> = if self.cluster.is_some() {
            s.scene
                .routes
                .ordinary
                .iter()
                .map(|&slot| &s.scene.instances[slot])
                .collect()
        } else {
            s.scene.instances.iter().collect()
        };
        let visits = candidates.len();
        let mut custom = Vec::new();
        let mut ordinary = Vec::new();
        for instance in candidates {
            let mut routes = self.routes.iter().filter(|route| (route.owns)(s, instance));
            if let Some(route) = routes.next() {
                if let Some(other) = routes.next() {
                    bail!(
                        "both {} and {} claim instance {:?}",
                        route.name,
                        other.name,
                        instance.id
                    );
                }
                custom.push((route, instance));
            } else {
                ordinary.push(instance);
            }
        }
        let ordinary = Forward::default().prepare_surfaces(s, ordinary)?;
        let layers = self
            .lights
            .as_ref()
            .map(|l| l.layers(s, &ordinary))
            .transpose()?
            .unwrap_or_default();
        let outputs = std::cell::RefCell::new(GeometryOutputs::default());
        light_stack::build_modules(
            s,
            |g, t| {
                g.products.request_ray_proxies |=
                    self.lights.as_ref().is_some_and(|l| l.uses_shadow_rays(s));
                g.products.instance_visits += visits;
                let draw = |g: &mut FrameGraph, t: &mut crate::render::Targets| {
                    if let Some(draw) = &self.ordinary {
                        draw(g, t, s, ordinary)?;
                    } else {
                        light_stack::draw_surfaces(g, t, s, ordinary)?;
                    }
                    let mut outputs = outputs.borrow_mut();
                    for (route, instance) in custom {
                        (route.draw)(g, t, s, instance, &mut outputs)?;
                    }
                    Ok(())
                };
                if let Some(clusters) = &self.cluster {
                    clusters.draw_with(g, t, s, draw)
                } else {
                    draw(g, t)
                }
            },
            layers,
            self.lights.as_ref().is_none_or(|lights| lights.hdr),
            |g, inputs| {
                let products = outputs.borrow();
                for (_, draw) in &self.layers.scene {
                    draw(g, s, inputs, &products)?;
                }
                Ok(())
            },
            |g, inputs| {
                for draw in &self.layers.display {
                    draw(g, s, inputs)?;
                }
                super::draw_visibility_overlays(g, s, inputs.ids, inputs.color)
            },
        )
    }
}
pub fn command(engine: &mut Engine, arg: &str) -> Result<String> {
    let args: Vec<_> = arg.split_whitespace().collect();
    let [layer, state] = args.as_slice() else {
        bail!("feature forest/water/actor/ibl/restir on/off");
    };
    ensure!(
        ["forest", "water", "actor"].contains(layer)
            || light_stack::REGISTRY.iter().any(|r| r.feature == *layer),
        "unknown scene feature"
    );
    let active = match *state {
        "on" => true,
        "off" => false,
        _ => bail!("expected on/off"),
    };
    let (id, config) = engine
        .world
        .first_raw_component(SETTING)
        .context("open the confluence scene first")?;
    if enabled(&engine.world, layer) == active {
        return Ok(format!("{layer} {state}"));
    }
    let mut config = config.clone();
    config[*layer] = active.into();
    if let Some(registration) = light_stack::REGISTRY.iter().find(|r| r.feature == *layer) {
        if active {
            ensure!(
                engine.world.first_raw_component(registration.key).is_some(),
                "scene is missing {} for {layer}",
                registration.key
            );
            if let Some(check) = registration.validate_device {
                check(&engine.world, engine.ray_caps)?;
            }
            engine
                .technique
                .prepare_feature(layer, &engine.world, &engine.tasks)?;
        }
        let revision = config["color_revision"]
            .as_u64()
            .unwrap_or(0)
            .wrapping_add(1);
        config["color_revision"] = revision.into();
        config["light_revisions"][*layer] = revision.into();
        Arc::make_mut(&mut engine.world).set_setting(id, SETTING, config);
        return Ok(format!("{layer} {state}"));
    }
    let mut world = (*engine.world).clone();
    world.clocks = engine.schedule.checkpoints(&world);
    world.set_setting(id, SETTING, config);
    for e in world
        .entities
        .values_mut()
        .filter(|e| e.components.get(LAYER).and_then(|v| v.as_str()) == Some(*layer))
    {
        if active {
            e.components.remove(HIDDEN);
        } else {
            e.components.insert(HIDDEN.into(), true.into());
        }
    }
    world.validate()?;
    let prepared = super::SceneConfiguration::prepare(&world, &engine.tasks, engine.ray_caps)?;
    engine.world = Arc::new(world);
    prepared.apply_layers(engine);
    Ok(format!("{layer} {state}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::post::{Mode, Sequence, Settings};

    #[test]
    #[ignore = "compares three actual Vulkan readbacks from the confluence scene"]
    fn ibl_hot_switch_changes_submitted_frames_at_fixed_time() -> Result<()> {
        use std::time::{Duration, Instant};
        let mut engine = super::super::open(scene()?)?;
        engine.select_post(Mode::Off)?;
        command(&mut engine, "restir off")?;
        command(&mut engine, "water off")?;
        let mut pixels = Vec::new();
        for (state, label) in [("ibl on", "on"), ("ibl off", "off"), ("ibl on", "restored")] {
            command(&mut engine, state)?;
            for sample in 0..2 {
                ensure!(engine.tick(0., [480, 300])?, "frame admission");
                let deadline = Instant::now() + Duration::from_secs(60);
                let frame = loop {
                    if let Some(frame) = engine.take_frame() {
                        break frame.map_err(anyhow::Error::msg)?;
                    }
                    ensure!(Instant::now() < deadline, "GPU frame timed out");
                    std::thread::sleep(Duration::from_millis(1));
                };
                assert_eq!(frame.pixels.len(), 480 * 300 * 4);
                if sample == 1 {
                    if let Some(dir) = std::env::var_os("RENDER_LAB_COMPARISON_DIR") {
                        let dir = std::path::Path::new(&dir);
                        std::fs::create_dir_all(dir)?;
                        let mut rgba = frame.pixels.clone();
                        for pixel in rgba.chunks_exact_mut(4) {
                            pixel.swap(0, 2);
                        }
                        image::save_buffer(
                            dir.join(format!("confluence_ibl_{label}.png")),
                            &rgba,
                            480,
                            300,
                            image::ColorType::Rgba8,
                        )?;
                    }
                    pixels.push(frame.pixels);
                }
            }
        }
        let different = |a: &[u8], b: &[u8]| {
            a.chunks_exact(4)
                .zip(b.chunks_exact(4))
                .filter(|(a, b)| a[..3] != b[..3])
                .count()
        };
        let off = different(&pixels[0], &pixels[1]);
        let restored = different(&pixels[0], &pixels[2]);
        // The toon actor receives the environment while the vegetation uses its own
        // material. Count actual illuminated pixels going dark and being relit.
        let mut darkened = 0;
        let mut relit = 0;
        for ((on, off), again) in pixels[0]
            .chunks_exact(4)
            .zip(pixels[1].chunks_exact(4))
            .zip(pixels[2].chunks_exact(4))
        {
            if on[..3].iter().any(|c| *c >= 10) && off[..3] == [0, 0, 0] {
                darkened += 1;
                relit += usize::from(
                    on[..3]
                        .iter()
                        .zip(&again[..3])
                        .all(|(a, b)| a.abs_diff(*b) <= 2),
                );
            }
        }
        println!(
            "pixels={} changed_with_ibl_off={off} changed_after_restoring_ibl={restored} lit_to_black={darkened} recovered_lighting={relit}",
            480 * 300
        );
        assert!(
            darkened >= 100,
            "no material visibly lost its environment lighting"
        );
        assert!(
            relit * 100 >= darkened * 99 && restored * 10 < off,
            "lighting did not return to its original appearance"
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires confluence assets and a Vulkan device"]
    fn switches_contributors_independently_and_preserves_the_camera() -> Result<()> {
        let mut engine = super::super::open(scene()?)?;
        let camera = engine.camera;
        let mut extractor = crate::render::SceneExtractor::default();
        let mut sequence = Sequence::default();
        let mut settings = Settings::default();
        settings.select(Mode::Taa);
        for (cmd, state) in [
            ("", [true, true, true, true, true]),
            ("water off", [true, false, true, true, true]),
            ("forest off", [false, false, true, true, true]),
            ("restir off", [false, false, true, true, false]),
            ("ibl off", [false, false, true, false, false]),
            ("actor off", [false, false, false, false, false]),
            ("restir on", [false, false, false, false, true]),
            ("water on", [false, true, false, false, true]),
            ("actor on", [false, true, true, false, true]),
            ("forest on", [true, true, true, false, true]),
            ("ibl on", [true, true, true, true, true]),
        ] {
            let light_change = cmd.starts_with("ibl ") || cmd.starts_with("restir ");
            let mut before_switch = None;
            if !cmd.is_empty() {
                if light_change {
                    engine
                        .schedule
                        .tick(&mut engine.world, 1. / 60., &engine.tasks)?;
                    let mut prior = extractor.snapshot(
                        &engine.world,
                        camera,
                        [224, 140],
                        engine.materials.clone(),
                    )?;
                    sequence.assign(settings, &engine.technique, &mut prior);
                    before_switch = Some(prior);
                }
                let before = engine.schedule.describe();
                let technique = engine.technique.clone();
                let world = Arc::as_ptr(&engine.world);
                let start = std::time::Instant::now();
                command(&mut engine, cmd)?;
                if light_change {
                    eprintln!("{cmd}: apply_us={:.2}", start.elapsed().as_secs_f64() * 1e6);
                    assert_eq!(before, engine.schedule.describe());
                    assert_eq!(world, Arc::as_ptr(&engine.world));
                    assert!(Arc::ptr_eq(&technique, &engine.technique));
                }
            }
            let expected: Vec<_> = [
                (state[1], "Effect playback"),
                (state[0], "Vegetation wind"),
                (state[2], "Silver Wolf motion matcher"),
            ]
            .into_iter()
            .filter_map(|(active, name)| active.then_some(name))
            .collect();
            let groups: Vec<Vec<_>> = engine
                .schedule
                .stages()
                .map(|(systems, _)| systems.iter().map(|s| s.name).collect())
                .collect();
            // Frame-rate contributors share the first parallel batch. Enabling characters
            // also installs a fixed-step contact chain for actors that acquire a contact rig.
            assert_eq!(
                groups.first().map(Vec::as_slice).unwrap_or_default(),
                expected,
                "{cmd}"
            );
            let contact: Vec<_> = groups.iter().skip(1).flatten().copied().collect();
            if state[2] {
                let stage = |name| {
                    groups
                        .iter()
                        .position(|batch| batch.contains(&name))
                        .unwrap()
                };
                assert_eq!(contact.len(), 4, "{cmd}");
                let dynamics = stage("Character momentum and support impulses");
                assert!(stage("Rotating support drives") < dynamics, "{cmd}");
                assert!(stage("Locomotion and traversal intent") < dynamics, "{cmd}");
                assert!(dynamics < stage("Foot placement and fingertip IK"), "{cmd}");
            } else {
                assert!(contact.is_empty(), "{cmd}");
            }
            if cmd == "actor off" {
                let temp = tempfile::tempdir()?;
                let path = temp.path().join("confluence.json");
                engine.world.save(&path)?;
                super::super::replace(&mut engine, World::load(&path)?)?;
            }
            let start = std::time::Instant::now();
            let mut view = extractor.snapshot(
                &engine.world,
                engine.camera,
                [224, 140],
                engine.materials.clone(),
            )?;
            assert_eq!(
                view.scene.has_derived(motion::silver::PLAYER),
                state[2],
                "{cmd}"
            );
            let elapsed = start.elapsed();
            sequence.assign(settings, &engine.technique, &mut view);
            let graph = engine.technique.build(&view)?;
            if let Some(prior) = before_switch {
                eprintln!(
                    "{cmd}: extraction_us={:.2} entities={} visited={} instances_updated={}",
                    elapsed.as_secs_f64() * 1e6,
                    engine.world.entities.len(),
                    view.scene.stats.entities_visited,
                    view.scene.stats.instances_updated
                );
                assert!(!view.scene.stats.full_rebuild);
                assert_eq!(view.scene.stats.instances_updated, 0);
                assert!(Arc::ptr_eq(&view.scene.instances, &prior.scene.instances));
                assert!(Arc::ptr_eq(
                    view.temporal
                        .as_ref()
                        .unwrap()
                        .previous_scene
                        .as_ref()
                        .unwrap(),
                    &prior.scene
                ));
                let old = engine.technique.build(&prior)?;
                let history = |g: &FrameGraph, name: &str| {
                    g.resources
                        .iter()
                        .position(|(n, _)| n == name)
                        .and_then(|i| g.histories.get(&crate::rdg::Resource(i)).copied())
                };
                let name = "Temporal history color and depth";
                assert_ne!(history(&old, name), history(&graph, name));
                if cmd.starts_with("ibl ") {
                    assert_eq!(
                        light_revision(&view.scene, "restir"),
                        light_revision(&prior.scene, "restir")
                    );
                }
                command(&mut engine, cmd)?;
                assert_eq!(
                    extractor
                        .snapshot(&engine.world, camera, [224, 140], engine.materials.clone())?
                        .scene
                        .stats
                        .entities_visited,
                    0
                );
            }
            let has = |name: &str| graph.passes.iter().any(|p| p.name.contains(name));
            let resources = |name: &str| graph.resources.iter().filter(|(n, _)| n == name).count();
            assert_eq!(resources("Visibility IDs"), 1, "{cmd}");
            assert_eq!(resources("Scene depth"), 1, "{cmd}");
            assert_eq!(resources("Linear HDR scene color"), 1, "{cmd}");
            let index = |name| graph.passes.iter().position(|p| p.name == name);
            let temporal = index("Temporal AA resolve").expect("TAA");
            let tone = index("Tone map linear scene color").expect("display transform");
            assert!(temporal < tone, "{cmd}");
            if let Some(atmosphere) = index("Vegetation atmosphere") {
                assert!(atmosphere < temporal, "{cmd}");
                if let Some(sky) = index("Environment backdrop") {
                    assert!(sky < atmosphere, "{cmd}");
                }
            }
            if let Some(trajectory) = index("Motion trajectory") {
                assert!(tone < trajectory, "{cmd}");
            }
            let actual = [
                has("GPU cluster cull"),
                resources("Cascade solver parameters") > 0,
                has("GPU skin mesh"),
                resources("IBL camera, exposure and rotation") == 1,
                has("ReSTIR: initial and temporal resampling"),
            ];
            assert_eq!(actual, state, "{cmd}");
            assert_eq!(engine.camera.eye(), camera.eye());
            graph.compile()?;
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires the imported character assets and IBL environment"]
    fn river_uses_scene_radiance_and_ink_uses_reconstructed_display_pixels() -> Result<()> {
        use crate::experiments::ibl;
        use crate::rdg::{Op, Resource};
        let mut world = cartoon::scene()?;
        effects::river::install(&mut world);
        let mut env = ibl::Settings::default();
        env.sky = true;
        world.spawn(Entity {
            components: [(ibl::SETTING.into(), serde_json::to_value(env)?)].into(),
            ..Default::default()
        });
        let engine = super::super::open(world)?;
        for mode in [Mode::Taa, Mode::Dlss] {
            let mut view = Snapshot::new(
                &engine.world,
                engine.camera,
                [320, 180],
                engine.materials.clone(),
            )?;
            let mut settings = Settings::default();
            settings.select(mode);
            Sequence::default().assign(settings, &engine.technique, &mut view);
            let graph = engine.technique.build(&view)?;
            let pass = |name| graph.passes.iter().position(|p| p.name == name).unwrap();
            let sky = pass("Environment backdrop");
            let reflection = pass("Reflect visible banks in river");
            let reconstruction = pass(if mode == Mode::Taa {
                "Temporal AA resolve"
            } else {
                "NVIDIA DLSS Quality"
            });
            let tone_map = pass("Tone map linear scene color");
            let ink = pass("Image-space outlines");
            assert!(
                sky < reflection
                    && reflection < reconstruction
                    && reconstruction < tone_map
                    && tone_map < ink
            );
            let scene_color = Resource(
                graph
                    .resources
                    .iter()
                    .position(|(name, _)| name == "Linear HDR scene color")
                    .unwrap(),
            );
            assert_eq!(
                graph.passes[reflection].op.program().unwrap().bindings[2].0,
                scene_color
            );
            let display = graph.products().final_color.unwrap();
            let overlay = graph.passes[ink].op.program().unwrap();
            assert_eq!(overlay.bindings[2].0, display);
            let push: &[u32] = bytemuck::cast_slice(&overlay.push);
            assert_eq!(push[1..3], [320, 180]);
            if mode == Mode::Dlss {
                assert!(matches!(&graph.passes[reconstruction].op, Op::Dlss(d) if d.hdr));
                assert_ne!(graph.extent, graph.output_extent());
            }
            graph.compile()?;
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires Vulkan and the IBL environment"]
    fn toggling_hidden_water_preserves_linear_radiance_through_reload() -> Result<()> {
        use crate::experiments::ibl;
        use crate::rdg::Resource;
        let mut world = World::demo();
        scene::append_scenery(&mut world, effects::water()?, "water", Vec3::X * 2000.)?;
        let mut env = serde_json::to_value(ibl::Settings::default())?;
        env["exposure"] = 2.0.into();
        env["sky"] = false.into();
        world.spawn(Entity {
            components: [
                (SETTING.into(), serde_json::json!({"water":true})),
                (ibl::SETTING.into(), env),
            ]
            .into(),
            ..Default::default()
        });
        let mut engine = super::super::open(world)?;
        engine.camera = Camera {
            distance: 5.5,
            ..Camera::default()
        };
        let mut gpu = crate::rhi::Backend::open()?;
        let capture = |engine: &Engine, gpu: &mut crate::rhi::Backend| -> Result<Vec<u8>> {
            let [w, h] = [128, 80];
            let view = Snapshot::new(
                &engine.world,
                engine.camera,
                [w, h],
                engine.materials.clone(),
            )?;
            let mut graph = engine.technique.build(&view)?;
            let index = graph
                .resources
                .iter()
                .position(|(n, _)| n == "Linear HDR scene color")
                .context("composition lost linear scene color")?;
            // The capture API reads four bytes per output pixel. Expose all four float words.
            graph.display_extent = Some([4 * w, h]);
            graph.set_output(Resource(index));
            Ok(gpu.execute(graph.compile()?, &engine.tasks)?.pixels)
        };
        let start = capture(&engine, &mut gpu)?;
        let pixels: Vec<[f32; 4]> = start
            .chunks_exact(16)
            .map(bytemuck::pod_read_unaligned)
            .collect();
        assert!(
            pixels
                .iter()
                .all(|p| p.iter().all(|c| c.is_finite()) && p[3] == 1.)
        );
        assert!(
            pixels.iter().any(|p| p[..3].iter().any(|c| *c > 1.)),
            "fixture has no HDR highlight"
        );
        command(&mut engine, "water off")?;
        assert!(
            capture(&engine, &mut gpu)? == start,
            "unaffected radiance changed when disabling the water solver"
        );
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("layers.json");
        engine.world.save(&path)?;
        let camera = engine.camera;
        super::super::replace(&mut engine, World::load(&path)?)?;
        engine.camera = camera;
        assert!(!enabled(&engine.world, "water"));
        assert!(
            capture(&engine, &mut gpu)? == start,
            "reloaded linear radiance changed"
        );
        command(&mut engine, "water on")?;
        assert!(
            capture(&engine, &mut gpu)? == start,
            "radiance changed when re-enabling water"
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires Vulkan ray tracing"]
    fn off_camera_tree_casts_into_view_from_the_shared_wind_pose() -> Result<()> {
        use crate::{console::Console, render::SceneExtractor, rhi::Backend};
        use glam::Mat4;
        let mut forest = vegetation::scene()?;
        let tree = forest
            .entities
            .values()
            .find(|e| e.name == "Forest tree")
            .unwrap()
            .local;
        forest.entities.retain(|_, e| {
            e.instance.is_none()
                || (e.name != "Grass tuft" && (e.name != "Forest tree" || e.local == tree))
        });
        let parts: Vec<_> = forest
            .entities
            .iter()
            .filter(|(_, e)| e.name == "Forest tree")
            .map(|(&id, _)| id)
            .collect();
        for &id in &parts {
            forest.set_local(
                id,
                Mat4::from_scale_rotation_translation(
                    Vec3::splat(0.47),
                    glam::Quat::IDENTITY,
                    Vec3::new(-2.22, 0., 1.65),
                ),
            )?;
        }
        let mut engine = super::super::open(forest)?;
        engine.camera = Camera {
            target: Vec3::ZERO,
            distance: 1.2,
            yaw: 0.,
            pitch: 1.55,
        };
        let mut gpu = Backend::open()?;
        let mut extractor = SceneExtractor::default();
        let mut console = Console::default();
        let mut captures = vec![];
        console.execute(&mut engine, &[], "vegetation wind 2")?;
        for (time, hidden) in [(0., false), (1.7, false), (1.7, true)] {
            console.execute(&mut engine, &[], &format!("vegetation time {time}"))?;
            if hidden {
                for id in &parts {
                    Arc::make_mut(&mut engine.world).entities.remove(id);
                }
            }
            for mode in ["distance", "hardware", "software"] {
                console.execute(&mut engine, &[], &format!("rays {mode}"))?;
                let view = extractor.snapshot(
                    &engine.world,
                    engine.camera,
                    [144, 120],
                    engine.materials.clone(),
                )?;
                let graph = engine.technique.build(&view)?;
                captures.push(gpu.execute(graph.compile()?, &engine.tasks)?.pixels);
            }
        }
        let diff = |a: usize, b: usize| {
            captures[a]
                .chunks_exact(4)
                .zip(captures[b].chunks_exact(4))
                .filter(|(x, y)| x != y)
                .count()
        };
        assert_eq!(
            diff(0, 3) + diff(3, 6),
            0,
            "offscreen caster changed primary depth"
        );
        let wind = diff(1, 4);
        let shadow = diff(4, 7);
        eprintln!(
            "offscreen wind changed {wind} shadow pixels; removing the tree changed {shadow}"
        );
        assert!(wind > 5, "offscreen foliage stopped following the wind");
        assert!(shadow > 100, "tree cast no shadow on the visible receiver");
        assert!(
            diff(1, 2) < 144 && diff(4, 5) < 144 && diff(7, 8) < 144,
            "software and hardware disagree"
        );
        Ok(())
    }
}
