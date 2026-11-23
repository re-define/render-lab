use anyhow::Result;
use glam::{Mat4, Vec3};
use rayon::ThreadPoolBuilder;
use render_lab::{
    engine::{
        Edit, Schedule,
        SimulationData::{Component, LocalTransform},
        System, SystemAccess,
    },
    experiments::motion,
    render::{Camera, SceneExtractor},
    scene::{Id, LocalPose, World},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Declared(SystemAccess);
impl System for Declared {
    fn access(&self) -> SystemAccess {
        self.0
    }
    fn update(&self, _: &World, _: f32) -> Result<Edit> {
        Ok(Box::new(|_| {}))
    }
}

#[test]
fn automatic_groups_keep_registration_order_and_explain_boundaries() {
    let mut schedule = Schedule::default();
    let write = |name| {
        Arc::new(Declared(SystemAccess {
            name,
            reads: &[],
            writes: &[Component("clock")],
        }))
    };
    let read = |name| {
        Arc::new(Declared(SystemAccess {
            name,
            reads: &[Component("clock")],
            writes: &[],
        }))
    };
    let unrelated = || {
        Arc::new(Declared(SystemAccess {
            name: "pose",
            reads: &[],
            writes: &[LocalTransform],
        }))
    };
    schedule.add_system(write("advance"));
    schedule.add_system(unrelated());
    schedule.add_system(read("consume"));
    schedule.add_system(read("observe"));
    schedule.add_system(write("reset"));
    schedule.stage(vec![write("one"), write("two")]);
    schedule.add_system(unrelated());
    let groups: Vec<Vec<_>> = schedule
        .stages()
        .map(|(group, _)| group.iter().map(|s| s.name).collect())
        .collect();
    assert_eq!(
        groups,
        [
            vec!["advance", "pose"],
            vec!["consume", "observe"],
            vec!["reset"],
            vec!["one", "two"],
            vec!["pose"]
        ]
    );
    let report = schedule.describe();
    for conflict in [
        "stage boundary: write/read component clock: advance → consume",
        "stage boundary: read/write component clock: observe → reset",
        "write/write component clock: one → two",
    ] {
        assert!(report.contains(conflict), "{report}");
    }
    // Older plugins inherit whole-world access, including conflicts with named resources.
    let mut schedule = Schedule::default();
    schedule.add_system(Arc::new(FailSecond(AtomicUsize::new(0))));
    schedule.add_system(Arc::new(Declared(SystemAccess {
        name: "physics",
        reads: &[render_lab::engine::SimulationData::Resource("gravity")],
        writes: &[],
    })));
    assert_eq!(schedule.stages().count(), 2);
    assert!(
        schedule
            .describe()
            .contains("stage boundary: write/read resource gravity")
    );
}

struct Nudge {
    id: Id,
    delta: f32,
}
impl System for Nudge {
    fn access(&self) -> SystemAccess {
        SystemAccess {
            name: "Nudge",
            reads: &[LocalTransform],
            writes: &[LocalTransform],
        }
    }
    fn update(&self, world: &World, dt: f32) -> Result<Edit> {
        let local =
            world.entities[&self.id].local * Mat4::from_translation(Vec3::X * self.delta * dt);
        let pose = LocalPose::prepare(world, [(self.id, local)])?;
        Ok(Box::new(move |world| pose.apply(world)))
    }
}

#[test]
fn fixed_animation_replays_with_the_same_world_and_extraction_across_workers_and_frame_cadence()
-> Result<()> {
    let replay = |workers, fps| -> Result<_> {
        let pool = ThreadPoolBuilder::new().num_threads(workers).build()?;
        let mut world = motion::scene();
        motion::set_goal(&mut world, 1.4, 0.45)?;
        let id = world.spawn(Default::default());
        let mut world = Arc::new(world);
        let mut stage = Schedule::default();
        stage.fixed_stage(
            Duration::from_secs_f64(1. / 60.),
            4,
            vec![
                Arc::new(motion::Matcher::default()),
                Arc::new(Nudge { id, delta: 2. }),
                Arc::new(Nudge { id, delta: 3. }),
            ],
        )?;
        let mut extractor = SceneExtractor::default();
        let mut snapshot =
            |w: &World| extractor.snapshot(w, Camera::default(), [64, 32], Default::default());
        snapshot(&world)?;
        for _ in 0..fps {
            stage.tick(&mut world, 1. / fps as f32, &pool)?;
        }
        let stats = snapshot(&world)?.scene.stats;
        let steps = stage.stages().next().unwrap().1;
        // Both writers see each substep's original pose. The second registered writer commits last.
        assert!((world.entities[&id].local.w_axis.x - 3.).abs() < 1e-5);
        assert_eq!(steps, 60);
        assert!(stats.transforms_updated > 1 && stats.palettes_updated > 0);
        assert!(stats.entities_visited < world.entities.len());
        Ok((serde_json::to_value(world.as_ref())?, stats, steps))
    };
    let reference = replay(1, 60)?;
    for (workers, fps) in [(4, 60), (1, 30), (4, 30)] {
        assert_eq!(reference, replay(workers, fps)?);
    }
    Ok(())
}

#[test]
fn pose_batch_validates_before_commit_and_journals_only_changed_rows() -> Result<()> {
    let mut world = World::demo();
    let id = world
        .entities
        .iter()
        .find(|(_, e)| e.instance.is_some())
        .map(|(&id, _)| id)
        .unwrap();
    let removed = world.spawn(Default::default());
    let first = Mat4::from_translation(Vec3::X);
    let last = Mat4::from_translation(Vec3::Y);
    let mut extractor = SceneExtractor::default();
    let mut snapshot =
        |w: &World| extractor.snapshot(w, Camera::default(), [64, 32], Default::default());
    snapshot(&world)?;
    for (bad_id, invalid) in [
        (Id::MAX, first),
        (id, Mat4::from_cols_array(&[f32::NAN; 16])),
    ] {
        assert!(LocalPose::prepare(&world, [(id, first), (bad_id, invalid)]).is_err());
    }
    assert_eq!(snapshot(&world)?.scene.stats.entities_visited, 0);
    let pose = LocalPose::prepare(&world, [(id, first), (removed, last), (id, last)])?;
    world.entities.remove(&removed);
    snapshot(&world)?;
    pose.apply(&mut world);
    assert_eq!(world.entities[&id].local, last);
    let stats = snapshot(&world)?.scene.stats;
    assert_eq!((stats.entities_visited, stats.instances_updated), (1, 1));
    LocalPose::prepare(&world, [(id, last)])?.apply(&mut world);
    assert_eq!(snapshot(&world)?.scene.stats.entities_visited, 0);
    Ok(())
}

struct FailSecond(AtomicUsize);
impl System for FailSecond {
    fn update(&self, _: &World, _: f32) -> Result<Edit> {
        anyhow::ensure!(
            self.0.fetch_add(1, Ordering::SeqCst) != 1,
            "injected failure"
        );
        Ok(Box::new(|_| {}))
    }
}

#[test]
fn failed_substep_retries_the_uncommitted_remainder() -> Result<()> {
    let mut world = World::default();
    let id = world.spawn(Default::default());
    let mut world = Arc::new(world);
    let pool = ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut stage = Schedule::default();
    stage.fixed_stage(
        Duration::from_millis(125),
        4,
        vec![
            Arc::new(Nudge { id, delta: 1. }),
            Arc::new(FailSecond(AtomicUsize::new(0))),
        ],
    )?;
    assert!(stage.tick(&mut world, 0.375, &pool).is_err());
    assert_eq!(stage.stages().next().unwrap().1, 1);
    assert_eq!(world.entities[&id].local.w_axis.x, 0.125);
    stage.tick(&mut world, 0., &pool)?;
    assert_eq!(stage.stages().next().unwrap().1, 3);
    assert_eq!(world.entities[&id].local.w_axis.x, 0.375);
    Ok(())
}
