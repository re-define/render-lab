use anyhow::Result;
use glam::Mat4;
use render_lab::{
    engine::{Schedule, System},
    experiments::{clusters, motion},
    render::{
        Camera, Forward, GeometryPath, GeometrySelection, MeshId, SceneExtractor, Snapshot,
        Technique,
    },
    rhi::Backend,
    scene::*,
};
use std::sync::Arc;

#[test]
fn cluster_cut_refines_and_round_trips() -> Result<()> {
    let mut w = clusters::terrain()?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("terrain.json");
    w.save(&path)?;
    w = World::load(&path)?;
    let camera = Camera::fit(&w)?;
    let cut = |mut w: World, px: f32| -> Result<Vec<u32>> {
        *w.entities
            .values_mut()
            .find_map(|e| e.components.get_mut(clusters::SETTING))
            .unwrap() = px.into();
        let view = Snapshot::new(&w, camera, [480, 300], Default::default())?;
        let geom = view
            .scene
            .meshes()
            .values()
            .find(|e| e.hierarchy.is_some())
            .unwrap();
        clusters::ClusterLod::default().triangles(geom, Mat4::IDENTITY, &view)
    };
    let mut fine = cut(w.clone(), 0.)?;
    fine.sort_unstable();
    assert_eq!(fine, (0..8192).collect::<Vec<_>>());
    let coarse = cut(w, 1000.)?;
    assert!(!coarse.is_empty() && coarse.len() < fine.len() / 2);
    Ok(())
}
fn player(w: &World) -> Result<motion::Player> {
    Ok(serde_json::from_value(
        w.entities
            .values()
            .find_map(|e| e.components.get(motion::PLAYER))
            .unwrap()
            .clone(),
    )?)
}
#[test]
fn motion_queries_change_clips_and_drive_skin() -> Result<()> {
    let mut w = motion::scene();
    w.validate()?;
    let framed = Camera::fit(&w)?;
    let mut displaced = w.clone();
    let actor = displaced
        .entities
        .values_mut()
        .find(|e| e.skin_binding.is_some())
        .unwrap();
    actor.local = Mat4::from_translation(glam::Vec3::X * 100.);
    assert_eq!(
        Camera::fit(&displaced)?.target,
        framed.target,
        "frame the skinned pose in joint space"
    );
    let m = motion::Matcher::default();
    for (speed, turn, expected) in [(0., 0., "Idle"), (3., 0., "Run"), (1.35, -1., "Turn left")] {
        motion::set_goal(&mut w, speed, turn)?;
        for _ in 0..10 {
            m.update(&w, 1. / 30.)?(&mut w);
        }
        let p = player(&w)?;
        assert_eq!(p.clip, expected);
        assert!(p.cost.is_finite());
    }
    let initial = player(&w)?;
    let before = w.entities[&initial.bones[1]].local;
    m.update(&w, 0.)?(&mut w);
    assert_eq!(before, w.entities[&initial.bones[1]].local);
    m.update(&w, 1. / 30.)?(&mut w);
    assert_ne!(before, w.entities[&initial.bones[1]].local);
    Ok(())
}

#[test]
fn extraction_updates_only_the_affected_hierarchy_and_shares_rig_palettes() -> Result<()> {
    let mut world = World::demo();
    let original = world
        .entities
        .values()
        .find_map(|e| e.instance.clone())
        .unwrap();
    let mut weighted = world.entities[&original.mesh]
        .mesh
        .as_deref()
        .unwrap()
        .clone();
    weighted.joints = vec![[0; 4]; weighted.positions.len()];
    weighted.weights = vec![[1., 0., 0., 0.]; weighted.positions.len()];
    let topology = Arc::new(weighted.edges());
    let mesh = world.spawn(Entity {
        mesh: Some(Arc::new(weighted)),
        meshlets: Some(Arc::new(vec![Meshlet {
            triangles: vec![0],
            refined_group: None,
        }])),
        hierarchy: Some(Arc::new(vec![ClusterNode {
            meshlets: vec![0],
            center: [0.; 3],
            radius: 1.,
            error: 0.,
        }])),
        topology: Some(topology),
        ..Default::default()
    });
    let joint = world.spawn(Entity::default());
    let child = world.spawn(Entity {
        parent: Some(joint),
        ..Default::default()
    });
    let skin = world.spawn(Entity {
        skin: Some(Skin {
            joints: vec![child],
            inverse_bind: vec![Mat4::IDENTITY],
        }),
        ..Default::default()
    });
    let mut outline = Instance::new(mesh, original.material);
    outline.outline = true;
    let outlined = world.spawn(Entity {
        instance: Some(outline),
        ..Default::default()
    });
    let rigid = Instance::new(mesh, original.material);
    let first_rigid = world.spawn(Entity {
        instance: Some(rigid.clone()),
        ..Default::default()
    });
    for _ in 1..2048 {
        world.spawn(Entity {
            instance: Some(rigid.clone()),
            ..Default::default()
        });
    }
    let binding = Entity {
        instance: Some(rigid),
        skin_binding: Some(skin),
        ..Default::default()
    };
    let first_actor = world.spawn(binding.clone());
    let second_actor = world.spawn(binding);
    world.validate()?;
    let mut world = Arc::new(world);
    let mut extractor = SceneExtractor::default();
    let mut extract =
        |world: &World| extractor.snapshot(world, Camera::default(), [128, 80], Default::default());
    let before = extract(&world)?;
    let get = |scene: &render_lab::render::RenderScene, id| {
        scene
            .instances()
            .iter()
            .find(|i| i.id == id)
            .unwrap()
            .clone()
    };
    let asset = &before.scene.meshes()[&MeshId(mesh)];
    assert_eq!(
        get(&before.scene, first_rigid).geometry_path(asset, true),
        GeometryPath::ClusterLod
    );
    assert_eq!(
        get(&before.scene, outlined).geometry_path(asset, true),
        GeometryPath::Ordinary
    );
    assert_eq!(
        get(&before.scene, first_actor).geometry_path(asset, true),
        GeometryPath::Ordinary
    );
    assert_eq!(Arc::strong_count(&world), 1);

    Arc::make_mut(&mut world)
        .entities
        .get_mut(&joint)
        .unwrap()
        .local = Mat4::from_translation(glam::Vec3::X);
    let after = extract(&world)?;
    assert_eq!(after.scene.stats.transforms_updated, 2);
    assert_eq!(after.scene.stats.palettes_updated, 1);
    assert_eq!(after.scene.stats.instances_updated, 2);
    assert!(Arc::ptr_eq(asset, &after.scene.meshes()[&MeshId(mesh)]));
    for (old, new) in before.scene.instances().iter().zip(after.scene.instances()) {
        assert_eq!(
            Arc::ptr_eq(old, new),
            old.id != first_actor && old.id != second_actor
        );
    }
    let first = get(&after.scene, first_actor);
    let second = get(&after.scene, second_actor);
    assert!(Arc::ptr_eq(
        first.palette.as_ref().unwrap(),
        second.palette.as_ref().unwrap()
    ));
    assert_eq!(
        first.palette.as_ref().unwrap()[0],
        Mat4::from_translation(glam::Vec3::X)
    );
    assert_eq!(
        get(&before.scene, first_actor).palette.as_ref().unwrap()[0],
        Mat4::IDENTITY
    );

    Arc::make_mut(&mut world)
        .entities
        .get_mut(&joint)
        .unwrap()
        .parent = Some(child);
    assert!(extract(&world).is_err());
    Arc::make_mut(&mut world)
        .entities
        .get_mut(&joint)
        .unwrap()
        .parent = None;
    Arc::make_mut(&mut world)
        .entities
        .get_mut(&child)
        .unwrap()
        .parent = None;
    let detached = extract(&world)?;
    assert_eq!(detached.scene.stats.transforms_updated, 1);
    assert_eq!(
        get(&detached.scene, first_actor).palette.as_ref().unwrap()[0],
        Mat4::IDENTITY
    );
    let unchanged = extract(&world)?;
    assert_eq!(unchanged.scene.stats.transforms_updated, 0);
    assert_eq!(unchanged.scene.stats.instances_updated, 0);
    Ok(())
}
#[test]
#[ignore = "requires Vulkan 1.4 and VK_EXT_descriptor_heap"]
fn experiments_share_gpu_backend_and_match_reference() -> Result<()> {
    let mut gpu = Backend::open()?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let w = Arc::new(clusters::terrain()?);
    let mut camera = Camera::fit(&w)?;
    camera.pitch = 0.5;
    let mut extraction = SceneExtractor::default();
    let view = extraction.snapshot(&w, camera, [256, 160], Default::default())?;
    let reference = Forward {
        selection: Arc::new(clusters::ClusterLod::default()),
    };
    let draw =
        |g: &mut Backend, t: &dyn Technique, v: &Snapshot| g.execute(t.build(v)?.compile()?, &pool);
    let cpu = draw(&mut gpu, &reference, &view)?;
    let compact = draw(&mut gpu, &clusters::ClusterLod::default(), &view)?;
    let mismatches = cpu
        .pixels
        .chunks_exact(4)
        .zip(compact.pixels.chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        mismatches < 30,
        "indirect and direct images differ at {mismatches} pixels"
    );
    assert!(compact.passes.iter().any(|p| {
        p.parents
            .iter()
            .any(|&row| compact.passes[row].name.starts_with("Main Hi-Z"))
    }));
    let mut world = Arc::new(motion::scene());
    let camera = Camera::fit(&world)?;
    let view = extraction.snapshot(&world, camera, view.extent, view.materials.clone())?;
    let bind = draw(&mut gpu, &Forward::default(), &view)?;
    let mut schedule = Schedule::default();
    schedule.stage(vec![Arc::new(motion::Matcher::default())]);
    for _ in 0..10 {
        schedule.tick(&mut world, 1. / 30., &pool)?;
    }
    let posed_view = extraction.snapshot(&world, camera, view.extent, view.materials.clone())?;
    let posed = draw(&mut gpu, &Forward::default(), &posed_view)?;
    assert_ne!(bind.pixels, posed.pixels);
    Ok(())
}

#[test]
#[ignore = "requires Vulkan mesh shader"]
fn different_assets_share_one_frontier_and_cluster_list() -> Result<()> {
    let mut world = clusters::terrain()?;
    let marker = world.spawn(Entity {
        mesh: Some(Arc::new(Mesh {
            positions: vec![[-0.2, 0., -0.2], [0.2, 0., -0.2], [0., 0., 0.2]],
            indices: vec![0, 2, 1],
            normals: vec![],
            uv: vec![],
            joints: vec![],
            weights: vec![],
        })),
        ..Default::default()
    });
    let paint = world.spawn(Entity {
        material: Some(Material {
            shading: Shading::Toon,
            ..Default::default()
        }),
        ..Default::default()
    });
    for i in 0..70 {
        world.spawn(Entity {
            local: Mat4::from_translation(glam::Vec3::new(
                (i % 10) as f32 * 0.6 - 2.7,
                1.3,
                (i / 10) as f32 * 0.6 - 1.8,
            )),
            instance: Some(Instance::new(marker, Some(paint))),
            ..Default::default()
        });
    }
    clusters::prepare(&mut world)?;
    let view = Snapshot::new(
        &world,
        Camera {
            pitch: 1.1,
            distance: 13.,
            ..Default::default()
        },
        [320, 240],
        Default::default(),
    )?;
    let technique = clusters::ClusterLod::default();
    let graph = technique.build(&view)?;
    for name in ["Global BVH node list", "Global cluster candidates"] {
        assert_eq!(graph.resources.iter().filter(|(n, _)| n == name).count(), 1);
    }
    for name in [
        "GPU instance frustum",
        "GPU BVH level 0",
        "GPU cluster cull",
    ] {
        assert_eq!(graph.passes.iter().filter(|p| p.name == name).count(), 1);
    }
    let mut gpu = Backend::open()?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build()?;
    let frame = gpu.execute(graph.compile()?, &pool)?;
    assert_eq!(frame.counters["Frustum instances"], 71);
    assert!(frame.counters["Peak BVH frontier"] >= 71);
    assert!(frame.counters["PBR pixels"] > 0 && frame.counters["Toon pixels"] > 0);
    let reference = Forward {
        selection: Arc::new(technique),
    };
    let cpu = gpu.execute(reference.build(&view)?.compile()?, &pool)?;
    let mismatches = cpu
        .pixels
        .chunks_exact(4)
        .zip(frame.pixels.chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        mismatches < 30,
        "mixed-asset visibility mismatch at {mismatches} pixels"
    );
    Ok(())
}

#[test]
#[ignore = "requires Vulkan mesh shader"]
fn occlusion_history_recovers_after_camera_move() -> Result<()> {
    use glam::Vec3;
    let mut world = clusters::terrain()?;
    let wall = world.spawn(Entity {
        mesh: Some(Arc::new(Mesh {
            positions: vec![
                [-10., 3., -10.],
                [10., 3., -10.],
                [10., 3., 10.],
                [-10., 3., 10.],
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            normals: vec![],
            uv: vec![],
            joints: vec![],
            weights: vec![],
        })),
        ..Default::default()
    });
    let paint = world.spawn(Entity {
        material: Some(Material {
            shading: Shading::Toon,
            base_color: [1., 0.25, 0.05, 1.],
            ..Default::default()
        }),
        ..Default::default()
    });
    world.spawn(Entity {
        instance: Some(Instance::new(wall, Some(paint))),
        ..Default::default()
    });
    let settings = world
        .entities
        .values_mut()
        .find(|e| e.components.contains_key(clusters::SETTING))
        .unwrap();
    settings
        .components
        .insert(clusters::SETTING.into(), 0.into());
    let mut extractor = SceneExtractor::default();
    let mut s = extractor.snapshot(
        &world,
        Camera {
            distance: 12.,
            pitch: 1.52,
            yaw: 0.,
            target: Vec3::ZERO,
        },
        [257, 159],
        Default::default(),
    )?;
    let mut gpu = Backend::open()?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build()?;
    let clusters = clusters::ClusterLod::default();
    let mut draw =
        |tech: &dyn Technique, s: &Snapshot| gpu.execute(tech.build(s)?.compile()?, &pool);
    let cold = draw(&clusters, &s)?;
    assert_eq!(cold.counters["Deferred clusters"], 0);
    let covered = draw(&clusters, &s)?;
    assert!(covered.counters["Deferred clusters"] > 0);
    assert_eq!(covered.counters["Recovered clusters"], 0);
    assert_eq!(covered.counters["PBR pixels"], 0);
    assert!(covered.counters["Toon pixels"] > 0);
    s.camera.pitch = -1.48;
    let restored = draw(&clusters, &s)?;
    assert!(restored.counters["Recovered clusters"] > 0);
    assert!(restored.counters["L0"] > 0);
    let reference = draw(&Forward::default(), &s)?;
    let different = reference
        .pixels
        .chunks_exact(4)
        .zip(restored.pixels.chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        different < 30,
        "visibility mismatch at {different} pixels after disocclusion"
    );
    let e = world
        .entities
        .values_mut()
        .find(|e| e.components.contains_key(clusters::SETTING))
        .unwrap();
    e.components.insert(clusters::VISUALIZE.into(), true.into());
    s = extractor.snapshot(&world, s.camera, s.extent, s.materials.clone())?;
    let debug = draw(&clusters, &s)?;
    assert_ne!(restored.pixels, debug.pixels);
    assert_eq!(
        restored.counters["Toon pixels"],
        debug.counters["Toon pixels"]
    );
    for (normal, visual) in restored
        .pixels
        .chunks_exact(4)
        .zip(debug.pixels.chunks_exact(4))
    {
        if normal[2] > normal[1] && normal[1] > normal[0] {
            assert_eq!(normal, visual, "ordinary material was recolored");
        }
    }
    s.extent = [251, 157];
    assert_eq!(draw(&clusters, &s)?.counters["Deferred clusters"], 0);
    s.camera.target += Vec3::X * 1000.;
    let empty = draw(&clusters, &s)?;
    assert_eq!(empty.counters["Frustum instances"], 0);
    assert_eq!(empty.counters["BVH visits"], 0);
    assert_eq!(empty.counters["Cluster tests"], 0);
    Ok(())
}

#[test]
#[ignore = "requires local dragon glTF and Vulkan mesh shader"]
fn dragon_farm_shares_geometry_and_displays_different_lod_cuts() -> Result<()> {
    let mut world = clusters::scene()?;
    let geometries = world
        .entities
        .values()
        .filter(|e| e.hierarchy.is_some())
        .count();
    assert_eq!(geometries, 1);
    let instances = world
        .entities
        .values()
        .filter_map(|e| e.instance.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(
        instances
            .iter()
            .filter(|i| world.entities[&i.mesh].meshlets.is_some())
            .count(),
        36
    );
    assert_eq!(
        instances
            .iter()
            .filter(|i| world.entities[&i.mesh].meshlets.is_none())
            .count(),
        1
    );
    let camera = Camera {
        distance: 195.,
        pitch: 0.48,
        target: glam::Vec3::Y * 8.,
        ..Default::default()
    };
    let mut extractor = SceneExtractor::default();
    let materials = Arc::<render_lab::render::MaterialPrograms>::default();
    let technique = clusters::ClusterLod::default();
    let mut gpu = Backend::open()?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build()?;
    let mut draw = |error: f32| {
        let e = world
            .entities
            .values_mut()
            .find(|e| e.components.contains_key(clusters::SETTING))
            .unwrap();
        e.components.insert(clusters::SETTING.into(), error.into());
        e.components.insert(clusters::VISUALIZE.into(), true.into());
        let s = extractor.snapshot(&world, camera, [403, 259], materials.clone())?;
        gpu.execute(technique.build(&s)?.compile()?, &pool)
    };
    let fine = draw(0.2)?;
    let coarse = draw(1.5)?;
    for frame in [&fine, &coarse] {
        assert!(frame.counters["Cluster tests"] < frame.counters["Potential clusters"]);
        assert!(frame.counters["Toon pixels"] > 0 && frame.counters["PBR pixels"] > 0);
    }
    let tris = |frame: &render_lab::rhi::Frame| {
        frame
            .counters
            .iter()
            .filter(|(k, _)| k.starts_with('L'))
            .map(|(_, n)| n)
            .sum::<u64>()
    };
    assert!(tris(&fine) > tris(&coarse));
    assert_ne!(fine.pixels, coarse.pixels);
    Ok(())
}
