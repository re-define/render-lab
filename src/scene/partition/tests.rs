use super::*;
use crate::{engine::Schedule, experiments::movement_lab, movement::linear};
#[test]
fn preload_activate_simulate_unload_save_and_resume() -> Result<()> {
    let mut w = Arc::new(movement_lab::partitioned()?);
    let mut r = Runtime::new(&w)?.unwrap();
    let far = Vec3::new(0., 0., 12.);
    let start = std::time::Instant::now();
    loop {
        r.update(&mut w, &[far])?;
        if !r.loaded.is_empty() {
            break;
        }
        ensure!(start.elapsed().as_secs() < 4, "cell read timeout");
        std::thread::yield_now();
    }
    assert!(!w.entities.contains_key(&1000));
    assert!(r.active.is_empty());
    let mut extractor = crate::render::SceneExtractor::default();
    extractor.snapshot(&w, Default::default(), [64, 64], Default::default())?;
    r.update(&mut w, &[Vec3::ZERO])?;
    let rendered = extractor.snapshot(&w, Default::default(), [64, 64], Default::default())?;
    assert!(!rendered.scene.stats.full_rebuild);
    assert_eq!(rendered.scene.stats.instances_updated, 6);
    let authored = w.entities[&1000].local;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut sim = Schedule::default();
    linear::register(&mut sim)?;
    sim.tick(&mut w, 1. / 30., &pool)?;
    let moved = w.entities[&1000].local;
    let momentum = w.linear_body(1000)?.unwrap().momentum;
    assert_ne!(moved, authored);
    let dir = tempfile::tempdir()?;
    let save = dir.path().join("scene.json");
    w.save(&save)?;
    let live = World::load(&save)?;
    assert_eq!(live.entities[&1000].local, moved);
    assert_eq!(Runtime::new(&live)?.unwrap().active.len(), 1);
    r.update(&mut w, &[Vec3::splat(1000.)])?;
    assert!(!w.entities.contains_key(&1000));
    while !r.writing.is_empty() {
        ensure!(start.elapsed().as_secs() < 5, "cold checkpoint timeout");
        r.update(&mut w, &[Vec3::splat(1000.)])?;
        std::thread::yield_now();
    }
    assert!(matches!(
        w.partition.as_ref().unwrap().overrides.get(&1000),
        Some(Checkpoint::File(..))
    ));
    w.save(&save)?;
    let mut reloaded = Arc::new(World::load(&save)?);
    let mut r = Runtime::new(&reloaded)?.unwrap();
    while !reloaded.entities.contains_key(&1000) {
        ensure!(start.elapsed().as_secs() < 5, "restore timeout");
        r.update(&mut reloaded, &[Vec3::ZERO])?;
        std::thread::yield_now();
    }
    assert_eq!(reloaded.entities[&1000].local, moved);
    assert_eq!(reloaded.linear_body(1000)?.unwrap().momentum, momentum);
    Ok(())
}
fn until(
    world: &mut Arc<World>,
    runtime: &mut Runtime,
    sources: &[Vec3],
    condition: impl Fn(&World, &Runtime) -> bool,
) -> Result<()> {
    let started = std::time::Instant::now();
    while !condition(world, runtime) {
        runtime.update(world, sources)?;
        ensure!(
            started.elapsed().as_secs() < 6,
            "partition transition timed out"
        );
        std::thread::yield_now();
    }
    Ok(())
}
#[test]
fn adjacent_sources_share_versioned_asset_levels_and_wait_for_readiness() -> Result<()> {
    let mut w = movement_lab::partitioned()?;
    let cells = Arc::make_mut(&mut Arc::make_mut(w.partition.as_mut().unwrap()).cells);
    for (c, min) in cells.iter_mut().zip([0, 2, 0]) {
        c.assets.insert("bark.v2".into(), min);
    }
    let mut w = Arc::new(w);
    let mut r = Runtime::new(&w)?.unwrap();
    let both = [Vec3::ZERO, Vec3::X * 20.];
    until(&mut w, &mut r, &both, |w, r| {
        w.entities.contains_key(&1000) && r.loaded.contains_key(&1)
    })?;
    assert_eq!(r.asset_levels()["bark.v2"], 2);
    assert!(!w.entities.contains_key(&1016));
    let old = crate::render::Snapshot::new(&w, Default::default(), [32, 32], Default::default())?;
    old.report_asset_residency("bark.v2", 2);
    until(&mut w, &mut r, &both, |w, _| w.entities.contains_key(&1016))?;
    until(&mut w, &mut r, &[Vec3::ZERO], |w, r| {
        !w.entities.contains_key(&1016) && r.asset_levels()["bark.v2"] == 0
    })?;
    until(&mut w, &mut r, &[], |w, r| {
        !w.entities.contains_key(&1000) && r.asset_levels().is_empty()
    })?;
    assert_eq!(old.cell_asset_levels["bark.v2"], 2);
    until(&mut w, &mut r, &both, |_, r| r.loaded.contains_key(&1))?;
    old.report_asset_residency("bark.v2", 2);
    r.update(&mut w, &both)?;
    assert!(
        !w.entities.contains_key(&1016),
        "retired asset epoch acknowledged a new request"
    );
    r.set_asset_ready("bark.v2", 2);
    until(&mut w, &mut r, &both, |w, _| w.entities.contains_key(&1016))?;
    Ok(())
}
#[test]
fn large_authored_cell_loads_in_atomic_independent_batches_and_saves_between_ticks() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let rows:Vec<_>=(0..73).map(|i|serde_json::json!({"name":format!("actor {i}"),"local":Mat4::IDENTITY,"instance":null})).collect();
    std::fs::write(dir.path().join("region.json"), serde_json::to_vec(&rows)?)?;
    let cell = Cell {
        first: 8000,
        count: rows.len(),
        file: "region.json".into(),
        center: Vec3::ZERO,
        assets: Default::default(),
        save_state: true,
    };
    let mut world = Arc::new(World {
        partition: Some(Layout::new(dir.path().into(), vec![cell], 9., 14., 18.)?.into()),
        ..Default::default()
    });
    let mut r = Runtime::new(&world)?.unwrap();
    let mut total = 0;
    let mut active_ticks = 0;
    let start = std::time::Instant::now();
    while total < rows.len() {
        r.update(&mut world, &[Vec3::ZERO])?;
        let next = world.entities.len();
        assert!(next - total <= FRAME_ROWS);
        if next != total {
            active_ticks += 1;
            for batch in world.partition.as_ref().unwrap().cells.iter() {
                let alive = (batch.first..batch.first + batch.count as u64)
                    .filter(|id| world.entities.contains_key(id))
                    .count();
                assert!(alive == 0 || alive == batch.count);
            }
        }
        if total == 0 && next > 0 {
            let file = dir.path().join("partial.json");
            world.save(&file)?;
            world = Arc::new(World::load(&file)?);
            r = Runtime::new(&world)?.unwrap();
            assert_eq!(world.entities.len(), next);
        }
        total = next;
        ensure!(start.elapsed().as_secs() < 6);
        std::thread::yield_now();
    }
    assert!(active_ticks >= 3);
    while total > 0 {
        r.update(&mut world, &[])?;
        let next = world.entities.len();
        assert!(total - next <= FRAME_ROWS);
        total = next;
        ensure!(start.elapsed().as_secs() < 6);
        std::thread::yield_now();
    }
    Ok(())
}
#[test]
fn moving_actor_keeps_id_and_momentum_across_cells_and_cold_archive() -> Result<()> {
    let mut w = Arc::new(movement_lab::partitioned()?);
    let mut r = Runtime::new(&w)?.unwrap();
    let a = Vec3::ZERO;
    let b = Vec3::X * 20.;
    until(&mut w, &mut r, &[a], |w, _| w.entities.contains_key(&1000))?;
    let target = Mat4::from_translation(b + Vec3::Y);
    let incarnation = w.entities.incarnation(1000);
    let mut body = w.linear_body(1000)?.unwrap();
    body.momentum = Vec3::new(2., 3., 4.);
    Arc::make_mut(&mut w).set_linear_body(1000, body)?;
    Arc::make_mut(&mut w).set_local(1000, target)?;
    until(&mut w, &mut r, &[b], |w, r| {
        !w.entities.contains_key(&1001) && w.entities.contains_key(&1016) && r.writing.is_empty()
    })?;
    assert_eq!(w.entities.incarnation(1000), incarnation);
    assert_eq!(w.partition.as_ref().unwrap().roaming[&1000].cell, 1016);
    until(&mut w, &mut r, &[], |w, r| {
        !w.entities.contains_key(&1000) && r.writing.is_empty()
    })?;
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("roaming.json");
    w.save(&file)?;
    drop(r);
    w = Arc::new(World::load(&file)?);
    r = Runtime::new(&w)?.unwrap();
    until(&mut w, &mut r, &[a], |w, _| w.entities.contains_key(&1001))?;
    assert!(
        !w.entities.contains_key(&1000),
        "birth cell resurrected migrated actor"
    );
    until(&mut w, &mut r, &[a, b], |w, _| {
        w.entities.contains_key(&1000) && w.entities.contains_key(&1016)
    })?;
    assert_eq!(w.entities[&1000].local, target);
    assert_eq!(w.linear_body(1000)?.unwrap().momentum, body.momentum);
    assert_eq!(w.entities.range(1000..1022).count(), 12);
    Ok(())
}

#[test]
fn repeated_saves_only_rehash_dirty_cells() -> Result<()> {
    let mut w = Arc::new(movement_lab::partitioned()?);
    let mut r = Runtime::new(&w)?.unwrap();
    let dir = tempfile::tempdir()?;
    let save = dir.path().join("scene.json");
    w.save(&save)?;
    let count = |w: &World| w.partition.as_ref().unwrap().exports.lock().unwrap().writes;
    assert_eq!(count(&w), 3);
    w.save(&save)?;
    assert_eq!(count(&w), 3);
    until(&mut w, &mut r, &[Vec3::ZERO], |w, _| {
        w.entities.contains_key(&1000)
    })?;
    Arc::make_mut(&mut w).set_local(1000, Mat4::from_translation(Vec3::Y))?;
    until(&mut w, &mut r, &[], |w, r| {
        !w.entities.contains_key(&1000) && r.writing.is_empty()
    })?;
    w.save(&save)?;
    assert_eq!(count(&w), 4);
    w.save(&save)?;
    assert_eq!(count(&w), 4);
    Ok(())
}
/// Standalone World update costs. Advance one synthetic source per call and yield between ticks
/// so the bounded file worker can progress independently from the timing interval.
#[test]
#[ignore = "world-stream disk and main-thread microbenchmark"]
fn cold_revisit_and_teleport_world_cpu_cost() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actors = 768usize;
    let mut cells = Vec::new();
    for i in 0..3 {
        let center = Vec3::X * (30. * i as f32);
        let rows:Vec<_>=(0..actors).map(|n|serde_json::json!({
            "name":format!("cell-{i}-{n}"),"local":Mat4::from_translation(center),"instance":null
        })).collect();
        let file = PathBuf::from(format!("region-{i}.json"));
        std::fs::write(dir.path().join(&file), serde_json::to_vec(&rows)?)?;
        cells.push(Cell {
            first: 1000 + 1000 * i,
            count: actors,
            center,
            file,
            assets: Default::default(),
            save_state: true,
        });
    }
    let mut w = Arc::new(World {
        partition: Some(Layout::new(dir.path().into(), cells, 9., 14., 18.)?.into()),
        ..Default::default()
    });
    let mut runtime = Runtime::new(&w)?.unwrap();
    for (label, index) in [("cold", 0), ("forward", 1), ("revisit", 0), ("teleport", 2)] {
        let mut times = Vec::<f64>::new();
        let first = 1000 + 1000 * index;
        let started = std::time::Instant::now();
        loop {
            let tick = std::time::Instant::now();
            runtime.update(&mut w, &[Vec3::X * (30. * index as f32)])?;
            times.push(tick.elapsed().as_secs_f64() * 1000.);
            if w.entities.len() == actors
                && w.entities.range(first..first + actors as u64).count() == actors
                && runtime.writing.is_empty()
            {
                break;
            }
            ensure!(started.elapsed().as_secs() < 12);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        times.sort_by(f64::total_cmp);
        let q = |p: usize| times[(times.len() * p).div_ceil(100) - 1];
        println!(
            "WORLD_STREAM phase={label} actors={actors} updates={} P50_ms={:.4} P95_ms={:.4} P99_ms={:.4} max_ms={:.4}",
            times.len(),
            q(50usize),
            q(95),
            q(99),
            times[times.len() - 1]
        );
    }
    Ok(())
}
