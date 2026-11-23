use super::*;
use crate::{
    console::Console,
    experiments,
    rhi::{Backend, Frame},
};

#[test]
fn closed_fracture_cells_partition_the_crystal_and_settle_above_the_platform() -> Result<()> {
    let mut world = destruction()?;
    let volume = |mesh: &Mesh| {
        mesh.indices
            .chunks_exact(3)
            .map(|tri| {
                let p = |k: usize| Vec3::from(mesh.positions[tri[k] as usize]);
                p(0).dot(p(1).cross(p(2))) / 6.
            })
            .sum::<f32>()
    };
    let mut total = 0.;
    let mut intact = 0.;
    for entity in world.entities.values() {
        if let Some(mesh) = &entity.mesh {
            if entity.name == "Convex fracture cell" {
                assert!(mesh.uv.iter().any(|uv| uv[0] == 1.));
                let v = volume(mesh);
                assert!(v > 0.0);
                total += v;
            } else if entity.name == "Intact crystal" {
                intact = volume(mesh);
            }
        }
    }
    assert!((total - intact).abs() < 0.0001 && intact > 1.);
    let pieces = fracture::pieces(&world)?;
    assert_eq!(pieces.len(), 36);
    for piece in &pieces {
        for time in [0., 1.14, 1.97, 4.9] {
            let (id, model) = piece.pose(time);
            let instance = world.entities[&id].instance.as_ref().unwrap();
            let mesh = world.entities[&instance.mesh].mesh.as_ref().unwrap();
            assert!(
                mesh.positions
                    .iter()
                    .all(|&p| model.transform_point3(p.into()).y >= 0.12 - 0.0001)
            );
            world.set_local(id, model)?;
        }
        assert_eq!(piece.pose(0.).1, piece.pose(5.4).1);
    }
    Ok(())
}
#[test]
#[ignore = "requires Vulkan compute"]
fn water_parcels_cross_every_riser_and_reproject_the_same_stream() -> Result<()> {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut gpu = Backend::open()?;
    let (lanes, rows) = (water::LANES, water::ROWS);
    let mut read = |rewind: f32| -> Result<Vec<[f32; 4]>> {
        let mut g = FrameGraph::new([lanes * 4, rows]);
        let parcels = water::parcels(
            &mut g,
            1.17,
            rewind,
            (rewind > 0.).then_some(Mat4::IDENTITY),
        );
        g.set_output(parcels);
        let frame = gpu.execute(g.compile()?, &pool)?;
        Ok(frame
            .pixels
            .chunks_exact(16)
            .map(bytemuck::pod_read_unaligned)
            .collect())
    };
    let points = read(0.)?;
    let previous = read(1. / 30.)?;
    let mid: Vec<_> = points
        .chunks_exact(lanes as usize)
        .map(|row| row[lanes as usize / 2])
        .collect();
    assert!(mid.first().unwrap()[1] > 2.65 && mid.last().unwrap()[1] < 0.26);
    assert!(points.iter().flatten().all(|x| x.is_finite()));
    for r in 0..5 {
        let lip = -4. + (r + 1) as f32 * 1.38;
        let landing = mid
            .iter()
            .find(|p| p[2] > lip + 0.62 && p[3] > 0.12)
            .unwrap();
        assert!(landing[2] < lip + 1.05, "missing impact below lip {r}");
    }
    let i = (lanes * (rows / 2) + lanes / 2) as usize;
    assert!((0.055..0.08).contains(&(points[i][2] - previous[i][2])));
    assert_eq!(points, read(0.)?);
    Ok(())
}
#[test]
#[ignore = "requires Vulkan mesh shaders"]
fn dissolve_updates_visibility_and_restores_after_reload_and_restart() -> Result<()> {
    let mut engine = experiments::open(destruction()?)?;
    let mut console = Console::default();
    let mut draw = |engine: &mut Engine, command: &str| -> Result<Frame> {
        console.execute(engine, &[], command)?;
        ensure!(engine.tick(0., [360, 225])?, "render queue full");
        let start = std::time::Instant::now();
        loop {
            if let Some(result) = engine.take_frame() {
                return result.map_err(anyhow::Error::msg);
            }
            ensure!(start.elapsed().as_secs() < 30, "frame timed out");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    };
    let intact = draw(&mut engine, "effect restart")?;
    let mid = draw(&mut engine, "effect time 2.2")?;
    assert!(mid.counters[DISSOLVE] > 0 && mid.counters[DISSOLVE] < intact.counters[DISSOLVE]);
    assert!(mid.counters[ORE] > 0 && mid.pixels != intact.pixels);
    let saved: World = serde_json::from_slice(&serde_json::to_vec(&*engine.world)?)?;
    experiments::replace(&mut engine, saved)?;
    let end = draw(&mut engine, "effect time 4.7")?;
    assert_eq!(end.counters[DISSOLVE], 0);
    assert!(end.counters[ORE] > 0);
    let reset = draw(&mut engine, "effect restart")?;
    assert_eq!(intact.counters, reset.counters);
    assert_eq!(intact.pixels, reset.pixels);
    Ok(())
}

#[test]
#[ignore = "requires Vulkan ray tracing"]
fn translated_water_reaches_both_ray_backends_at_each_simulation_time() -> Result<()> {
    let mut world = water()?;
    let id = *world
        .entities
        .iter()
        .find(|(_, e)| e.name == "Cascade")
        .unwrap()
        .0;
    let model = Mat4::from_translation(Vec3::new(0.45, 0.20, -0.12));
    world.set_local(id, model)?;
    let mut engine = experiments::open(world)?;
    let mut extractor = crate::render::SceneExtractor::default();
    let mut console = Console::default();
    let mut gpu = Backend::open()?;
    let mut images = vec![];
    for time in ["0", "1.32"] {
        console.execute(&mut engine, &[], &format!("effect time {time}"))?;
        for mode in ["hardware", "software"] {
            console.execute(&mut engine, &[], &format!("rays {mode}"))?;
            let view = extractor.snapshot(
                &engine.world,
                engine.camera,
                [288, 180],
                engine.materials.clone(),
            )?;
            let graph = engine.technique.build(&view)?;
            let surface = graph
                .products
                .ray_surfaces
                .iter()
                .find(|s| s.dynamic)
                .context("missing water")?
                .geometry;
            assert_eq!(
                graph.resources[surface.vertices.0].0,
                "Cascade surface vertices"
            );
            assert_eq!(surface.model, model);
            images.push(gpu.execute(graph.compile()?, &engine.tasks)?.pixels);
        }
    }
    let different = |i: usize, j: usize| {
        images[i]
            .chunks_exact(4)
            .zip(images[j].chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count()
    };
    assert!(
        different(0, 1) < 52 && different(2, 3) < 52,
        "ray backends disagree on the cascade"
    );
    assert!(
        different(0, 2) > 35,
        "advancing the water simulation left ray hits unchanged"
    );
    Ok(())
}
