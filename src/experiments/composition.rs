//! Assembly of independently configured raster geometry, lighting and overlays.
use super::{
    cartoon, clusters, effects,
    light_stack::{self, LightStack},
    motion, restir, vegetation,
};
use crate::{
    engine::Engine,
    render::FrameGraph,
    render::{Camera, Forward, MaterialPrograms, Snapshot, Technique},
    scene::*,
};
use anyhow::{Context, Result, bail, ensure};
mod scene;
use glam::Vec3;
pub use scene::scene;
use std::sync::Arc;
const SETTING: &str = "lab.composition";
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

fn layer_enabled(settings: Option<&serde_json::Value>, layer: &str) -> bool {
    settings
        .and_then(|v| v.get(layer))
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}
pub(super) type Configured = (Arc<dyn Technique>, Camera);
#[derive(Default)]
pub(super) struct GeometryModules {
    pub cluster: bool,
    pub plants: bool,
    pub effects: bool,
    pub cartoon: bool,
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
        plants,
        effects,
        cartoon,
    } = geometry;
    let extended = cluster || plants || effects || cartoon;
    let lights = LightStack::open_modules(world, materials, tasks, extended)?;
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
    let cluster = (cluster || plants) && enabled(world, "forest");
    let effect = effects && enabled(world, "water");
    let technique: Arc<dyn Technique> = match (cluster, effect, cartoon, lights) {
        (false, false, false, Some(l)) => Arc::new(l),
        (true, false, false, None) => Arc::new(clusters::ClusterLod::default()),
        (false, true, false, None) => Arc::new(effects::Effects),
        (false, false, true, None) => Arc::new(cartoon::Cartoon::default()),
        (false, false, false, None) => Arc::new(Forward::default()),
        (cl, effect, ct, lights) => Arc::new(Modules {
            cluster: cl.then(clusters::ClusterLod::default),
            effect,
            cartoon: ct.then(cartoon::Cartoon::default),
            lights,
            plants: plants && cl,
        }),
    };
    Ok((technique, camera))
}
struct Modules {
    cluster: Option<clusters::ClusterLod>,
    effect: bool,
    cartoon: Option<cartoon::Cartoon>,
    lights: Option<LightStack>,
    plants: bool,
}
impl Technique for Modules {
    fn needs_surface_motion(&self, s: &Snapshot) -> bool {
        self.lights
            .as_ref()
            .is_some_and(|l| l.needs_surface_motion(s))
    }
    fn build(&self, s: &Snapshot) -> Result<FrameGraph> {
        let owns_effect =
            |i: &crate::render::RenderInstance| self.effect && effects::Effects::owns(s, i);
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
        let (effects, ordinary): (Vec<_>, Vec<_>) =
            candidates.into_iter().partition(|i| owns_effect(i));
        let ordinary = Forward::default().prepare_surfaces(s, ordinary)?;
        let layers = self
            .lights
            .as_ref()
            .map(|l| l.layers(s, &ordinary))
            .transpose()?
            .unwrap_or_default();
        let rivers = std::cell::RefCell::new(Vec::new());
        light_stack::build_modules(
            s,
            |g, t| {
                g.products.request_ray_proxies |=
                    self.lights.as_ref().is_some_and(|l| l.uses_shadow_rays());
                g.products.instance_visits += visits;
                let draw = |g: &mut FrameGraph, t: &mut crate::render::Targets| {
                    if let Some(cartoon) = &self.cartoon {
                        cartoon.draw(g, t, s, ordinary)?
                    } else {
                        light_stack::draw_surfaces(g, t, s, ordinary)?
                    }
                    if self.effect {
                        for i in effects {
                            if let Some(id) = effects::Effects::draw(g, t, s, i)? {
                                rivers.borrow_mut().push(id);
                            }
                        }
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
            false,
            |g, ids, depth, color| {
                effects::river::reflections(g, s, ids, depth, color, &rivers.borrow());
                if self.plants {
                    clusters::ClusterLod::atmosphere(g, s, ids, depth, color)?;
                }
                if self.cartoon.is_some() {
                    cartoon::Cartoon::overlay(g, s, ids, depth, color)?;
                }
                Ok(())
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
    let mut world = (*engine.world).clone();
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
    let lighting_only = light_stack::REGISTRY.iter().any(|r| r.feature == *layer);
    engine.world = Arc::new(world);
    prepared.apply_layers(engine, lighting_only);
    Ok(format!("{layer} {state}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::post::{Mode, Sequence, Settings};

    #[test]
    #[ignore = "requires confluence assets and a Vulkan device"]
    fn switches_contributors_independently_and_preserves_the_camera() -> Result<()> {
        let mut engine = super::super::open(scene()?)?;
        let camera = engine.camera;
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
            if !cmd.is_empty() {
                let light_change = cmd.starts_with("ibl ") || cmd.starts_with("restir ");
                if light_change {
                    engine
                        .schedule
                        .tick(&mut engine.world, 1. / 60., &engine.tasks)?;
                }
                let before = engine.schedule.describe();
                command(&mut engine, cmd)?;
                if light_change {
                    assert_eq!(before, engine.schedule.describe());
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
            assert_eq!(groups.len(), usize::from(!expected.is_empty()), "{cmd}");
            assert_eq!(
                groups.into_iter().flatten().collect::<Vec<_>>(),
                expected,
                "{cmd}"
            );
            if cmd == "actor off" {
                let temp = tempfile::tempdir()?;
                let path = temp.path().join("confluence.json");
                engine.world.save(&path)?;
                super::super::replace(&mut engine, World::load(&path)?)?;
            }
            let mut view = Snapshot::new(
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
            sequence.assign(settings, &engine.technique, &mut view);
            let graph = engine.technique.build(&view)?;
            let has = |name: &str| graph.passes.iter().any(|p| p.name.contains(name));
            let resources = |name: &str| graph.resources.iter().filter(|(n, _)| n == name).count();
            assert_eq!(resources("Visibility IDs"), 1, "{cmd}");
            assert_eq!(resources("Scene depth"), 1, "{cmd}");
            assert!(has("Temporal AA resolve"), "{cmd}");
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
