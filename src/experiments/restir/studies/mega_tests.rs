use super::*;
use crate::{console::Console, engine::Engine, rdg::*, render::SceneExtractor, rhi::Backend};
use std::sync::Arc;
fn render(
    engine: &Engine,
    extractor: &mut SceneExtractor,
    gpu: &mut Backend,
    probe: bool,
) -> Result<crate::rhi::Frame> {
    let mut view = extractor.snapshot(
        &engine.world,
        engine.camera,
        [320, 200],
        engine.materials.clone(),
    )?;
    view.buffer_atomic64 = gpu.buffer_atomic64();
    let mut g = engine.technique.build(&view)?;
    if probe {
        let cluster = g.products.cluster_shadow.unwrap();
        let by_name =
            |name: &str| Resource(g.resources.iter().position(|(n, _)| n == name).unwrap());
        let ids = by_name("Visibility IDs");

        let mut reconstruct = g
            .passes
            .iter()
            .find(|p| p.name == "Reconstruct visible surfaces")
            .unwrap()
            .op
            .program()
            .unwrap()
            .clone();
        let surfaces =
            crate::render::rays::buffer(&mut g, "Probe positions and barycentrics", 320 * 200 * 32);
        reconstruct.shader = Shader::Compute(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/mega_barycentric.comp.spv"
        )));
        reconstruct.bindings[2].0 = surfaces;
        g.pass(
            "Classify subpixel edges",
            Op::Compute {
                program: reconstruct,
                groups: [40, 25, 1],
            },
        );
        let raw = crate::render::rays::buffer(&mut g, "Raster triangle IDs", 320 * 200 * 8);
        g.pass(
            "Read raster triangle IDs",
            Op::ReadPixels {
                image: ids,
                buffer: raw,
            },
        );
        let output = g.input("Primary visibility check", vec![0; 16]);
        use crate::render::rays::{RayHits, native_scene};
        let (tlas, blases, _) =
            native_scene(&mut g, &[], Some(cluster.blas), RayHits::InstanceGeometry)?;
        use Access::{RayRead as R, RayWrite as W};
        let extent = g.extent;
        g.pass(
            "Verify exact raster triangle in hardware",
            Op::TraceRays {
                extent,
                tlas,
                blases,
                program: Dispatch {
                    shader: Shader::ClusterShadows(
                        include_bytes!(concat!(env!("OUT_DIR"), "/mega_probe.rgen.spv")),
                        include_bytes!(concat!(env!("OUT_DIR"), "/mega_shadow.rmiss.spv")),
                        include_bytes!(concat!(env!("OUT_DIR"), "/mega_probe.rahit.spv")),
                    ),
                    bindings: vec![
                        (raw, R),
                        (surfaces, R),
                        (cluster.metadata, R),
                        (cluster.view, R),
                        (output, W),
                    ],
                    push: vec![],
                },
            },
        );
        g.pass(
            "Read primary visibility check",
            Op::Counters {
                buffer: output,
                first: 0,
                labels: vec![
                    "Raster triangles sampled",
                    "Matching hardware triangles",
                    "Interior raster samples",
                    "Matching interior samples",
                ],
            },
        );
    }
    gpu.execute(g.compile()?, &engine.tasks)
}
#[test]
#[ignore = "requires VK_NV_cluster_acceleration_structure"]
fn mega_matches_raster_triangle_ids_positions_and_lod_coverage_in_wind() -> Result<()> {
    let mut world = forest_study()?;
    world
        .entities
        .retain(|_, e| e.instance.is_none() || e.name == "Forest tree" || e.name == "Grass tuft");
    let mut engine = crate::experiments::open(world)?;
    let mut console = Console::default();
    for cmd in [
        "vegetation shadows mega",
        "restir gi direct",
        "vegetation wind 2",
    ] {
        console.execute(&mut engine, &[], cmd)?;
    }
    let mut gpu = Backend::open()?;
    let mut extractor = SceneExtractor::default();
    for (step, (time, pixels)) in [(0., 1.45), (0., 1.45), (1.7, 1.45), (1.7, 3.)]
        .into_iter()
        .enumerate()
    {
        for cmd in [
            format!("vegetation time {time}"),
            format!("vegetation pixels {pixels}"),
        ] {
            console.execute(&mut engine, &[], &cmd)?;
        }
        let f = render(&engine, &mut extractor, &mut gpu, true)?;
        let (n, k) = (
            f.counters["Raster triangles sampled"],
            f.counters["Matching hardware triangles"],
        );
        eprintln!("time={time}, LOD={pixels}, exact raster triangle and position in CLAS {k}/{n}");
        let (inside, exact) = (
            f.counters["Interior raster samples"],
            f.counters["Matching interior samples"],
        );
        eprintln!(
            "Positive barycentrics: {exact}/{inside}, raster-only fringe: {}/{}",
            k - exact,
            n - inside
        );
        if step == 1 {
            assert_eq!(
                f.counters["Reused CLAS"],
                f.counters["CLAS including empty sentinel"] - 1
            );
        }
        assert!(n > 3000 && inside > 3000 && exact * 1000 >= 999 * inside);
        assert!(
            f.counters["Transition shadow clusters"] > 0
                && f.counters["Offscreen shadow clusters"] > 0
        );
    }
    console.execute(&mut engine, &[], "restir gi multi")?;
    let gi = render(&engine, &mut extractor, &mut gpu, false)?;
    assert!(
        gi.passes
            .iter()
            .any(|p| p.name == "GI-1.2 trace hardware cluster probe paths")
    );
    assert!(
        !gi.passes
            .iter()
            .any(|p| p.name == "Gather shared ray triangles" || p.name == "Refit ray BVH level")
    );
    Ok(())
}

#[test]
#[ignore = "requires VK_NV_cluster_acceleration_structure"]
fn stable_clusters_reuse_native_storage_until_their_geometry_changes() -> Result<()> {
    let mut engine = crate::experiments::open(forest_study()?)?;
    let mut gpu = Backend::open()?;
    let mut extractor = SceneExtractor::default();
    let mut console = Console::default();
    for cmd in [
        "vegetation shadows mega",
        "restir gi direct",
        "vegetation wind 0",
        "restir reference",
        "restir denoise off",
    ] {
        console.execute(&mut engine, &[], cmd)?;
    }
    let cold = render(&engine, &mut extractor, &mut gpu, false)?;
    let count = cold.counters["CLAS including empty sentinel"] - 1;
    assert!(count > 1000);
    assert_eq!(cold.counters["New CLAS"], count);
    let warm = render(&engine, &mut extractor, &mut gpu, false)?;
    assert_eq!(warm.counters["Reused CLAS"], count);
    let difference = |a: &crate::rhi::Frame, b: &crate::rhi::Frame| {
        let mut pixels = 0;
        let mut steps = 0u64;
        for (p, q) in a.pixels.chunks_exact(4).zip(b.pixels.chunks_exact(4)) {
            pixels += usize::from(p != q);
            steps += (0..3).map(|i| u64::from(p[i].abs_diff(q[i]))).sum::<u64>();
        }
        (pixels, steps)
    };
    let fixed_difference = difference(&warm, &cold);
    let tree = engine
        .world
        .entities
        .iter()
        .find(|(_, e)| e.name == "Forest tree")
        .unwrap();
    let (id, model) = (*tree.0, tree.1.local);
    Arc::make_mut(&mut engine.world).set_local(id, Mat4::from_translation(Vec3::X) * model)?;
    let moved = render(&engine, &mut extractor, &mut gpu, false)?;
    assert!(moved.counters["Transform invalidated CLAS"] > 0);
    assert!(moved.counters["Reused CLAS"] > 0);
    for cmd in ["vegetation wind 1", "vegetation time 1.5"] {
        console.execute(&mut engine, &[], cmd)?;
    }
    render(&engine, &mut extractor, &mut gpu, false)?;
    console.execute(&mut engine, &[], "vegetation time 2")?;
    let windy = render(&engine, &mut extractor, &mut gpu, false)?;
    assert!(windy.counters["Pose invalidated CLAS"] > 0);
    console.execute(&mut engine, &[], "vegetation cache off")?;
    let forced = render(&engine, &mut extractor, &mut gpu, false)?;
    assert_eq!(
        forced.counters["New CLAS"],
        forced.counters["CLAS including empty sentinel"] - 1
    );
    assert_eq!(forced.counters["Reused CLAS"], 0);
    let forced_again = render(&engine, &mut extractor, &mut gpu, false)?;
    let same_difference = difference(&forced, &forced_again);
    let cached_difference = difference(&windy, &forced);
    eprintln!(
        "64000 foreground/background pixels (changed, sum of RGB steps): cold/warm {fixed_difference:?}, wind cached/rebuild {cached_difference:?}, wind rebuild/rebuild {same_difference:?}"
    );
    // Raster order can choose a different primitive for exactly overlapping foliage.
    // Bound this whole-image measure; the isolated receiver below checks every byte.
    for (pixels, steps) in [fixed_difference, cached_difference, same_difference] {
        assert!(pixels <= 32 && steps <= 4096);
    }
    console.execute(&mut engine, &[], "vegetation cache on")?;
    let mut fresh_extractor = SceneExtractor::default();
    let fresh = render(&engine, &mut fresh_extractor, &mut gpu, false)?;
    assert_eq!(
        fresh.counters["New CLAS"],
        fresh.counters["CLAS including empty sentinel"] - 1
    );
    assert_eq!(fresh.counters["Reused CLAS"], 0);
    Ok(())
}

#[test]
#[ignore = "requires VK_NV_cluster_acceleration_structure"]
fn mega_offscreen_wind_casts_into_a_visible_ground_receiver() -> Result<()> {
    let mut world = forest_study()?;
    let tree = world
        .entities
        .values()
        .find(|e| e.name == "Forest tree")
        .unwrap()
        .local;
    world.entities.retain(|_, e| {
        e.instance.is_none()
            || (e.name != "Grass tuft" && (e.name != "Forest tree" || e.local == tree))
    });
    let parts: Vec<_> = world
        .entities
        .iter()
        .filter(|(_, e)| e.name == "Forest tree")
        .map(|(&id, _)| id)
        .collect();
    for &id in &parts {
        world.set_local(
            id,
            Mat4::from_scale_rotation_translation(
                Vec3::splat(0.47),
                glam::Quat::IDENTITY,
                Vec3::new(-2.22, 0., 1.65),
            ),
        )?;
    }
    for e in world.entities.values_mut() {
        if let Some(light) = &mut e.light {
            light.enabled = e.name == "Late afternoon sun";
            light.angular_radius_degrees = 0.;
            if light.enabled {
                e.local = Light::aiming(Vec3::new(3.6, -6.4, -3.5))?;
            }
        }
    }
    let mut engine = crate::experiments::open(world)?;
    engine.camera = Camera {
        target: Vec3::ZERO,
        distance: 1.2,
        yaw: 0.,
        pitch: 1.55,
    };
    let mut console = Console::default();
    for cmd in [
        "vegetation shadows mega",
        "restir gi direct",
        "restir reference",
        "restir denoise off",
        "vegetation wind 2",
    ] {
        console.execute(&mut engine, &[], cmd)?;
    }
    let mut gpu = Backend::open()?;
    let mut extractor = SceneExtractor::default();
    let mut images = vec![];
    for (time, hide) in [(0., false), (1.7, false), (1.7, true)] {
        console.execute(&mut engine, &[], &format!("vegetation time {time}"))?;
        if hide {
            for id in &parts {
                Arc::make_mut(&mut engine.world).entities.remove(id);
            }
        }
        let f = render(&engine, &mut extractor, &mut gpu, false)?;
        if !hide {
            assert!(f.counters["Offscreen shadow clusters"] > 0);
            assert_eq!(f.counters["GI foliage"] + f.counters["GI bark"], 0);
        }
        if !hide && time == 1.7 {
            let warm = render(&engine, &mut extractor, &mut gpu, false)?;
            assert_eq!(
                warm.counters["Reused CLAS"],
                warm.counters["CLAS including empty sentinel"] - 1
            );
            console.execute(&mut engine, &[], "vegetation cache off")?;
            let forced = render(&engine, &mut extractor, &mut gpu, false)?;
            let forced_again = render(&engine, &mut extractor, &mut gpu, false)?;
            assert!(
                forced.pixels == forced_again.pixels,
                "isolated shadow reference must be deterministic"
            );
            assert!(
                warm.pixels == forced.pixels,
                "reused CLAS changed an offscreen caster shadow pixel"
            );
        }
        images.push(f.pixels);
    }
    let diff = |a: usize, b: usize| {
        images[a]
            .chunks_exact(4)
            .zip(images[b].chunks_exact(4))
            .filter(|(p, q)| p[1].abs_diff(q[1]) > 3)
            .count()
    };
    eprintln!(
        "Offscreen shadows: wind changed {} pixels, removing the tree changed {}",
        diff(0, 1),
        diff(1, 2)
    );
    assert!(diff(0, 1) > 5 && diff(1, 2) > 100);
    Ok(())
}
