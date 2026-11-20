mod support;
use anyhow::Result;
use render_lab::{
    engine::{Edit, Engine, Schedule, System},
    rdg::*,
    render::{Camera, Forward, Snapshot, Technique},
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
    let dir = support::tempdir()?;
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
fn forward_graph_declares_all_viewport_passes() -> Result<()> {
    let snapshot = Snapshot {
        materials: Default::default(),
        world: Arc::new(World::demo()),
        camera: Camera::default(),
        extent: [160, 100],
    };
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
    console.execute(&mut e, &[], "pause")?;
    assert!(console.paused);
    console.execute(&mut e, &[], "rdg")?;
    assert!(!console.show_graph);
    let dir = support::tempdir()?;
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
        assert!(f.counters["PBR pixels"] > 0 && f.counters["Toon pixels"] > 0);
    }
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
    let mut s = Snapshot {
        materials: Default::default(),
        world: Arc::new(World::demo()),
        camera: Camera::default(),
        extent: [160, 101],
    };
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut gpu = render_lab::rhi::Backend::open()?;
    let mut draw = |s: &Snapshot| gpu.execute(Forward::default().build(s)?.compile()?, &pool);
    let first = draw(&s)?;
    let n = first.counters["PBR pixels"] + first.counters["Toon pixels"];
    assert!(first.counters["PBR pixels"] > 0 && first.counters["Toon pixels"] > 0);
    for m in Arc::make_mut(&mut s.world)
        .entities
        .values_mut()
        .filter_map(|e| e.material.as_mut())
    {
        m.shading = Shading::Toon;
    }
    let changed = draw(&s)?;
    assert_eq!(changed.counters["PBR pixels"], 0);
    assert_eq!(changed.counters["Toon pixels"], n);
    assert_ne!(first.pixels, changed.pixels);
    let ink = Shading::Custom("test.ink".into());
    for m in Arc::make_mut(&mut s.world)
        .entities
        .values_mut()
        .filter_map(|e| e.material.as_mut())
    {
        m.shading = ink.clone();
    }
    assert!(Forward::default().build(&s).is_err());
    // A plugin can reuse the Toon kernel under a distinct stable key.
    let key = Arc::make_mut(&mut s.materials).register(
        "test.ink",
        include_bytes!(concat!(env!("OUT_DIR"), "/toon.comp.spv")),
    )?;
    assert_eq!(key, ink);
    let plugin = draw(&s)?;
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
    let dir = support::tempdir()?;
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
