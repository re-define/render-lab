//! Reproducible extraction workload. Shared asset entities, independent bodies, skins and pose state.
use super::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

fn population(n: usize, pool: &rayon::ThreadPool) -> Result<(Arc<World>, Schedule)> {
    let w = population::populated_world(n, pool, false)?;
    let mut schedule = Schedule::default();
    install(&w, &mut schedule)?;
    Ok((Arc::new(w), schedule))
}

#[test]
fn movement_intent_selects_one_character_and_releases_the_previous_owner() -> Result<()> {
    use crate::movement::{MoveIntent, MovementControls};
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
    let mut w = duo_scene()?;
    let mut schedule = Schedule::default();
    install(&w, &mut schedule)?;
    let mut running = Arc::new(w);
    schedule.tick(&mut running, 1. / 30., &pool)?;
    w = Arc::try_unwrap(running).ok().unwrap();
    let ids: [Id; 2] = w.entities.component_owners(PLAYER).try_into().unwrap();
    let mut controls = MovementControls::new(Some(super::super::MOVEMENT));
    assert_eq!(controls.target(&w), Some(ids[0]));
    controls.apply(
        &mut w,
        ids[0],
        MoveIntent {
            side: 0,
            forward: 1,
        },
    )?;
    controls.select(&mut w, ids[1])?;
    controls.apply(
        &mut w,
        ids[1],
        MoveIntent {
            side: 1,
            forward: 0,
        },
    )?;
    assert_eq!(controls.target(&w), Some(ids[1]));
    assert_eq!(player(&w, ids[0])?.target_velocity(), Vec2::ZERO);
    assert_eq!(player(&w, ids[1])?.target_velocity(), Vec2::new(1.2, 0.));
    assert!(controls.select(&mut w, Id::MAX).is_err());
    w.entities.remove(&ids[1]);
    assert!(controls.target(&w).is_none());
    assert!(
        controls
            .apply(&mut w, ids[1], MoveIntent::default())
            .is_err()
    );
    Ok(())
}

#[test]
fn shared_platform_reduces_occupants_once_and_keeps_skeletons_independent() -> Result<()> {
    let one_worker = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
    let four_workers = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let (world, _) = population(2, &one_worker)?;
    let mut w = Arc::try_unwrap(world).ok().unwrap();
    let ids: [Id; 2] = w.entities.component_owners(SETTING).try_into().unwrap();
    let mut fixture = w
        .typed_component::<Contact>(ids[0], SETTING)?
        .unwrap()
        .into_owned();
    let (spacer, marker) = population::alternate_rig(&mut w, ids[1])?;
    let fingertip = marker.joint;
    // Both occupants stand on one spring in the same simulation coordinate system.
    w.entities.get_mut(&ids[1]).unwrap().parent = w.entities[&ids[0]].parent;
    fixture.course = None;
    fixture.auto_walk = false;
    fixture.play = None;
    fixture.source.clear();
    fixture.feet = Default::default();
    let plate = fixture.plate;
    let top = w.entities[&plate].local.transform_point3(0.5 * Vec3::Y);
    for (index, &id) in ids.iter().enumerate() {
        let at = top + Vec3::X * (index as f32 - 0.5) * 0.08;
        w.entities.get_mut(&id).unwrap().local = Mat4::from_translation(at);
        let mut p = player(&w, id)?.into_owned();
        (p.move_root, p.speed, p.side, p.turn) = (false, 0., 0., 0.);
        w.set_typed_component(id, PLAYER, p);
        let mut c = fixture.clone();
        if index == 1 {
            c.finger = Some(FingerMarker {
                joint: fingertip,
                offset: FINGERTIP,
            });
        }
        w.set_typed_component(id, SETTING, c);
        let mut b = dynamics::Body::new(at);
        b.support = Some(plate);
        w.set_typed_component(id, dynamics::BODY, b);
    }
    let mut single = w.clone();
    single
        .entities
        .get_mut(&ids[1])
        .unwrap()
        .components
        .remove(SETTING);
    // This comparison keeps the other actor animated after detaching its solver.
    single.set_motion_policy(ids[1], MotionPolicy::KINEMATIC)?;
    let mut single_schedule = Schedule::default();
    install(&single, &mut single_schedule)?;
    let mut single = Arc::new(single);
    single_schedule.tick(&mut single, 1. / 60., &one_worker)?;

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("shared-plate.scene.json");
    w.save(&path)?;
    let mut loaded = World::load(&path)?;
    crate::experiments::SceneConfiguration::restore(&mut loaded)?;
    let mut replay = Schedule::default();
    install(&loaded, &mut replay)?;
    let mut loaded = Arc::new(loaded);
    let mut schedule = Schedule::default();
    install(&w, &mut schedule)?;
    let joints = {
        let children = rig::children(&w);
        let a = Rig::bind(&w, ids[0], &children)?;
        let b = Rig::bind(&w, ids[1], &children)?;
        assert!(a.ids.iter().all(|id| !b.ids.contains(id)));
        assert_eq!(b.ids[b.finger], fingertip);
        assert!(b.ids.contains(&spacer) && b.ids.len() == a.ids.len() + 1);
        [a.ids, b.ids].concat()
    };
    let mut w = Arc::new(w);
    for step in 0..10 {
        schedule.tick(&mut w, 1. / 60., &four_workers)?;
        replay.tick(&mut loaded, 1. / 60., &one_worker)?;
        assert_eq!(w.entities[&plate].local, loaded.entities[&plate].local);
        for &id in &joints {
            assert!(
                w.entities[&id]
                    .local
                    .abs_diff_eq(loaded.entities[&id].local, 2e-5)
            );
        }
        if step == 0 {
            let velocity = |world: &World| {
                world
                    .typed_component::<dynamics::Spring>(plate, dynamics::SPRING)
                    .unwrap()
                    .unwrap()
                    .velocity
            };
            let (one, two) = (velocity(&single), velocity(&w));
            assert!(
                one < 0. && (two - 2. * one).abs() < 1e-5,
                "spring velocities {one} {two}"
            );
            assert!(w.entities[&plate].local.w_axis.y < single.entities[&plate].local.w_axis.y);
        }
    }
    // Toggle membership without reinstalling the schedule; standalone locomotion remains active.
    let id = ids[1];
    let mut contact = w
        .typed_component::<Contact>(id, SETTING)?
        .unwrap()
        .into_owned();
    Arc::make_mut(&mut w)
        .entities
        .get_mut(&id)
        .unwrap()
        .components
        .remove(SETTING);
    Arc::make_mut(&mut w).set_motion_policy(id, MotionPolicy::KINEMATIC)?;
    let prior = w
        .typed_component::<dynamics::Body>(id, dynamics::BODY)?
        .unwrap()
        .particle
        .position;
    schedule.tick(&mut w, 1. / 60., &four_workers)?;
    assert_eq!(
        w.typed_component::<dynamics::Body>(id, dynamics::BODY)?
            .unwrap()
            .particle
            .position,
        prior
    );
    contact.source.clear();
    Arc::make_mut(&mut w).set_motion_policy(id, MotionPolicy::DYNAMIC)?;
    Arc::make_mut(&mut w).set_typed_component(id, SETTING, contact);
    schedule.tick(&mut w, 1. / 60., &four_workers)?;
    assert_eq!(
        w.typed_component::<Contact>(id, SETTING)?
            .unwrap()
            .source
            .len(),
        21
    );
    Ok(())
}

#[test]
#[ignore = "release CPU benchmark; uses Silver Wolf assets"]
fn silver_crowd_extraction_profile() -> Result<()> {
    use crate::render::SceneExtractor;
    let count = std::env::var("RENDER_LAB_NPCS").map_or(Ok(300), |n| n.parse::<usize>())?;
    ensure!(count > 0, "positive population required");
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(std::thread::available_parallelism()?.get().min(16))
        .build()?;
    let (mut world, mut schedule) = population(count, &pool)?;
    let mut extractor = SceneExtractor::default();
    let mut times = Vec::new();
    let mut sim_times = Vec::new();
    let mut pose_counts = [0usize; 3];
    schedule.set_profiling(std::env::var_os("RENDER_LAB_SIM_PROFILE").is_some());
    let mut stages: [Vec<f64>; 5] = Default::default();
    for frame in 0..116 {
        let tick = Instant::now();
        schedule.tick(&mut world, 0.1, &pool)?;
        let sim_ms = tick.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        let view =
            extractor.snapshot(&world, Camera::default(), [1920, 1080], Default::default())?;
        let ms = start.elapsed().as_secs_f64() * 1000.;
        if frame >= 20 {
            assert!(!view.scene.stats.full_rebuild);
            if frame == 20 || frame == 115 {
                let cold = SceneExtractor::default().snapshot(
                    &world,
                    Camera::default(),
                    [1920, 1080],
                    Default::default(),
                )?;
                assert_eq!(view.scene.instances().len(), cold.scene.instances().len());
                for (a, b) in view.scene.instances().iter().zip(cold.scene.instances()) {
                    assert_eq!(
                        (a.id, a.model, a.palette_root, &a.palette),
                        (b.id, b.model, b.palette_root, &b.palette)
                    );
                }
            }
            let stats = view.scene.stats;
            for (sum, n) in pose_counts.iter_mut().zip([
                stats.pose_blocks_read,
                stats.pose_locals_read,
                stats.palette_joints_examined,
            ]) {
                *sum += n;
            }
            if let Some(p) = schedule.tick_profile() {
                for (series, d) in stages.iter_mut().zip([
                    p.binding,
                    p.evaluate,
                    p.commit,
                    p.capture_undo,
                    p.rollback,
                ]) {
                    series.push(d.as_secs_f64() * 1000.);
                }
            }
            times.push(ms);
            sim_times.push(sim_ms);
        }
    }
    for (series, name) in
        stages
            .iter_mut()
            .zip(["binding", "workers", "commit", "capture undo", "rollback"])
    {
        if !series.is_empty() {
            series.sort_by(f64::total_cmp);
            println!("{name}: P50/P95 {:.2}/{:.2} ms", series[48], series[90]);
        }
    }
    times.sort_by(f64::total_cmp);
    sim_times.sort_by(f64::total_cmp);
    let [blocks, locals, examined] = pose_counts.map(|n| n as f64 / 96.);
    println!(
        "per sampled frame: {blocks:.1} pose blocks, {locals:.1} dirty joint locals, {examined:.1} palette joints examined"
    );
    println!(
        "NPCs {count}; 96 frames, 6 fixed substeps; simulation P50/P95 {:.2}/{:.2} ms, extraction P50/P95 {:.2}/{:.2} ms",
        sim_times[48], sim_times[90], times[48], times[90]
    );
    Ok(())
}

#[test]
fn visual_budget_preserves_ik_focus_and_catches_up_without_new_edits() -> Result<()> {
    use crate::render::SceneExtractor;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut world = population::populated_world(6, &pool, false)?;
    let actors = world.entities.component_owners(PLAYER).to_vec();
    for &actor in &actors[1..] {
        world
            .entities
            .get_mut(&actor)
            .unwrap()
            .components
            .remove(SETTING);
        world.set_motion_policy(actor, MotionPolicy::KINEMATIC)?;
    }
    world.set_motion_policy(actors[2], MotionPolicy::MOVING_COLLIDER)?;
    let mut schedule = Schedule::default();
    install(&world, &mut schedule)?;
    let mut world = Arc::new(world);
    let snap = |r: &mut SceneExtractor, w: &World| {
        r.snapshot(w, Camera::default(), [128, 72], Default::default())
    };
    let (mut exact, mut limited) = (SceneExtractor::default(), SceneExtractor::default());
    let first = snap(&mut exact, &world)?;
    snap(&mut limited, &world)?;
    limited.set_visual_pose_budget(std::num::NonZeroUsize::new(1), super::super::cosmetic_pose);
    limited.focused_actor = Some(actors[1]);
    schedule.tick(&mut world, 1. / 30., &pool)?;
    let full = snap(&mut exact, &world)?;
    let owners: BTreeMap<_, _> = full
        .scene
        .instances()
        .iter()
        .filter_map(|inst| {
            let mut id = *world.entities[&world.entities[&inst.id].skin_binding?]
                .skin
                .as_ref()?
                .joints
                .first()?;
            while !actors.contains(&id) {
                id = world.entities[&id].parent?;
            }
            Some((inst.id, id))
        })
        .collect();
    for held in [2, 1, 0] {
        let view = snap(&mut limited, &world)?;
        assert_eq!(view.scene.stats.held_visual_actors, held);
        let mut outdated = BTreeSet::new();
        for ((inst, current), prior) in view
            .scene
            .instances()
            .iter()
            .zip(full.scene.instances())
            .zip(first.scene.instances())
        {
            assert_eq!(
                (inst.id, inst.model, inst.palette_root),
                (current.id, current.model, current.palette_root)
            );
            if inst.palette != current.palette {
                let actor = owners[&inst.id];
                assert!(!actors[..3].contains(&actor));
                assert_eq!(inst.palette, prior.palette);
                outdated.insert(actor);
            }
        }
        assert_eq!(outdated.len(), held);
    }
    Ok(())
}

/// Paired render extraction using the same assets and the same sequence of authored edits.
#[test]
#[ignore = "release CPU character-space palette benchmark; uses Silver Wolf assets"]
fn silver_crowd_palette_paths() -> Result<()> {
    use crate::render::SceneExtractor;
    let count = std::env::var("RENDER_LAB_NPCS").map_or(Ok(500), |n| n.parse::<usize>())?;
    ensure!(count > 0);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(16).build()?;
    let mut character = population::populated_world(count, &pool, false)?;
    let mut reference = character.clone();
    let actors = character.entities.component_owners(PLAYER).to_vec();
    let mut selections: [Vec<(Id, Mat4)>; 3] = Default::default();
    for &actor in &actors {
        let p = player(&character, actor)?;
        // A skin without an explicit moving ancestor goes through the original world-space path.
        reference
            .entities
            .get_mut(&actor)
            .unwrap()
            .components
            .remove(crate::scene::MOTION_POLICY);
        selections[0].push((actor, character.entities[&actor].local));
        selections[1].push((p.bones[8], character.entities[&p.bones[8]].local));
        selections[2].extend(p.bones.iter().map(|&j| (j, character.entities[&j].local)));
    }
    let mut extractors = [SceneExtractor::default(), SceneExtractor::default()];
    for (case, edits) in ["root only", "right wrist", "all 21 animated joints"]
        .into_iter()
        .zip(selections)
    {
        let mut samples: [Vec<f64>; 2] = Default::default();
        let mut checks = [0usize; 2];
        for frame in 0..52 {
            let offset = 0.003 * (frame + 1) as f32;
            let edit =
                Mat4::from_rotation_translation(Quat::from_rotation_y(offset), Vec3::X * offset);
            for (id, local) in &edits {
                reference.set_local(*id, *local * edit)?;
                character.set_local(*id, *local * edit)?;
            }
            let mut views = [None, None];
            for side in [frame % 2, 1 - frame % 2] {
                let w = if side == 0 { &reference } else { &character };
                let start = Instant::now();
                let snap = extractors[side].snapshot(
                    w,
                    Camera::default(),
                    [1920, 1080],
                    Default::default(),
                )?;
                let ms = start.elapsed().as_secs_f64() * 1000.;
                if frame >= 4 {
                    samples[side].push(ms);
                    checks[side] += snap.scene.stats.palette_joints_examined;
                }
                views[side] = Some(snap)
            }
            if frame == 51 {
                for (a, b) in views[0]
                    .as_ref()
                    .unwrap()
                    .scene
                    .instances()
                    .iter()
                    .zip(views[1].as_ref().unwrap().scene.instances())
                {
                    assert_eq!((a.id, a.model), (b.id, b.model));
                    if let (Some(ap), Some(bp)) = (&a.palette, &b.palette) {
                        for (x, y) in ap.iter().zip(bp.iter()) {
                            assert!(
                                x.abs_diff_eq(b.palette_root * *y, 2e-4),
                                "divergent pose on {}",
                                a.id
                            );
                        }
                    }
                }
            }
        }
        for (side, name) in ["world", "character"].into_iter().enumerate() {
            samples[side].sort_by(f64::total_cmp);
            println!(
                "{count} actors {case} {name}: P50/P95 {:.2}/{:.2} ms, {} palette joints",
                samples[side][24],
                samples[side][45],
                checks[side] / 48
            );
        }
    }
    Ok(())
}

#[cfg(windows)]
#[test]
#[ignore = "release 1080p benchmark; requires Win32 Vulkan WSI and Silver Wolf assets"]
fn silver_crowd_window_profile() -> Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_POPUP};
    let count = std::env::var("RENDER_LAB_NPCS").map_or(Ok(300), |n| n.parse::<usize>())?;
    ensure!(count > 0, "positive population required");
    // The window owner is declared before the engine so its handle outlives the RHI thread.
    struct Window(isize);
    impl Drop for Window {
        fn drop(&mut self) {
            unsafe {
                DestroyWindow(self.0 as _);
            }
        }
    }
    let raw = unsafe {
        CreateWindowExW(
            0,
            windows_sys::w!("STATIC"),
            std::ptr::null(),
            WS_POPUP,
            0,
            0,
            1920,
            1080,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    ensure!(!raw.is_null(), "benchmark window unavailable");
    let window = Window(raw as isize);
    let mut engine = crate::experiments::open(scene()?)?;
    engine.set_ordered_completions(true);
    (engine.world, engine.schedule) = population(count, &engine.tasks)?;
    let cols = (count as f32 * 1.8).sqrt().ceil() as usize;
    engine.camera = Camera {
        target: Vec3::ZERO,
        distance: (cols as f32 * 2.3)
            .max(count.div_ceil(cols) as f32 * 7.)
            .max(8.),
        yaw: 0.10,
        pitch: 0.89,
    };
    unsafe {
        engine.set_viewport_window(window.0);
    }
    let (mut sent, mut done, mut observed) = (0, 0, 0);
    let mut last_tick = Instant::now();
    let mut last_frame = None;
    let mut series = BTreeMap::<&str, Vec<f64>>::new();
    let mut deadline = Instant::now() + Duration::from_secs(120);
    while observed < 116 {
        if sent - done < 2 {
            let now = Instant::now();
            let dt = if sent == 0 {
                1. / 60.
            } else {
                (now - last_tick).as_secs_f32().min(0.1)
            };
            last_tick = now;
            if engine.tick(dt, [1920, 1080])? {
                sent += 1;
            }
        }
        if let Some(frame) = engine.take_frame() {
            let frame = frame.map_err(anyhow::Error::msg)?;
            ensure!(frame.profile.wsi_binary_pairs > 0 && frame.extent == [1920, 1080]);
            done = frame.sequence;
            observed += 1;
            if observed > 20 {
                let p = &frame.profile;
                assert!(!p.extraction_full_rebuild);
                ensure!(
                    p.snapshot_id == done && p.tick_id >= done,
                    "lost or reordered completed snapshot"
                );
                if observed == 21 {
                    println!(
                        "pose: {} blocks, {} dirty locals, {} palette joints examined, {} dynamic upload bytes, {} scene table upload bytes, {} skin pose upload bytes",
                        p.pose_blocks_read,
                        p.pose_locals_read,
                        p.palette_joints_examined,
                        p.dynamic_upload_bytes,
                        p.scene_table_upload_bytes,
                        p.skin_pose_upload_bytes
                    );
                    println!(
                        "frame: {} passes, {} animation passes, {} recording chunks, {} queue submits, {} bound-device bytes",
                        frame.passes.len(),
                        frame
                            .passes
                            .iter()
                            .filter(|p| p.phase == crate::rdg::PassPhase::Animation)
                            .count(),
                        p.recording_chunks,
                        frame.submissions,
                        frame.memory.device_local_bytes
                    );
                }
                for (key, ms) in [
                    ("simulation", p.main_ms),
                    ("extraction", p.extract_ms),
                    ("render build", p.build_ms),
                    ("RDG compile", p.compile_ms),
                    ("RDG template hit", u8::from(p.rdg_template_hit) as f64),
                    ("render build+compile", p.build_ms + p.compile_ms),
                    ("RHI elapsed", p.rhi_ms),
                    ("render queue", p.render_queue_ms),
                    ("RHI queue", p.rhi_queue_ms),
                    ("submit to driver", p.submit_ms),
                    ("present request", p.present_ms),
                    ("tick to completion", p.tick_to_completion_ms.unwrap()),
                    (
                        "GPU animation scopes",
                        frame
                            .passes
                            .iter()
                            .filter(|p| p.phase == crate::rdg::PassPhase::Animation)
                            .map(|p| p.ms)
                            .sum(),
                    ),
                    (
                        "GPU span",
                        p.gpu_span_ms.context("aligned GPU timestamps required")?,
                    ),
                ] {
                    series.entry(key).or_default().push(ms);
                }
                if let Some((seq, at)) = last_frame
                    && done == seq + 1
                {
                    series
                        .entry("completion interval")
                        .or_default()
                        .push(frame.completed.duration_since(at).as_secs_f64() * 1000.);
                }
            }
            last_frame = Some((done, frame.completed));
            deadline = Instant::now() + Duration::from_secs(120);
        } else if sent - done >= 2 {
            std::thread::sleep(Duration::from_micros(200));
        }
        ensure!(
            Instant::now() < deadline,
            "timed out waiting for crowd frame"
        );
    }
    let intervals = &series["completion interval"];
    ensure!(intervals.len() == 96, "expected every completed frame");
    println!(
        "throughput {:.2} completed frames/s",
        1000. * intervals.len() as f64 / intervals.iter().sum::<f64>()
    );
    for (key, mut values) in series {
        values.sort_by(f64::total_cmp);
        println!(
            "{key}: samples {} P50 {:.2} P95 {:.2} ms",
            values.len(),
            values[values.len() / 2],
            values[((values.len() - 1) as f64 * 0.95).round() as usize]
        );
    }
    verify_character_coverage(engine, count)
}

#[cfg(windows)]
fn verify_character_coverage(engine: crate::engine::Engine, count: usize) -> Result<()> {
    // Release the WSI device before opening the offscreen readback device. Keep the last simulated pose.
    let (world, camera, post, technique, materials, pool) = (
        engine.world.clone(),
        engine.camera,
        engine.post,
        engine.technique.clone(),
        engine.materials.clone(),
        engine.tasks.clone(),
    );
    drop(engine);
    let mut gpu = crate::rhi::Backend::open()?;
    let mut view = crate::render::SceneExtractor::default().snapshot(
        &world,
        camera,
        [1920, 1080],
        materials,
    )?;
    view.buffer_atomic64 = gpu.buffer_atomic64();
    view.ray_caps = gpu.ray_capabilities();
    crate::render::post::Sequence::default().assign(post, &technique, &mut view);
    let mut graph = technique.build(&view)?;
    let ids = graph
        .products()
        .visibility
        .context("missing crowd visibility image")?;
    let size = graph
        .image_desc(ids)
        .context("visibility must be an image")?
        .extent;
    let bytes = size[0] as usize * size[1] as usize * 8;
    let readback = graph.resource(
        "Crowd surface and triangle IDs",
        crate::rdg::Storage::Buffer {
            bytes,
            initial: None,
        },
    );
    graph.pass(
        "Read crowd visibility",
        crate::rdg::Op::ReadPixels {
            image: ids,
            buffer: readback,
        },
    );
    // ReadPixels keeps the source image's extent. The frame readback exposes four bytes
    // per output pixel, so give it two output words for each visibility texel.
    graph.display_extent = Some([size[0] * 2, size[1]]);
    graph.set_output(readback);
    let frame = gpu.execute(graph.compile()?, &pool)?;
    ensure!(
        frame.pixels.len() == bytes,
        "incomplete crowd visibility readback"
    );

    let actors = world.entities.component_owners(SETTING);
    let owner = |mut id| loop {
        if actors.binary_search(&id).is_ok() {
            return Some(id);
        }
        id = world.entities.get(&id)?.parent?;
    };
    // This scene has ordinary surfaces only. Its cartoon draw registers one visibility ID per snapshot
    // instance in slot order. A skin can be shared by several parts belonging to the same character.
    let mut skin_owners = BTreeMap::new();
    let mut draw_owners = vec![None];
    for instance in view.scene.instances() {
        let skin_id = world.entities[&instance.id].skin_binding;
        let actor = if let Some(id) = skin_id {
            if let std::collections::btree_map::Entry::Vacant(slot) = skin_owners.entry(id) {
                let joints = &world.entities[&id]
                    .skin
                    .as_ref()
                    .context("missing crowd skin")?
                    .joints;
                let actor = joints
                    .first()
                    .and_then(|&j| owner(j))
                    .context("no character owns this skin")?;
                ensure!(
                    joints.iter().all(|&j| owner(j) == Some(actor)),
                    "skin mixes character skeletons"
                );
                slot.insert(actor);
            }
            Some(skin_owners[&id])
        } else {
            None
        };
        draw_owners.push(actor);
    }
    let mut visible = BTreeSet::new();
    let mut pixels = 0usize;
    for pixel in frame.pixels.chunks_exact(8) {
        let surface = u32::from_le_bytes(pixel[..4].try_into()?) as usize;
        if let Some(actor) = draw_owners
            .get(surface)
            .context("unknown crowd surface ID")?
        {
            visible.insert(*actor);
            pixels += 1;
        }
    }
    println!(
        "Visibility after timing: {} of {} characters, {pixels} character pixels in {}×{}",
        visible.len(),
        count,
        size[0],
        size[1]
    );
    ensure!(
        visible.len() >= count.div_ceil(2) && pixels >= visible.len() * 4,
        "insufficient character coverage: {} of {count} characters, {pixels} pixels",
        visible.len()
    );
    Ok(())
}

#[path = "crowd_draw_test.rs"]
mod crowd_draw_test;

#[path = "structural_profile.rs"]
mod structural_profile;
