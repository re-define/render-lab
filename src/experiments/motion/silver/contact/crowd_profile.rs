//! Reproducible extraction workload. Shared asset entities, independent bodies, skins and pose state.
use super::*;
use crate::engine::{
    SimulationSystem, SystemAccess, SystemContext, SystemOrder, TransformBindings, TransformTargets,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

struct PopulationStage<T>(Vec<T>);
impl<T: SimulationSystem> SimulationSystem for PopulationStage<T> {
    fn access(&self) -> SystemAccess {
        self.0[0].access()
    }
    fn order(&self) -> SystemOrder {
        self.0[0].order()
    }
    fn transform_targets(&self, b: &TransformBindings) -> Result<TransformTargets> {
        let mut all = TransformTargets::default();
        for s in &self.0 {
            let t = s.transform_targets(b)?;
            all.reads.extend(t.reads);
            all.writes.extend(t.writes);
        }
        Ok(all)
    }
    fn run(&self, cx: &mut SystemContext, dt: f32) -> Result<()> {
        cx.for_each_parallel(&self.0, |system, cx| system.run(cx, dt))
    }
}
fn population(n: usize, pool: &rayon::ThreadPool) -> Result<(Arc<World>, Schedule)> {
    let mut initial = Arc::new(scene()?);
    let mut schedule = Schedule::default();
    install(&initial, &mut schedule)?;
    let actor = super::actor(&initial).unwrap();
    let r = Rig::bind(&initial, actor, true)?;
    let action_rig = traversal::ActionRig::bind(&initial, actor, &r)?;
    let prototype = initial
        .typed_component::<Contact>(actor, SETTING)?
        .unwrap()
        .into_owned();
    let f = prototype.course.unwrap();
    let supports = [
        prototype.floor,
        prototype.plate,
        f.landing,
        f.station,
        f.ramp,
        f.service.unwrap(),
    ];
    let mut nodes = BTreeSet::new();
    for (&id, ent) in &initial.entities {
        let mut at = Some(id);
        while let Some(k) = at {
            if k == actor {
                nodes.insert(id);
                break;
            }
            at = initial.entities[&k].parent;
        }
        if ent.skin.is_some() || ent.skin_binding.is_some() {
            nodes.insert(id);
        }
    }
    nodes.extend(supports);
    nodes.extend([f.rail, f.gate, prototype.target]);
    // Stagger up to twenty states sampled from the real course, including vault and duck.
    let phases = n.min(20);
    let mut seeds = vec![initial.clone()];
    for _ in 1..phases {
        for _ in 0..24 {
            schedule.tick(&mut initial, 1. / 60., pool)?;
        }
        seeds.push(initial.clone());
    }
    let mut w = (*seeds[0]).clone();
    let mut intents = Vec::new();
    let mut dynamics = Vec::new();
    let mut feedback = Vec::new();
    // Offset the rendered lanes in world space. Each collider and actor simulate in the same lane-local frame.
    let cols = (n as f32 * 1.8).sqrt().ceil() as usize;
    let rows = n.div_ceil(cols);
    for i in 0..n {
        let seed = &seeds[i % phases];
        let mut ids = BTreeMap::new();
        for &k in &nodes {
            let dst = if i == 0 {
                k
            } else {
                w.spawn(Entity::default())
            };
            ids.insert(k, dst);
        }
        let map = |id| ids[&id];
        let shift = Vec3::new(
            2.7 * ((i % cols) as f32 - (cols - 1) as f32 * 0.5),
            0.,
            9.8 * ((i / cols) as f32 - (rows - 1) as f32 * 0.5),
        );
        let lane = w.spawn(Entity {
            local: Mat4::from_translation(shift),
            ..Default::default()
        });
        for &k in &nodes {
            let mut row = seed.entities[&k].clone();
            row.parent = Some(
                row.parent
                    .map_or(lane, |v| ids.get(&v).copied().unwrap_or(v)),
            );
            if let Some(s) = &mut row.skin {
                for j in &mut s.joints {
                    *j = map(*j);
                }
            }
            row.skin_binding = row.skin_binding.map(map);
            // Other lanes share asset data and carry independent, invisible collision fixtures.
            if i != 0 && (supports.contains(&k) || [f.rail, f.gate, prototype.target].contains(&k))
            {
                row.instance = None;
            }
            w.entities.insert(map(k), row);
        }
        let mut p = player(seed, actor)?.into_owned();
        p.bones.iter_mut().for_each(|b| *b = map(*b));
        let mut c = seed
            .typed_component::<Contact>(actor, SETTING)?
            .unwrap()
            .into_owned();
        let mut b = seed
            .typed_component::<dynamics::Body>(actor, dynamics::BODY)?
            .unwrap()
            .into_owned();
        b.support = b.support.map(map);
        for foot in &mut c.feet {
            foot.anchor = foot.anchor.map(|(k, p)| (map(k), p));
        }
        c.floor = map(c.floor);
        c.plate = map(c.plate);
        c.target = map(c.target);
        c.course = Some(course::Fixtures {
            landing: map(f.landing),
            station: map(f.station),
            ramp: map(f.ramp),
            service: f.service.map(map),
            rail: map(f.rail),
            gate: map(f.gate),
        });
        w.set_typed_component(map(actor), PLAYER, p);
        w.set_typed_component(map(actor), SETTING, c);
        w.set_typed_component(map(actor), dynamics::BODY, b);
        let mut rig = r.clone();
        rig.ids.iter_mut().for_each(|k| *k = map(*k));
        let stage = Arc::new(Stage {
            actor: map(actor),
            rig,
            action_rig: Some(action_rig.clone()),
            supports: supports.iter().copied().map(map).collect(),
            rail: Some(map(f.rail)),
            plate: map(prototype.plate),
            target: map(prototype.target),
        });
        intents.push(stage::Prepare(stage.clone()));
        dynamics.push(dynamics::Integrate(stage.clone()));
        feedback.push(stage::Feedback(stage));
    }
    w.validate()?;
    let skinned = w
        .entities
        .values()
        .filter(|e| e.instance.is_some() && e.skin_binding.is_some())
        .count();
    eprintln!(
        "population={n}, skinned instances={skinned}, entities={}, players={}",
        w.entities.len(),
        w.entities.component_owners(PLAYER).len()
    );
    anyhow::ensure!(
        skinned > 0
            && w.entities.component_owners(PLAYER).len() == n
            && w.entities.component_owners(dynamics::BODY).len() == n
    );
    let mut schedule = Schedule::default();
    let mut matcher = SilverMatcher::load()?;
    matcher.after = &[PREPARE];
    schedule.fixed_stage(
        Duration::from_secs_f64(1. / 60.),
        8,
        vec![
            Arc::new(PopulationStage(intents)),
            Arc::new(matcher),
            Arc::new(PopulationStage(dynamics)),
            Arc::new(PopulationStage(feedback)),
        ],
    )?;
    Ok((Arc::new(w), schedule))
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
                    assert_eq!((a.id, a.model, &a.palette), (b.id, b.model, &b.palette));
                }
            }
            times.push(ms);
            sim_times.push(sim_ms);
        }
    }
    times.sort_by(f64::total_cmp);
    sim_times.sort_by(f64::total_cmp);
    println!(
        "NPCs {count}; 96 frames, 6 fixed substeps; simulation P50/P95 {:.2}/{:.2} ms, extraction P50/P95 {:.2}/{:.2} ms",
        sim_times[48], sim_times[90], times[48], times[90]
    );
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
                for (key, ms) in [
                    ("simulation", p.main_ms),
                    ("extraction", p.extract_ms),
                    ("render build+compile", p.build_ms + p.compile_ms),
                    ("RHI elapsed", p.rhi_ms),
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
    for (key, mut values) in series {
        values.sort_by(f64::total_cmp);
        println!(
            "{key}: samples {} P50 {:.2} P95 {:.2} ms",
            values.len(),
            values[values.len() / 2],
            values[((values.len() - 1) as f64 * 0.95).round() as usize]
        );
    }
    Ok(())
}
