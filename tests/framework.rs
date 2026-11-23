use anyhow::Result;
use render_lab::{
    engine::{Edit, Engine, Schedule, System},
    rdg::*,
    render::{Camera, Forward, SceneExtractor, Snapshot, Technique},
    scene::*,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn compute(g: &mut Graph, name: &str, bindings: Vec<(Resource, Access)>) -> usize {
    g.pass(
        name,
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(&[]),
                bindings,
                push: vec![],
            },
            groups: [1; 3],
        },
    )
}
#[test]
fn dependencies_cover_fanout_and_overwrite() -> Result<()> {
    let mut g = Graph::new([32, 32]);
    let r = g.resource(
        "data",
        Storage::Buffer {
            bytes: 16,
            initial: None,
        },
    );
    compute(&mut g, "produce", vec![(r, Access::ComputeWrite)]);
    compute(&mut g, "a", vec![(r, Access::ComputeRead)]);
    compute(&mut g, "b", vec![(r, Access::ComputeRead)]);
    compute(&mut g, "overwrite", vec![(r, Access::ComputeWrite)]);
    let p = g.compile()?;
    assert_eq!(p.steps()[2].parents, vec![0]);
    assert!(p.steps()[2].barriers.is_empty());
    assert_eq!(p.steps()[3].parents, vec![1, 2]);
    Ok(())
}
#[test]
fn invalid_graphs_fail_before_recording() {
    let mut g = Graph::new([1, 1]);
    let r = g.resource(
        "empty",
        Storage::Buffer {
            bytes: 8,
            initial: None,
        },
    );
    compute(&mut g, "bad", vec![(r, Access::ComputeRead)]);
    assert!(g.compile().is_err());
    let mut g = Graph::new([1, 1]);
    let a = compute(&mut g, "a", vec![]);
    let b = compute(&mut g, "b", vec![]);
    g.passes[a].after.push(b);
    g.passes[b].after.push(a);
    assert!(g.compile().is_err());
}
#[test]
fn scene_round_trip_is_self_contained() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("scene.json");
    let mut w = World::demo();
    let id = w.spawn(Entity::default());
    w.entities
        .get_mut(&id)
        .unwrap()
        .components
        .insert("test.body".into(), serde_json::json!({"mass":2}));
    w.save(&path)?;
    let loaded = World::load(&path)?;
    assert_eq!(serde_json::to_value(&w)?, serde_json::to_value(&loaded)?);
    w.entities.get_mut(&id).unwrap().parent = Some(id);
    assert!(w.save(&path).is_err());
    World::load(&path)?;
    Ok(())
}

#[test]
fn light_instances_follow_scene_hierarchy_and_round_trip() -> Result<()> {
    use glam::{Mat4, Vec3};
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("lights.json");
    let mut world = World::demo();
    let parent = world.spawn(Entity {
        local: Mat4::from_translation(Vec3::new(1., 2., 3.)),
        ..Default::default()
    });
    let mesh = world
        .entities
        .values()
        .find_map(|e| e.instance.as_ref())
        .unwrap()
        .mesh;
    let id = world.spawn(Entity {
        parent: Some(parent),
        local: Mat4::from_translation(Vec3::X),
        instance: Some(Instance::new(mesh, None)),
        light: Some(Light::point([1., 0.5, 0.25], 4., 12.)),
        ..Default::default()
    });
    world.save(&path)?;
    let mut restored = World::load(&path)?;
    let mut extractor = SceneExtractor::default();
    let mut view =
        |world: &World| extractor.snapshot(world, Camera::default(), [40, 30], Default::default());
    let first = view(&restored)?;
    assert!(
        first
            .scene
            .instances()
            .iter()
            .any(|instance| instance.id == id)
    );
    let light = first.scene.lights()[0];
    assert_eq!(light.id, id);
    assert_eq!(light.position, Vec3::new(2., 2., 3.));
    assert_eq!(light.radiance, [4., 2., 1.]);
    restored.entities.get_mut(&parent).unwrap().local =
        Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2);
    assert!(
        view(&restored)?.scene.lights()[0]
            .position
            .abs_diff_eq(Vec3::NEG_Z, 1e-5)
    );
    let entity = restored.entities.get_mut(&id).unwrap();
    entity.light.as_mut().unwrap().enabled = false;
    assert!(view(&restored)?.scene.lights().is_empty());
    let graph = Forward::default().build(&view(&restored)?)?;
    assert!(
        graph
            .resources
            .iter()
            .any(|(name, _)| name == "Scene light instances")
    );
    restored
        .entities
        .get_mut(&id)
        .unwrap()
        .light
        .as_mut()
        .unwrap()
        .intensity = -1.;
    assert!(restored.validate().is_err());
    Ok(())
}

#[test]
#[ignore = "requires Vulkan 1.4 and VK_EXT_descriptor_heap"]
fn point_light_edits_change_material_shading() -> Result<()> {
    let mut engine = Engine::new(World::demo(), Arc::new(Forward::default()))?;
    let mut console = render_lab::console::Console::default();
    let position = engine.camera.eye();
    let created = console.execute(
        &mut engine,
        &[],
        &format!(
            "light point {} {} {} 180 30 1 0.18 0.04",
            position.x, position.y, position.z
        ),
    )?;
    let id = created.rsplit('#').next().unwrap();
    let mut device = render_lab::rhi::Backend::open()?;
    let mut draw = |engine: &Engine| {
        let view = Snapshot::new(&engine.world, engine.camera, [160, 100], Default::default())?;
        device.execute(engine.technique.build(&view)?.compile()?, &engine.tasks)
    };
    let lit = draw(&engine)?;
    assert!(
        console
            .execute(&mut engine, &[], &format!("light power {id} -1"))
            .is_err()
    );
    let unedited = draw(&engine)?;
    assert_eq!(lit.pixels, unedited.pixels);
    console.execute(&mut engine, &[], &format!("light enable {id} off"))?;
    let dark = draw(&engine)?;
    let changed = lit
        .pixels
        .chunks_exact(4)
        .zip(dark.pixels.chunks_exact(4))
        .filter(|(bright, dim)| bright[2] > dim[2].saturating_add(8))
        .count();
    assert!(changed > 100, "{changed} pixels changed");
    console.execute(&mut engine, &[], &format!("light enable {id} on"))?;
    console.execute(&mut engine, &[], &format!("light move {id} 999 999 999"))?;
    assert_eq!(dark.pixels, draw(&engine)?.pixels);
    Ok(())
}
struct Add;
impl System for Add {
    fn update(&self, w: &World, _: f32) -> Result<Edit> {
        let count = w.entities.len();
        Ok(Box::new(move |w| {
            w.spawn(Entity {
                name: count.to_string(),
                ..Default::default()
            });
        }))
    }
}
#[test]
fn systems_read_stage_snapshots_and_commit_in_order() -> Result<()> {
    let mut w = Arc::new(World::default());
    let mut sched = Schedule::default();
    sched.stage(vec![Arc::new(Add), Arc::new(Add)]);
    sched.stage(vec![Arc::new(Add)]);
    sched.tick(
        &mut w,
        0.1,
        &rayon::ThreadPoolBuilder::new().num_threads(2).build()?,
    )?;
    assert_eq!(
        w.entities
            .values()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        ["0", "0", "2"]
    );
    Ok(())
}

#[test]
fn fixed_stages_accumulate_and_cap_catch_up() -> Result<()> {
    let mut world = Arc::new(World::default());
    let mut schedule = Schedule::default();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    schedule.fixed_stage(Duration::from_millis(125), 2, vec![Arc::new(Add)])?;
    for (dt, expected) in [(0.0625, 0), (0.0625, 1), (0.0, 1), (1.0, 3), (0.0, 3)] {
        schedule.tick(&mut world, dt, &pool)?;
        assert_eq!(world.entities.len(), expected);
    }
    Ok(())
}

#[test]
fn history_keys_cannot_alias_within_a_frame() {
    let mut graph = Graph::new([32, 32]);
    graph.history_buffer("first", 17, 16);
    graph.history_buffer("second", 17, 16);
    assert!(graph.compile().is_err());
}
#[test]
fn forward_graph_declares_all_viewport_passes() -> Result<()> {
    let snapshot = Snapshot::new(
        &World::demo(),
        Camera::default(),
        [160, 100],
        Default::default(),
    )?;
    let p = Forward::default().build(&snapshot)?.compile()?;
    assert!(
        p.graph()
            .passes
            .iter()
            .any(|p| p.name == "Classify visible pixels")
    );
    assert!(
        p.graph()
            .passes
            .iter()
            .filter(|p| p.name.starts_with("Shade "))
            .count()
            == 2
    );
    Ok(())
}
#[test]
#[ignore = "requires Vulkan 1.4 and VK_EXT_descriptor_heap"]
fn threaded_gpu_frame_and_resize() -> Result<()> {
    let mut e = Engine::new(World::demo(), Arc::new(Forward::default()))?;
    let mut console = render_lab::console::Console::default();
    assert!(
        console
            .execute(&mut e, &[], "memory idle")?
            .contains("第一帧")
    );
    console.execute(&mut e, &[], "pause")?;
    assert!(console.paused);
    console.execute(&mut e, &[], "rdg")?;
    assert!(!console.show_graph);
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("console round trip.json");
    console.execute(&mut e, &[], &format!("save \"{}\"", path.display()))?;
    console.execute(&mut e, &[], &format!("load \"{}\"", path.display()))?;
    assert!(console.execute(&mut e, &[], "stats")?.contains("entities"));
    assert!(console.execute(&mut e, &[], "typo").is_err());
    for extent in [[160, 100], [180, 110], [180, 110]] {
        assert!(e.tick(0.016, extent)?);
        let start = Instant::now();
        let f = loop {
            if let Some(r) = e.take_frame() {
                break r.map_err(anyhow::Error::msg)?;
            }
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(f.pixels.len(), extent[0] as usize * extent[1] as usize * 4);
        assert!(
            f.pixels
                .chunks_exact(4)
                .filter(|p| *p != &f.pixels[..4])
                .count()
                > 100
        );
        assert!(f.passes.iter().all(|p| p.ms >= 0.));
        assert_eq!(
            e.last_memory().unwrap().submission,
            f.memory.managed.submission
        );
        for mode in ["idle", "alloc", "driver"] {
            let summary =
                console.execute(&mut e, &[], &format!("memory {mode} resource descriptor"))?;
            assert!(summary.contains("Resource descriptor heap") && summary.contains("最近扩容"));
        }
        assert!(f.counters["PBR pixels"] > 0 && f.counters["Toon pixels"] > 0);
    }
    Ok(())
}

#[test]
#[ignore = "requires Vulkan 1.4 and VK_EXT_descriptor_heap"]
fn mixed_lighting_combines_material_bins_and_keeps_live_scene_on_failed_load() -> Result<()> {
    let mut engine = render_lab::experiments::open(render_lab::experiments::mixed::scene()?)?;
    render_lab::console::Console::default().execute(&mut engine, &[], "ibl sky on")?;
    let world = engine.world.clone();
    let technique = engine.technique.clone();
    let camera = engine.camera.eye();
    let mut invalid = (*world).clone();
    let env = invalid
        .entities
        .values_mut()
        .find_map(|entity| {
            entity
                .components
                .get_mut(render_lab::experiments::ibl::SETTING)
        })
        .unwrap();
    *env = serde_json::json!("invalid environment settings");
    assert!(render_lab::experiments::replace(&mut engine, invalid).is_err());
    assert!(Arc::ptr_eq(&engine.world, &world));
    assert!(Arc::ptr_eq(&engine.technique, &technique));
    assert_eq!(engine.camera.eye(), camera);

    let mut extractor = SceneExtractor::default();
    let snapshot =
        extractor.snapshot(&world, engine.camera, [160, 100], engine.materials.clone())?;
    let mut gpu = render_lab::rhi::Backend::open()?;
    let frame = gpu.execute(technique.build(&snapshot)?.compile()?, &engine.tasks)?;
    use render_lab::rdg::PassPhase;
    assert!(frame.passes.iter().any(|p| p.phase == PassPhase::Denoise));
    assert!(frame.passes.iter().any(|p| p.phase == PassPhase::Lighting));
    assert!(frame.passes.iter().any(|p| p.phase == PassPhase::Shade));
    assert!(frame.counters["PBR pixels"] > 0 && frame.counters["Toon pixels"] > 0);
    let tonemap = frame
        .passes
        .iter()
        .position(|p| p.name == "Tone map linear scene color")
        .unwrap();
    assert_eq!(
        frame
            .passes
            .iter()
            .filter(|p| p.name.starts_with("Tone map"))
            .count(),
        1
    );
    assert!(
        frame.passes[..tonemap]
            .iter()
            .any(|p| p.name.contains("ReSTIR"))
    );
    let sky = frame
        .passes
        .iter()
        .position(|p| p.name == "Environment backdrop")
        .unwrap();
    assert!(frame.passes[tonemap].parents.contains(&sky));
    assert!(
        frame
            .pixels
            .chunks_exact(4)
            .any(|p| p != &frame.pixels[..4])
    );
    render_lab::console::Console::default().execute(&mut engine, &[], "ibl ev 0")?;
    let brighter = extractor.snapshot(
        &engine.world,
        engine.camera,
        snapshot.extent,
        engine.materials.clone(),
    )?;
    let brighter = gpu.execute(technique.build(&brighter)?.compile()?, &engine.tasks)?;
    let sky_row = |data: &[u8]| {
        data[..160 * 4]
            .chunks_exact(4)
            .flat_map(|pixel| &pixel[..3])
            .map(|&channel| u32::from(channel))
            .sum::<u32>()
    };
    assert!(sky_row(&brighter.pixels) > sky_row(&frame.pixels));
    // Remove each producer in turn while preserving the mixed scene's camera and materials.
    let mut variants = Vec::new();
    let saved = tempfile::tempdir()?;
    let path = saved.path().join("lighting.scene");
    for (remove, expected_direct) in [
        (render_lab::experiments::ibl::SETTING, true),
        (render_lab::experiments::restir::SETTING, false),
    ] {
        let mut candidate = (*world).clone();
        for entity in candidate.entities.values_mut() {
            entity.components.remove(remove);
        }
        candidate.save(&path)?;
        let restored = World::load(&path)?;
        assert!(restored.first_raw_component(remove).is_none());
        render_lab::experiments::replace(&mut engine, restored)?;
        let view = extractor.snapshot(
            &engine.world,
            snapshot.camera,
            snapshot.extent,
            engine.materials.clone(),
        )?;
        let image = gpu.execute(engine.technique.build(&view)?.compile()?, &engine.tasks)?;
        assert_eq!(
            image
                .passes
                .iter()
                .any(|pass| pass.phase == PassPhase::Denoise),
            expected_direct
        );
        assert!(image.counters["PBR pixels"] > 0 && image.counters["Toon pixels"] > 0);
        assert_ne!(image.pixels, frame.pixels);
        variants.push(image.pixels);
    }
    assert_ne!(variants[0], variants[1]);
    Ok(())
}

#[test]
fn mouse_look_preserves_eye_and_wasd_translates_camera() {
    let mut c = Camera::default();
    let eye = c.eye();
    let initial = c.target;
    c.look(0.2, -0.1);
    assert!(c.eye().distance(eye) < 1e-5);
    assert_ne!(c.target, initial);
    let direction = (c.target - c.eye()).normalize();
    c.translate(0., 1., 0.1);
    assert!((c.eye() - eye).normalize().dot(direction) > 0.9999);
    let eye = c.eye();
    c.translate(1., 0., 0.1);
    assert!((c.eye() - eye).dot(direction).abs() < 1e-5);
}
#[test]
#[ignore = "requires Vulkan 1.4"]
fn material_selects_pixel_shader_for_every_bound_instance() -> Result<()> {
    let mut world = World::demo();
    let mut extract = SceneExtractor::default();
    let mut view = |world: &World, materials| {
        extract.snapshot(world, Camera::default(), [160, 101], materials)
    };
    let mut materials = Arc::<render_lab::render::MaterialPrograms>::default();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut gpu = render_lab::rhi::Backend::open()?;
    let mut draw = |s: &Snapshot| gpu.execute(Forward::default().build(s)?.compile()?, &pool);
    let first = draw(&view(&world, materials.clone())?)?;
    let n = first.counters["PBR pixels"] + first.counters["Toon pixels"];
    assert!(first.counters["PBR pixels"] > 0 && first.counters["Toon pixels"] > 0);
    for m in world
        .entities
        .values_mut()
        .filter_map(|e| e.material.as_mut())
    {
        m.shading = Shading::Toon;
    }
    let changed = draw(&view(&world, materials.clone())?)?;
    assert_eq!(changed.counters["PBR pixels"], 0);
    assert_eq!(changed.counters["Toon pixels"], n);
    assert_ne!(first.pixels, changed.pixels);
    let ink = Shading::Custom("test.ink".into());
    for m in world
        .entities
        .values_mut()
        .filter_map(|e| e.material.as_mut())
    {
        m.shading = ink.clone();
    }
    assert!(
        Forward::default()
            .build(&view(&world, materials.clone())?)
            .is_err()
    );
    // A plugin can reuse the Toon kernel under a distinct stable key.
    let key = Arc::make_mut(&mut materials).register(
        "test.ink",
        include_bytes!(concat!(env!("OUT_DIR"), "/toon.comp.spv")),
    )?;
    assert_eq!(key, ink);
    let plugin = draw(&view(&world, materials)?)?;
    assert_eq!(plugin.counters["test.ink"], n);
    assert_eq!(plugin.counters["Toon pixels"], 0);
    assert_eq!(plugin.pixels, changed.pixels);
    Ok(())
}

#[test]
fn old_instance_shading_migrates_to_shared_material_variants() -> Result<()> {
    let mut json = serde_json::to_value(World::demo())?;
    let entities = json["entities"].as_object_mut().unwrap();
    let shared = entities
        .iter()
        .find(|(_, e)| !e["material"].is_null())
        .unwrap()
        .0
        .parse::<u64>()?;
    let mut count = 0;
    for e in entities.values_mut() {
        if let Some(m) = e["material"].as_object_mut() {
            m.remove("shading");
        }
        if !e["instance"].is_null() {
            e["instance"]["material"] = shared.into();
            e["instance"]["shading"] = if count == 0 {
                "Pbr".into()
            } else {
                "Toon".into()
            };
            count += 1;
        }
    }
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("legacy.json");
    std::fs::write(&path, serde_json::to_vec(&json)?)?;
    let w = World::load(&path)?;
    let mats = w
        .entities
        .values()
        .filter_map(|e| e.instance.as_ref())
        .map(|i| {
            w.entities[&i.material.unwrap()]
                .material
                .as_ref()
                .unwrap()
                .shading
                .clone()
        })
        .collect::<Vec<_>>();
    assert!(mats.contains(&Shading::Pbr) && mats.contains(&Shading::Toon));
    w.save(&path)?;
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    assert!(
        saved["entities"]
            .as_object()
            .unwrap()
            .values()
            .filter(|e| e["instance"].is_object())
            .all(|e| e["instance"].get("shading").is_none())
    );
    Ok(())
}
