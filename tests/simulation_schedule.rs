use anyhow::Result;
use glam::{Mat4, Vec3};
use rayon::ThreadPoolBuilder;
use render_lab::{
    engine::{
        Edit, Schedule, SimulationData,
        SimulationData::{Component, LocalTransform, LocalTransformsById},
        SimulationSystem, System, SystemAccess, SystemContext, SystemOrder, TransformBindings,
        TransformTargets,
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
impl SimulationSystem for Declared {
    fn access(&self) -> SystemAccess {
        self.0
    }
    fn run(&self, _: &mut SystemContext, _: f32) -> Result<()> {
        Ok(())
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
            vec!["one"],
            vec!["two"],
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
impl SimulationSystem for Nudge {
    fn access(&self) -> SystemAccess {
        SystemAccess {
            name: "Nudge",
            reads: &[LocalTransform],
            writes: &[LocalTransform],
        }
    }
    fn run(&self, cx: &mut SystemContext, dt: f32) -> Result<()> {
        let local = cx.local(self.id)?.unwrap() * Mat4::from_translation(Vec3::X * self.delta * dt);
        cx.set_pose([(self.id, local)])
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
        // The second writer observes the first writer's committed pose in every substep.
        assert!((world.entities[&id].local.w_axis.x - 5.).abs() < 1e-5);
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
type Job = dyn Fn(&mut SystemContext<'_>, f32) -> Result<()> + Send + Sync;
type Selector = dyn Fn(&TransformBindings<'_>) -> Result<TransformTargets> + Send + Sync;
struct Task {
    targets: Box<Selector>,
    access: SystemAccess,
    order: SystemOrder,
    job: Box<Job>,
}
impl SimulationSystem for Task {
    fn transform_targets(&self, bindings: &TransformBindings<'_>) -> Result<TransformTargets> {
        (self.targets)(bindings)
    }
    fn access(&self) -> SystemAccess {
        self.access
    }
    fn order(&self) -> SystemOrder {
        self.order
    }
    fn run(&self, cx: &mut SystemContext<'_>, dt: f32) -> Result<()> {
        (self.job)(cx, dt)
    }
}
fn task(
    key: &'static str,
    after: &'static [&'static str],
    reads: &'static [SimulationData],
    writes: &'static [SimulationData],
    job: impl Fn(&mut SystemContext<'_>, f32) -> Result<()> + Send + Sync + 'static,
) -> Arc<dyn System> {
    scoped_task(
        key,
        after,
        reads,
        writes,
        |_| Ok(TransformTargets::default()),
        job,
    )
}

fn scoped_task(
    key: &'static str,
    after: &'static [&'static str],
    reads: &'static [SimulationData],
    writes: &'static [SimulationData],
    targets: impl Fn(&TransformBindings<'_>) -> Result<TransformTargets> + Send + Sync + 'static,
    job: impl Fn(&mut SystemContext<'_>, f32) -> Result<()> + Send + Sync + 'static,
) -> Arc<dyn System> {
    Arc::new(Task {
        targets: Box::new(targets),
        access: SystemAccess {
            name: key,
            reads,
            writes,
        },
        order: SystemOrder {
            key: Some(key),
            after,
        },
        job: Box::new(job),
    })
}

#[test]
fn reverse_registration_orders_physics_before_animation_in_each_substep() -> Result<()> {
    use SimulationData::Resource;
    for workers in [1, 4] {
        let pool = ThreadPoolBuilder::new().num_threads(workers).build()?;
        let mut w = World::default();
        let id = w.spawn(Default::default());
        let mut w = Arc::new(w);
        let mut s = Schedule::default();
        let physics = task(
            "physics",
            &[],
            &[Resource("position")],
            &[Resource("position")],
            |cx, dt| {
                cx.set_resource(
                    "position",
                    cx.resource::<f32>("position")?.copied().unwrap_or_default() + dt,
                )
            },
        );
        let anim = task(
            "animation",
            &["physics"],
            &[Resource("position")],
            &[LocalTransform],
            move |cx, _| {
                let x = *cx.resource::<f32>("position")?.unwrap();
                cx.set_pose([(id, Mat4::from_translation(Vec3::X * x))])
            },
        );
        s.fixed_stage(Duration::from_millis(125), 4, vec![anim, physics])?;
        s.tick(&mut w, 0.375, &pool)?;
        assert_eq!(w.entities[&id].local.w_axis.x, 0.375);
        assert!(s.describe().contains("after physics"));
    }
    Ok(())
}

#[test]
fn named_domain_interleaves_plugins_and_rolls_back_an_entire_failed_step() -> Result<()> {
    use render_lab::engine::{FixedDomain, SimulationData::Component};
    let clock = FixedDomain {
        name: "replay",
        step: Duration::from_secs_f64(1. / 60.),
        max_steps: 4,
    };
    for workers in [1, 4] {
        let pool = ThreadPoolBuilder::new().num_threads(workers).build()?;
        let run = |cadence: &[f32], fail: bool| -> Result<_> {
            let fail_once = std::sync::atomic::AtomicBool::new(fail);
            let mut base = World::default();
            let id = base.spawn(Default::default());
            let mut world = Arc::new(base);
            let mut sched = Schedule::default();
            sched.set_profiling(true);
            // Plugins register in reverse dependency order. An adjacent independent clock
            // keeps its own catch-up limit and is not inserted between these systems.
            sched.add_fixed_system(
                clock,
                task(
                    "feedback",
                    &["body"],
                    &[Component("path")],
                    &[Component("path")],
                    move |cx, _| {
                        let n = cx.component::<u64>(id, "path")?.unwrap();
                        if fail_once.swap(false, Ordering::Relaxed) {
                            anyhow::bail!("fault after producer commit")
                        }
                        cx.set_component(id, "path", (*n * 10 + 2).into())
                    },
                ),
            )?;
            sched.fixed_stage(Duration::from_millis(250), 1, Vec::new())?;
            sched.add_fixed_system(
                clock,
                task(
                    "body",
                    &[],
                    &[Component("path")],
                    &[Component("path")],
                    move |cx, _| {
                        let n = cx
                            .component::<u64>(id, "path")?
                            .map(|x| *x)
                            .unwrap_or_default();
                        cx.set_component(id, "path", (n * 10 + 1).into())
                    },
                ),
            )?;
            assert!(
                sched
                    .add_fixed_system(
                        FixedDomain {
                            max_steps: 1,
                            ..clock
                        },
                        Arc::new(Nudge { id: 1, delta: 0. })
                    )
                    .is_err()
            );
            for &dt in cadence {
                if sched.tick(&mut world, dt, &pool).is_err() {
                    let measured = sched.tick_profile().unwrap();
                    assert!(measured.counts.undo_records > 0 && measured.counts.components > 0);
                    assert!(
                        measured.evaluate > Duration::ZERO
                            && measured.capture_undo > Duration::ZERO
                            && measured.rollback > Duration::ZERO
                    );
                    assert!(fail && !world.entities[&id].components.contains_key("path"));
                    sched.tick(&mut world, 0., &pool)?;
                }
            }
            assert_eq!(sched.stages().next().unwrap().1, 2);
            Ok(world.entities[&id].components["path"].as_u64().unwrap())
        };
        for fail in [false, true] {
            assert_eq!(run(&[1. / 30.], fail)?, 1212);
            assert_eq!(run(&[1. / 60., 1. / 60.], fail)?, 1212);
        }
    }
    Ok(())
}

#[test]
fn independent_systems_on_different_branches_actually_overlap() -> Result<()> {
    use std::sync::{Condvar, Mutex};
    let both = Arc::new((Mutex::new(0), Condvar::new()));
    let rendezvous = || {
        let both = both.clone();
        move |cx: &mut SystemContext<'_>, _: f32| {
            assert!(cx.resource::<u32>("weather")?.is_none());
            let mut count = both.0.lock().unwrap();
            *count += 1;
            both.1.notify_all();
            let (count, _) = both
                .1
                .wait_timeout_while(count, Duration::from_secs(3), |v| *v < 2)
                .unwrap();
            anyhow::ensure!(*count == 2, "independent systems did not run concurrently");
            Ok(())
        }
    };
    let mut s = Schedule::default();
    s.add_system(task(
        "first",
        &[],
        &[SimulationData::Resource("weather")],
        &[Component("a")],
        rendezvous(),
    ));
    s.add_system(task("dependent", &["first"], &[], &[], |_, _| Ok(())));
    s.add_system(task(
        "last",
        &[],
        &[SimulationData::Resource("weather")],
        &[Component("b")],
        rendezvous(),
    ));
    let groups = s
        .stages()
        .map(|(g, _)| g.iter().map(|s| s.name).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    assert_eq!(groups, [vec!["first", "last"], vec!["dependent"]]);
    s.tick(
        &mut Arc::new(World::default()),
        0.,
        &ThreadPoolBuilder::new().num_threads(2).build()?,
    )
}

#[test]
fn invalid_dependencies_are_rejected_before_any_world_update() -> Result<()> {
    let pool = ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut w = Arc::new(World::default());
    let mut s = Schedule::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    s.stage(vec![task("start", &[], &[], &[], move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })]);
    s.add_system(task("consumer", &["producer"], &[], &[], |_, _| Ok(())));
    assert!(
        s.tick(&mut w, 0., &pool)
            .unwrap_err()
            .to_string()
            .contains("unknown")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    s.add_system(task("producer", &[], &[], &[], |_, _| Ok(())));
    s.validate()?;
    s.add_system(task("producer", &[], &[], &[], |_, _| Ok(())));
    assert!(s.validate().unwrap_err().to_string().contains("duplicate"));
    let mut s = Schedule::default();
    s.add_system(task("a", &["b"], &[], &[], |_, _| Ok(())));
    s.add_system(task("b", &["a"], &[], &[], |_, _| Ok(())));
    assert!(s.validate().unwrap_err().to_string().contains("cycle"));
    let mut s = Schedule::default();
    s.stage(vec![task("a", &["b"], &[], &[], |_, _| Ok(()))]);
    s.fixed_stage(
        Duration::from_secs(1),
        1,
        vec![task("b", &[], &[], &[], |_, _| Ok(()))],
    )?;
    assert!(
        s.validate()
            .unwrap_err()
            .to_string()
            .contains("later clock/stage")
    );
    Ok(())
}

#[test]
fn undeclared_access_discards_staged_edits_and_rolls_back_prior_batches() -> Result<()> {
    use SimulationData::Resource;
    for read_violation in [false, true] {
        let mut w = World::default();
        let id = w.spawn(Default::default());
        w.entities
            .get_mut(&id)
            .unwrap()
            .components
            .insert("state".into(), serde_json::json!(7));
        let mut w = Arc::new(w);
        let pool = ThreadPoolBuilder::new().num_threads(2).build()?;
        let mut s = Schedule::default();
        s.stage(vec![task(
            "seed",
            &[],
            &[],
            &[Component("state"), Resource("clock")],
            move |cx, _| {
                cx.set_typed_component(id, "state", 17u32)?;
                cx.set_resource("clock", 12u32)
            },
        )]);
        s.tick(&mut w, 0., &pool)?;
        let mut s = Schedule::default();
        s.add_system(task(
            "modify",
            &[],
            &[],
            &[
                LocalTransform,
                Component("state"),
                Component("new"),
                Resource("clock"),
            ],
            move |cx, _| {
                cx.set_pose([(id, Mat4::from_translation(Vec3::Z))])?;
                cx.set_component(id, "state", serde_json::json!(99))?;
                cx.set_component(id, "new", serde_json::Value::Null)?;
                cx.set_resource("clock", 99u32)
            },
        ));
        s.add_system(task(
            "invalid",
            &["modify"],
            &[],
            &[Component("new")],
            move |cx, _| {
                cx.set_component(id, "new", serde_json::json!(123))?;
                if read_violation {
                    cx.world()?;
                } else {
                    cx.set_resource("clock", 0u32)?;
                }
                Ok(())
            },
        ));
        let err = s.tick(&mut w, 0., &pool).unwrap_err().to_string();
        assert!(
            err.contains(if read_violation {
                "undeclared read"
            } else {
                "undeclared write"
            }),
            "{err}"
        );
        let mut verify = Schedule::default();
        verify.add_system(task(
            "check",
            &[],
            &[
                LocalTransform,
                Component("state"),
                Component("new"),
                Resource("clock"),
            ],
            &[],
            move |cx, _| {
                assert_eq!(cx.local(id)?, Some(Mat4::IDENTITY));
                assert_eq!(*cx.component::<u32>(id, "state")?.unwrap(), 17);
                assert!(cx.component_owners("new")?.is_empty());
                assert_eq!(cx.resource::<u32>("clock")?, Some(&12));
                Ok(())
            },
        ));
        verify.tick(&mut w, 0., &pool)?;
    }
    Ok(())
}

#[test]
fn opaque_plugin_cannot_waive_exclusion_with_an_incomplete_access_list() -> Result<()> {
    struct Opaque;
    impl System for Opaque {
        fn access(&self) -> SystemAccess {
            SystemAccess {
                name: "legacy",
                reads: &[],
                writes: &[],
            }
        }
        fn update(&self, _: &World, _: f32) -> Result<Edit> {
            Ok(Box::new(|_| {}))
        }
    }
    let mut s = Schedule::default();
    s.add_system(task("before", &[], &[], &[], |_, _| Ok(())));
    s.add_system(Arc::new(Opaque));
    s.add_system(task("after", &[], &[], &[], |_, _| Ok(())));
    let stages = s.stages().map(|(a, _)| a.to_vec()).collect::<Vec<_>>();
    assert_eq!(stages.len(), 3);
    assert_eq!(stages[1][0].writes, [SimulationData::World]);
    Ok(())
}

#[test]
fn conflicting_resource_types_are_reported_before_the_tick_commits() -> Result<()> {
    use SimulationData::Resource;
    let mut s = Schedule::default();
    s.add_system(task("seed", &[], &[], &[Resource("dt")], |cx, _| {
        cx.set_resource("dt", 1u32)
    }));
    s.add_system(task("bad", &[], &[], &[Resource("dt")], |cx, _| {
        cx.set_resource("dt", "incorrect")
    }));
    let pool = ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut w = Arc::new(World::default());
    assert!(
        s.tick(&mut w, 0., &pool)
            .unwrap_err()
            .to_string()
            .contains("type mismatch")
    );
    let mut inspect = Schedule::default();
    inspect.add_system(task("verify", &[], &[Resource("dt")], &[], |cx, _| {
        assert!(cx.resource::<u32>("dt")?.is_none());
        Ok(())
    }));
    inspect.tick(&mut w, 0., &pool)
}

#[test]
fn disjoint_pose_writers_run_concurrently_on_two_workers() -> Result<()> {
    use std::sync::{Condvar, Mutex};
    let mut world = World::default();
    let a = world.spawn(Default::default());
    let b = world.spawn(Default::default());
    let rendezvous = Arc::new((Mutex::new(0), Condvar::new()));
    let mut schedule = Schedule::default();
    for (key, id) in [("bones", a), ("shards", b)] {
        let gate = rendezvous.clone();
        schedule.add_system(scoped_task(
            key,
            &[],
            &[],
            &[LocalTransformsById],
            move |_| {
                Ok(TransformTargets {
                    reads: vec![],
                    writes: vec![id, id],
                })
            },
            move |cx, _| {
                let mut count = gate.0.lock().unwrap();
                *count += 1;
                gate.1.notify_all();
                let (count, _) = gate
                    .1
                    .wait_timeout_while(count, Duration::from_secs(3), |v| *v < 2)
                    .unwrap();
                anyhow::ensure!(*count == 2, "second pose writer never started");
                cx.set_pose([(id, Mat4::from_translation(Vec3::X))])
            },
        ));
    }
    let mut world = Arc::new(world);
    schedule.tick(
        &mut world,
        0.,
        &ThreadPoolBuilder::new().num_threads(2).build()?,
    )?;
    assert_eq!(schedule.stages().count(), 1);
    for id in [a, b] {
        assert_eq!(world.entities[&id].local.w_axis.x, 1.);
    }
    Ok(())
}

#[test]
fn scoped_conflicts_use_read_and_write_sets_separately() -> Result<()> {
    let world = World::default();
    // Include unordered duplicates to verify normalization before intersection.
    for (read_a, write_a, read_b, write_b, expected) in [
        (vec![4], vec![9], vec![4], vec![7], None),
        (
            vec![],
            vec![9, 4, 9],
            vec![7, 4],
            vec![],
            Some("write/read"),
        ),
        (vec![4], vec![9], vec![], vec![4], Some("read/write")),
        (vec![], vec![4], vec![], vec![4], Some("write/write")),
    ] {
        let mut schedule = Schedule::default();
        for (name, reads, writes) in [("a", read_a, write_a), ("b", read_b, write_b)] {
            schedule.add_system(scoped_task(
                name,
                &[],
                &[LocalTransformsById],
                &[LocalTransformsById],
                move |_| {
                    Ok(TransformTargets {
                        reads: reads.clone(),
                        writes: writes.clone(),
                    })
                },
                |_, _| Ok(()),
            ));
        }
        schedule.bind_transforms(&world)?;
        assert_eq!(
            schedule.stages().count(),
            1 + usize::from(expected.is_some())
        );
        if let Some(hazard) = expected {
            assert!(
                schedule
                    .describe()
                    .contains(&format!("{hazard} LocalTransform entity 4"))
            );
        }
    }
    for (ids, after, count) in [
        (vec![], &[][..], 1),
        (vec![5], &[][..], 2),
        (vec![], &["all"][..], 2),
    ] {
        let mut schedule = Schedule::default();
        schedule.add_system(task("all", &[], &[], &[LocalTransform], |_, _| Ok(())));
        schedule.add_system(scoped_task(
            "bounded",
            after,
            &[LocalTransformsById],
            &[],
            move |_| {
                Ok(TransformTargets {
                    reads: ids.clone(),
                    writes: vec![],
                })
            },
            |_, _| Ok(()),
        ));
        schedule.bind_transforms(&world)?;
        assert_eq!(schedule.stages().count(), count);
    }
    Ok(())
}

#[test]
fn changing_bindings_are_resolved_after_commit_inside_each_fixed_substep() -> Result<()> {
    const KEY: &str = "active joint";
    let mut world = World::default();
    let a = world.spawn(Default::default());
    let b = world.spawn(Default::default());
    let mut world = Arc::new(world);
    let mut schedule = Schedule::default();
    let choose = task(
        "switch",
        &[],
        &[Component(KEY)],
        &[Component(KEY)],
        move |cx, _| {
            let next = if cx.component::<Id>(a, KEY)?.is_some_and(|id| *id == b) {
                a
            } else {
                b
            };
            cx.set_typed_component(a, KEY, next)
        },
    );
    let actor = scoped_task(
        "actor",
        &[],
        &[Component(KEY), LocalTransformsById],
        &[LocalTransformsById],
        move |cx| {
            let id = *cx.component::<Id>(a, KEY)?.unwrap();
            Ok(TransformTargets {
                reads: vec![id],
                writes: vec![id],
            })
        },
        move |cx, _| {
            let id = *cx.component::<Id>(a, KEY)?.unwrap();
            cx.set_pose([(id, cx.local(id)?.unwrap() * Mat4::from_translation(Vec3::X))])
        },
    );
    let shard = scoped_task(
        "shard",
        &[],
        &[LocalTransformsById],
        &[LocalTransformsById],
        move |_| {
            Ok(TransformTargets {
                reads: vec![b],
                writes: vec![b],
            })
        },
        move |cx, _| {
            let x = cx.local(b)?.unwrap().w_axis.x;
            cx.set_pose([(b, Mat4::from_translation(Vec3::X * (10. * x)))])
        },
    );
    // First substep binds B (actor → shard); second binds A (actor and shard can overlap).
    schedule.fixed_stage(Duration::from_millis(125), 2, vec![choose, actor, shard])?;
    schedule.bind_transforms(&world)?;
    assert!(schedule.describe().contains("pending binding"));
    schedule.tick(
        &mut world,
        0.25,
        &ThreadPoolBuilder::new().num_threads(2).build()?,
    )?;
    assert_eq!(world.entities[&a].local.w_axis.x, 1.);
    assert_eq!(world.entities[&b].local.w_axis.x, 100.);
    assert_eq!(
        schedule
            .stages()
            .map(|(g, n)| (g.len(), n))
            .collect::<Vec<_>>(),
        [(1, 2), (2, 2)]
    );
    Ok(())
}

#[test]
fn pose_scope_errors_rollback_the_substep() -> Result<()> {
    let pool = ThreadPoolBuilder::new().num_threads(2).build()?;
    for violation in ["read", "write", "selector"] {
        let mut world = World::default();
        let a = world.spawn(Default::default());
        let b = world.spawn(Default::default());
        let mut schedule = Schedule::default();
        let seed = task("seed", &[], &[], &[Component("binding")], move |cx, _| {
            cx.set_component(a, "binding", b.into())
        });
        let invalid = scoped_task(
            "invalid",
            &[],
            &[Component("binding"), LocalTransformsById],
            &[LocalTransformsById],
            move |bindings| {
                if violation == "selector" {
                    bindings.component_owners("undeclared selector input")?;
                }
                Ok(TransformTargets {
                    reads: vec![a],
                    writes: vec![a],
                })
            },
            move |cx, _| {
                if violation == "read" {
                    cx.local(b)?;
                }
                cx.set_pose([
                    (a, Mat4::from_translation(Vec3::X)),
                    (b, Mat4::from_translation(Vec3::Y)),
                ])
            },
        );
        schedule.fixed_stage(Duration::from_millis(125), 1, vec![seed, invalid])?;
        let mut world = Arc::new(world);
        let err = schedule
            .tick(&mut world, 0.125, &pool)
            .unwrap_err()
            .to_string();
        assert!(err.contains("undeclared"), "{err}");
        if violation != "selector" {
            assert!(
                err.contains(&format!("{violation} of LocalTransform entity {b}")),
                "{err}"
            );
        }
        assert!(world.first_raw_component("binding").is_none());
        for id in [a, b] {
            assert_eq!(world.entities[&id].local, Mat4::IDENTITY);
        }
        assert!(schedule.stages().all(|(_, n)| n == 0));
    }
    Ok(())
}

#[test]
fn immutable_transform_sets_are_built_once_per_system_binding() -> Result<()> {
    use render_lab::engine::{BoundSystems, FixedDomain};

    struct BoundWrite {
        id: Id,
        evaluations: Arc<AtomicUsize>,
    }
    impl SimulationSystem for BoundWrite {
        fn access(&self) -> SystemAccess {
            SystemAccess {
                name: "bound writer",
                reads: &[],
                writes: &[LocalTransformsById],
            }
        }
        fn bound_transforms(&self) -> Option<TransformTargets> {
            self.evaluations.fetch_add(1, Ordering::Relaxed);
            Some(TransformTargets {
                reads: vec![],
                writes: vec![self.id, self.id],
            })
        }
        fn run(&self, cx: &mut SystemContext, _: f32) -> Result<()> {
            cx.set_pose([(self.id, Mat4::from_translation(Vec3::Y))])
        }
    }

    let evaluated = Arc::new(AtomicUsize::new(0));
    let mut systems = BoundSystems::default();
    let counts = evaluated.clone();
    systems.register(move |&id| BoundWrite {
        id,
        evaluations: counts.clone(),
    });
    let mut w = World::default();
    let first = w.spawn(Default::default());
    let next = w.spawn(Default::default());
    w.entities
        .get_mut(&first)
        .unwrap()
        .components
        .insert("bound".into(), true.into());
    let mut schedule = Schedule::default();
    let clock = FixedDomain {
        name: "test-rig",
        step: Duration::from_millis(125),
        max_steps: 4,
    };
    systems.install(&mut schedule, clock, &w, "bound", |w| {
        Ok((
            w.entities.component_owners("bound")[0],
            Box::new(|_| Ok(true)),
        ))
    })?;
    let mut w = Arc::new(w);
    let pool = ThreadPoolBuilder::new().num_threads(2).build()?;
    schedule.bind_transforms(&w)?;
    schedule.tick(&mut w, 0.5, &pool)?;
    assert_eq!(evaluated.load(Ordering::Relaxed), 1);
    assert_eq!(w.entities[&first].local.w_axis.y, 1.);
    assert_eq!(w.entities[&next].local.w_axis.y, 0.);
    let rows = &mut Arc::make_mut(&mut w).entities;
    rows.get_mut(&first).unwrap().components.remove("bound");
    rows.get_mut(&next)
        .unwrap()
        .components
        .insert("bound".into(), true.into());
    schedule.tick(&mut w, 0.5, &pool)?;
    assert_eq!(evaluated.load(Ordering::Relaxed), 2);
    assert_eq!(w.entities[&next].local.w_axis.y, 1.);
    Ok(())
}

#[test]
fn independent_named_clocks_keep_frame_parallelism_and_can_run_at_different_rates() -> Result<()> {
    use render_lab::engine::TickRate;
    use std::sync::{Condvar, Mutex};
    let gate = Arc::new((Mutex::new(0), Condvar::new()));
    let mut schedule = Schedule::default();
    for (name, component) in [("wind", "wind phase"), ("effects", "effect phase")] {
        let gate = gate.clone();
        schedule.add_frame_system(
            name,
            task(name, &[], &[], &[], move |_, dt| {
                if dt == 0.0 {
                    let (lock, ready) = &*gate;
                    let mut reached = lock.lock().unwrap();
                    *reached += 1;
                    ready.notify_all();
                    let (reached, _) = ready
                        .wait_timeout_while(reached, Duration::from_secs(2), |n| *n < 2)
                        .unwrap();
                    anyhow::ensure!(*reached == 2, "{component} did not overlap");
                }
                Ok(())
            }),
        )?;
    }
    let pool = ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut world = Arc::new(World::default());
    schedule.tick(&mut world, 0., &pool)?;
    schedule.set_tick_rate("wind", TickRate::Hertz(15))?;
    schedule.set_tick_rate("effects", TickRate::Hertz(30))?;
    for _ in 0..60 {
        schedule.tick(&mut world, 1. / 60., &pool)?;
    }
    assert_eq!(
        schedule.stages().map(|(_, n)| n).collect::<Vec<_>>(),
        [16, 31]
    );
    assert!(
        schedule
            .set_tick_rate("missing", TickRate::Default)
            .is_err()
    );
    assert!(schedule.set_tick_rate("wind", TickRate::Hertz(0)).is_err());
    schedule.set_tick_rate("wind", TickRate::Default)?;
    assert_eq!(schedule.fixed_seconds("wind"), None);
    assert!(schedule.describe_clocks().contains("effects · 30.00 Hz"));
    Ok(())
}

#[test]
fn following_clock_keeps_its_time_when_an_earlier_domain_fails() -> Result<()> {
    use render_lab::engine::FixedDomain;
    let mut world = Arc::new(World::default());
    let first = FixedDomain {
        name: "actor",
        step: Duration::from_millis(125),
        max_steps: 8,
    };
    let mut schedule = Schedule::default();
    schedule.add_fixed_system(first, Arc::new(FailSecond(AtomicUsize::new(0))))?;
    schedule.add_fixed_system(
        FixedDomain {
            name: "wind",
            ..first
        },
        task("wind", &[], &[], &[], |_, _| Ok(())),
    )?;
    let pool = ThreadPoolBuilder::new().num_threads(1).build()?;
    assert!(schedule.tick(&mut world, 0.375, &pool).is_err());
    schedule.tick(&mut world, 0., &pool)?;
    assert_eq!(
        schedule.stages().map(|(_, n)| n).collect::<Vec<_>>(),
        [3, 3]
    );
    Ok(())
}

#[test]
fn world_time_interleaves_rates_and_ties_are_dependency_ordered() -> Result<()> {
    use render_lab::engine::FixedDomain;
    use std::sync::Mutex;
    for cadence in [vec![0.125], vec![0.0625, 0.0625]] {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut schedule = Schedule::default();
        for (name, hz, after) in [("fast", 64., &[][..]), ("slow", 32., &["fast"][..])] {
            let log = log.clone();
            schedule.add_fixed_system(
                FixedDomain {
                    name,
                    step: Duration::from_secs_f64(1. / hz),
                    max_steps: 8,
                },
                task(name, after, &[], &[], move |cx, dt| {
                    assert_eq!(f64::from(dt), 1. / hz);
                    log.lock()
                        .unwrap()
                        .push(((cx.time_seconds() * 64.).round() as u32, name));
                    Ok(())
                }),
            )?;
        }
        let pool = ThreadPoolBuilder::new().num_threads(2).build()?;
        let mut world = Arc::new(World::default());
        for dt in cadence {
            schedule.tick(&mut world, dt, &pool)?;
        }
        assert_eq!(world.time_seconds(), 0.125);
        assert_eq!(
            *log.lock().unwrap(),
            [
                (1, "fast"),
                (2, "fast"),
                (2, "slow"),
                (3, "fast"),
                (4, "fast"),
                (4, "slow"),
                (5, "fast"),
                (6, "fast"),
                (6, "slow"),
                (7, "fast"),
                (8, "fast"),
                (8, "slow")
            ]
        );
    }
    Ok(())
}
