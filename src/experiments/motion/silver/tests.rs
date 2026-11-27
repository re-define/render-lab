//! Motion player integration and search regressions.
use super::*;
use crate::engine::System;

#[test]
fn blocked_and_carried_motion_use_solver_space_and_database_units() -> Result<()> {
    let world = scene()?;
    let mut p = player(&world, actor(&world).unwrap())?.into_owned();
    p.move_root = true;
    p.speed = 1.;
    p.side = 0.;
    p.turn = 0.;
    let facing = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
    let moving_platform = Vec3::new(3., 0., -1.);
    let normal = facing * Vec3::NEG_Z;
    let bank = SilverMatcher::load()?;
    let pose = &bank.poses[p.sample];
    p.observe_motion(
        Mat4::from_quat(facing),
        moving_platform,
        moving_platform,
        Some(normal),
        [false, true],
    );
    let blocked = p.query_features(pose);
    for feature in blocked[..12].chunks_exact(4) {
        assert!(feature[0].abs() < 1e-5 && feature[1].abs() < 1e-5);
    }
    assert_eq!([blocked[18], blocked[20]], [0., 0.]);
    assert_eq!(&blocked[21..24], &pose.feet[9..12]);
    bank.step(&mut p)?;
    assert!(bank.search.candidate(p.sample) == search::Candidate::Idle);
    let velocity = moving_platform + facing * Vec3::new(0.5, 0., 0.25);
    p.observe_motion(
        Mat4::from_quat(facing),
        velocity,
        moving_platform,
        None,
        [true, false],
    );
    let released = p.query_features(pose);
    assert!(released[1] > blocked[1]);
    assert!((released[21] + 0.5).abs() < 1e-5 && (released[23] + 0.25).abs() < 1e-5);
    assert_eq!(&released[18..21], &pose.feet[6..9]);
    p.observe_motion(
        Mat4::from_quat(facing),
        moving_platform + facing * (p.speed * Vec3::Z),
        moving_platform,
        None,
        [false; 2],
    );
    bank.step(&mut p)?;
    assert!(bank.search.candidate(p.sample) == search::Candidate::Steady);
    Ok(())
}

#[test]
fn authored_locomotion_root_reproduces_the_baked_track_and_resumes_mid_sample() -> Result<()> {
    let mut world = scene()?;
    let id = actor(&world).unwrap();
    set_root_source(&mut world, Some(id), "clip")?;
    let mut p = player(&world, id)?.into_owned();
    let bank = SilverMatcher::load()?;
    // Check the independent 14- and 23-frame channels against the reconstructed 6-frame chain.
    let stride = 28 + 4 * p.bones.len();
    for (range, _) in &bank.clips {
        for row in (range.start..range.end - 23).step_by(7) {
            for (offset, horizon) in [(4, 14), (8, 23)] {
                let mut sum = Vec2::ZERO;
                for pose in &bank.poses[row..row + horizon] {
                    sum += glam::Mat2::from_angle(bank.poses[row].heading - pose.heading)
                        * pose.root_step;
                }
                let value = |k: usize| {
                    let at = (row * stride + offset + k) * 4;
                    f32::from_le_bytes(DATA[at..at + 4].try_into().unwrap())
                };
                assert!(sum.distance(Vec2::new(value(0), value(1))) < 1e-4);
            }
        }
    }
    let span = &bank
        .clips
        .iter()
        .find(|(_, name)| name == "walk_41")
        .unwrap()
        .0;
    p.sample = span.start + 12;
    p.move_root = true;
    assert!(matches!(p.root_source, profile::RootSource::Clip));
    p.fraction = 0.;
    p.yaw = 0.;
    p.search_in = 20;
    let read = |_: usize, id| Ok(world.entities[&id].local);
    let start = p.sample;
    let (half, _) = bank.sample_pose(&mut p, Mat4::IDENTITY, 0.5 / HZ, read)?;
    let encoded = serde_json::to_vec(&p)?;
    let mut loaded: State = serde_json::from_slice(&encoded)?;
    let (resume, _) = bank.sample_pose(&mut loaded, half, 0.5 / HZ, read)?;
    let (reference, _) = bank.sample_pose(&mut p, half, 0.5 / HZ, read)?;
    assert!(resume.abs_diff_eq(reference, 1e-6));
    let expected = bank.poses[start].root_step;
    assert!(
        Vec2::new(resume.w_axis.x, resume.w_axis.z).distance(expected) < 1e-5
            && expected.length() > 0.001
    );
    Ok(())
}

#[test]
fn standalone_motion_preserves_fixed_root_and_rejects_unowned_dynamic_authority() -> Result<()> {
    let mut world = scene()?;
    let id = actor(&world).unwrap();
    let before = world.entities[&id].local;
    set_actor_goal(&mut world, id, 0, 1)?;
    world.set_motion_policy(id, MotionPolicy::STATIC)?;
    let mut sched = crate::engine::Schedule::default();
    sched.add_system(Arc::new(SilverMatcher::load()?));
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
    let mut w = Arc::new(world);
    sched.tick(&mut w, 1. / 30., &pool)?;
    assert_eq!(w.entities[&id].local, before);
    Arc::make_mut(&mut w).set_motion_policy(id, MotionPolicy::DYNAMIC)?;
    let stamp = player(&w, id)?.sample;
    let error = sched.tick(&mut w, 1. / 30., &pool).unwrap_err();
    assert!(format!("{error:#}").contains("requires a physics solver"));
    assert_eq!(stamp, player(&w, id)?.sample);
    assert_eq!(w.entities[&id].local, before);
    Ok(())
}
#[test]
fn silver_joints_and_fracture_pieces_share_a_batch_until_rebound() -> Result<()> {
    use crate::{engine::Schedule, experiments::effects, render::MaterialPrograms};
    let rig = scene()?;
    let source = actor(&rig).unwrap();
    let mut binding = player(&rig, source)?.into_owned();
    let mut world = effects::destruction()?;
    // Copy the animated local poses into the destruction scene's entity namespace.
    for bone in &mut binding.bones {
        *bone = world.spawn(Entity {
            local: rig.entities[bone].local,
            ..Default::default()
        });
    }
    let hip = binding.bones[0];
    let root = world.spawn(Entity {
        local: rig.entities[&source].local,
        ..Default::default()
    });
    world.set_typed_component(root, PLAYER, binding);
    let (&piece, _) = world
        .entities
        .iter()
        .find(|(_, e)| e.name == "Crystal fragment")
        .unwrap();
    let (hip_before, piece_before) = (world.entities[&hip].local, world.entities[&piece].local);
    let mut schedule = Schedule::default();
    effects::configure(
        &world,
        &mut MaterialPrograms::default(),
        &mut schedule,
        true,
    )?;
    schedule.add_system(Arc::new(SilverMatcher::load()?));
    schedule.bind_transforms(&world)?;
    let batches: Vec<_> = schedule
        .stages()
        .map(|(batch, _)| batch.iter().map(|a| a.name).collect::<Vec<_>>())
        .collect();
    assert_eq!(
        batches,
        [vec!["Effect playback", "Silver Wolf motion matcher"]]
    );
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut world = Arc::new(world);
    schedule.tick(&mut world, 1.125, &pool)?;
    assert_ne!(world.entities[&piece].local, piece_before);
    assert_ne!(world.entities[&hip].local, hip_before);
    let mut binding = player(&world, root)?.into_owned();
    binding.bones[0] = piece;
    Arc::make_mut(&mut world).set_typed_component(root, PLAYER, binding);
    schedule.tick(&mut world, 1. / 30., &pool)?;
    assert_eq!(schedule.stages().count(), 2);
    assert!(schedule.describe().contains(&format!(
        "write/write LocalTransform entity {piece}: Effect playback → Silver Wolf motion matcher"
    )));
    Ok(())
}

#[test]
fn idle_pose_keeps_the_support_foot_at_ground_level() -> Result<()> {
    let world = scene()?;
    let transforms = world.transforms()?;
    let support_height = world
        .entities
        .iter()
        .filter(|(_, entity)| {
            entity.name.starts_with("Left toe_") || entity.name.starts_with("Right toe_")
        })
        .map(|(id, _)| transforms[id].w_axis.y)
        .fold(f32::INFINITY, f32::min);
    assert!((-0.02..0.08).contains(&support_height), "{support_height}");
    Ok(())
}

#[test]
fn keyboard_prediction_matches_root_motion_and_stops_on_release() -> Result<()> {
    let bank = SilverMatcher::load()?;
    let initial = scene()?;
    let id = *initial
        .entities
        .iter()
        .find(|(_, e)| e.components.contains_key(PLAYER))
        .unwrap()
        .0;
    for (side, forward, expected) in [
        (0, 1, "walk"),
        (0, -1, "backward"),
        (-1, 0, "strafe-left"),
        (1, 0, "strafe-right"),
    ] {
        let mut world = initial.clone();
        set_keyboard_goal(&mut world, side, forward)?;
        let state = player(&world, id)?.into_owned();
        let endpoint = state.predict(HORIZONS[2]).0;
        for _ in 0..LOOKAHEAD {
            bank.update(&world, 1. / HZ)?(&mut world);
        }
        let root = world.entities[&id].local.w_axis.truncate();
        assert!(root.abs_diff_eq(Vec3::new(endpoint.x, 0., endpoint.y), 1e-5));
        let mut matches = 0;
        for _ in 0..60 {
            bank.update(&world, 1. / HZ)?(&mut world);
            matches += usize::from(player(&world, id)?.clip.split('_').next() == Some(expected));
        }
        assert!(matches >= 30, "{expected} selected for {matches}/60 frames");

        set_keyboard_goal(&mut world, 0, 0)?;
        for _ in 0..30 {
            bank.update(&world, 1. / HZ)?(&mut world);
        }
        let stopped = world.entities[&id].local.w_axis.truncate();
        bank.update(&world, 1. / HZ)?(&mut world);
        assert!(
            world.entities[&id]
                .local
                .w_axis
                .truncate()
                .abs_diff_eq(stopped, 1e-4)
        );
        let clip = player(&world, id)?.clip.clone();
        assert!(clip.starts_with("idle"), "{expected} settled in {clip}");
    }
    Ok(())
}

#[test]
#[ignore = "release timing with the Silver Wolf asset"]
fn benchmark_silver_motion() -> Result<()> {
    use std::time::Instant;

    let bank = SilverMatcher::load()?;
    let mut world = scene()?;
    let id = *world
        .entities
        .iter()
        .find(|(_, e)| e.components.contains_key(PLAYER))
        .unwrap()
        .0;
    let mut state = player(&world, id)?.into_owned();
    let goals = [
        (0., 0., 0.),
        (1.35, 0., 0.),
        (3., 0., 0.),
        (1.35, -1., 0.),
        (0., 0., 1.2),
        (-0.9, 0., 0.),
    ];
    let mut kd_queries = Vec::new();
    let mut scan_queries = Vec::new();
    let mut kd_evaluated = Vec::new();
    let mut scan_evaluated = Vec::new();
    let mut updates_with_search = Vec::new();
    let mut updates_between_searches = Vec::new();
    for i in 0..2400 {
        let (speed, turn, side) = goals[i / 60 % goals.len()];
        (state.speed, state.turn, state.side, state.search_in) = (speed, turn, side, 0);
        let mut reference = state.clone();
        reference.mode = search::Mode::Scan;
        let measure = |player: &mut State| -> Result<_> {
            let start = Instant::now();
            bank.step(std::hint::black_box(player))?;
            Ok(start.elapsed())
        };
        let (kd_time, scan_time) = if i % 2 == 0 {
            (measure(&mut state)?, measure(&mut reference)?)
        } else {
            let scan_time = measure(&mut reference)?;
            (measure(&mut state)?, scan_time)
        };
        assert_eq!(
            (state.sample, state.cost),
            (reference.sample, reference.cost)
        );
        if i % 60 == 0 {
            set_goal(&mut world, speed, turn, side)?;
        }
        let start = Instant::now();
        bank.update(&world, 1. / HZ)?(&mut world);
        let elapsed = start.elapsed();
        if i >= 240 {
            kd_queries.push(kd_time);
            scan_queries.push(scan_time);
            kd_evaluated.push(state.evaluated);
            scan_evaluated.push(reference.evaluated);
            if player(&world, id)?.search_in == 4 {
                updates_with_search.push(elapsed);
            } else {
                updates_between_searches.push(elapsed);
            }
        }
    }
    for (label, mut samples) in [
        ("kd_query", kd_queries),
        ("scan_query", scan_queries),
        ("update_with_search", updates_with_search),
        ("update_between_searches", updates_between_searches),
    ] {
        samples.sort_unstable();
        let n = samples.len();
        let us = |percent: usize| {
            samples[((n - 1) * percent + 50usize) / 100].as_secs_f64() * 1_000_000.
        };
        println!(
            "{label}: n={n} median_us={:.2} p95_us={:.2}",
            us(50),
            us(95)
        );
    }
    for (method, mut counts) in [("kd", kd_evaluated), ("scan", scan_evaluated)] {
        counts.sort_unstable();
        println!(
            "{method}_evaluated: median={} p95={} max={}",
            counts[counts.len() / 2],
            counts[counts.len() * 95 / 100],
            counts.last().unwrap()
        );
    }
    Ok(())
}

#[test]
fn tree_agrees_with_scan_for_perturbed_poses_and_clip_boundaries() -> Result<()> {
    let bank = SilverMatcher::load()?;
    let stride = DATA.len() / bank.poses.len();
    let mut random = 9173u32;
    let mut draw = || {
        random = random.wrapping_mul(1664525).wrapping_add(1013904223);
        random
    };
    for i in 0..512 {
        let (span, _) = &bank.clips[draw() as usize % bank.clips.len()];
        let source = draw() as usize % bank.poses.len();
        let magnitude = [0., 0.015, 0.25, 2.0][i % 4];
        let raw = std::array::from_fn(|k| {
            let at = source * stride + 4 * k;
            f32::from_le_bytes(DATA[at..at + 4].try_into().unwrap())
                + magnitude * (draw() as f32 / u32::MAX as f32 - 0.5)
        });
        let next = match i % 3 {
            0 => span.start,
            1 => span.end - 1,
            _ => span.start + (draw() as usize % span.len()),
        };
        let class = match i % 4 {
            0 => Some(search::Candidate::Idle),
            1 => Some(search::Candidate::Steady),
            _ => None,
        };
        let kd = bank
            .search
            .nearest(&raw, search::Mode::Kd, next, span, &bank.clips, class);
        let scan = bank
            .search
            .nearest(&raw, search::Mode::Scan, next, span, &bank.clips, class);
        assert_eq!((kd.sample, kd.cost), (scan.sample, scan.cost), "query {i}");
    }
    Ok(())
}

#[test]
fn kimodo_search_tracks_direction_speed_and_strafe() -> Result<()> {
    let index: Index = serde_json::from_str(INDEX)?;
    let bank = SilverMatcher::load()?;
    assert_eq!(index.bones.len(), 21);
    assert_eq!(bank.clips.len(), 39);
    assert_eq!(bank.poses.len(), 4680);
    let mut p = State {
        mode: Default::default(),
        evaluated: 0,
        bones: vec![],
        rig: None,
        root_source: Default::default(),
        observed: None,
        layout: None,
        standalone_velocity: None,
        hip_origin: Vec3::ZERO,
        speed: 0.,
        turn: 0.,
        side: 0.,
        move_root: false,
        velocity: Vec2::ZERO,
        acceleration: Vec2::ZERO,
        yaw_rate: 0.,
        transition: None,
        sample: bank
            .clips
            .iter()
            .find(|(_, name)| name == "idle_41")
            .unwrap()
            .0
            .start
            + 10,
        clip: String::new(),
        cost: 0.,
        fraction: 0.,
        search_in: 0,
        yaw: 0.,
    };
    for (speed, turn, side, expected) in [
        (0., 0., 0., "idle"),
        (1.35, 0., 0., "walk"),
        (3., 0., 0., "run"),
        (1.35, -1., 0., "walk-left"),
        (0., 0., 1.2, "strafe-right"),
        (-0.9, 0., 0., "backward"),
    ] {
        (p.speed, p.turn, p.side, p.search_in) = (speed, turn, side, 0);
        let mut matches = 0;
        for _ in 0..60 {
            bank.step(&mut p)?;
            let (range, _) = bank.clips.iter().find(|(_, name)| name == &p.clip).unwrap();
            assert!(range.contains(&p.sample) && p.sample + LOOKAHEAD < range.end);
            assert!(p.cost.is_finite());
            if p.clip.split('_').next() == Some(expected) {
                matches += 1;
            }
        }
        assert!(matches >= 30, "{expected} selected for {matches}/60 frames");
    }
    Ok(())
}
