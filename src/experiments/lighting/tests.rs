use super::*;
use crate::{
    rdg::{Access, Op},
    rhi::Backend,
};

fn snapshot(kind: Kind) -> Result<Snapshot> {
    let world = scene(kind)?;
    // Keep the same settings and geometry when opening a saved scene.
    let world: World = serde_json::from_slice(&serde_json::to_vec(&world)?)?;
    world.validate()?;
    let mut materials = MaterialPrograms::default();
    let camera = configure(&world, &mut materials)?;
    Snapshot::new(&world, camera, [321, 199], Arc::new(materials))
}

#[test]
fn experiments_restore_graphs_and_declare_lighting_dependencies() -> Result<()> {
    for kind in [Kind::Gi, Kind::Shadow, Kind::Rt] {
        let snapshot = snapshot(kind)?;
        let plan = Lighting::default().build(&snapshot)?.compile()?;
        let step = |prefix| {
            plan.steps()
                .iter()
                .find(|step| plan.graph().passes[step.index].name.starts_with(prefix))
                .unwrap()
        };
        match kind {
            Kind::Gi => {
                let direct = step("Direct light:");
                let indirect = step("GI:");
                let upsample = step("Bilateral");
                assert!(indirect.parents.contains(&direct.index));
                assert!(upsample.parents.contains(&indirect.index));
                assert!(
                    step("Shade Lighting experiment")
                        .parents
                        .contains(&upsample.index)
                );
            }
            Kind::Shadow => {
                assert!(
                    step("Direct light:")
                        .parents
                        .contains(&step("Rasterize light-space").index)
                );
            }
            Kind::Rt => {
                assert!(
                    step("Build scene TLAS")
                        .parents
                        .contains(&step("Build triangle BLAS").index)
                );
                let trace = step("Trace primary");
                assert!(trace.parents.contains(&step("Build scene TLAS").index));
                assert!(
                    trace
                        .barriers
                        .iter()
                        .filter(|barrier| barrier.to == Access::RayAcceleration)
                        .count()
                        >= 2
                );
                assert!(matches!(
                    plan.graph().passes[trace.index].op,
                    Op::TraceRays { .. }
                ));
                assert!(step("Capture viewport").parents.contains(&trace.index));
            }
        }
    }
    Ok(())
}

fn differences(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count()
}

#[test]
#[ignore = "requires Vulkan 1.4 and VK_EXT_descriptor_heap"]
fn changing_gi_and_shadow_algorithms_changes_illumination() -> Result<()> {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let mut gpu = Backend::open()?;
    let gi = snapshot(Kind::Gi)?;
    let mut cfg = Settings::new(Kind::Gi);
    cfg.view = View::Indirect;
    cfg.gi = Gi::Off;
    let cache = geometry::Cache::default();
    let off = gpu.execute(pipeline::raster_frame(&gi, cfg, &cache)?.compile()?, &pool)?;
    let mut previous: Option<Vec<u8>> = None;
    for mode in [Gi::Ssgi, Gi::Probes, Gi::Ray] {
        cfg.gi = mode;
        let frame = gpu.execute(pipeline::raster_frame(&gi, cfg, &cache)?.compile()?, &pool)?;
        assert!(differences(&off.pixels, &frame.pixels) > 1000);
        if let Some(prev) = previous {
            assert!(differences(&prev, &frame.pixels) > 1000);
        }
        previous = Some(frame.pixels);
    }

    let shadow = snapshot(Kind::Shadow)?;
    let mut cfg = Settings::new(Kind::Shadow);
    cfg.view = View::Visibility;
    cfg.shadow = Shadow::Hard;
    let hard = gpu.execute(
        pipeline::raster_frame(&shadow, cfg, &cache)?.compile()?,
        &pool,
    )?;
    let partial = |pixels: &[u8]| {
        pixels
            .chunks_exact(4)
            .filter(|p| p[0] == p[1] && p[1] == p[2] && (4..252).contains(&p[0]))
            .count()
    };
    assert_eq!(partial(&hard.pixels), 0);
    for method in [Shadow::Pcf, Shadow::Pcss, Shadow::Ray] {
        cfg.shadow = method;
        let frame = gpu.execute(
            pipeline::raster_frame(&shadow, cfg, &cache)?.compile()?,
            &pool,
        )?;
        assert!(partial(&frame.pixels) > 50);
        assert!(differences(&hard.pixels, &frame.pixels) > 50);
    }
    cfg.shadow = Shadow::Pcss;
    cfg.size = 0.;
    let point = gpu.execute(
        pipeline::raster_frame(&shadow, cfg, &cache)?.compile()?,
        &pool,
    )?;
    assert_eq!(hard.pixels, point.pixels);
    Ok(())
}

#[test]
#[ignore = "requires VK_KHR_ray_tracing_pipeline and VK_KHR_acceleration_structure"]
fn native_ray_pipeline_renders_hits_and_off_camera_reflections() -> Result<()> {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let mut gpu = Backend::open()?;
    let mut world = scene(Kind::Rt)?;
    let camera = camera(Kind::Rt, "overview")?;
    let mut extractor = crate::render::SceneExtractor::default();
    let mut snapshot =
        |world: &World| extractor.snapshot(world, camera, [321, 199], Default::default());
    let view = snapshot(&world)?;
    let mut cfg = Settings::new(Kind::Rt);
    cfg.ray_view = RayView::Normals;
    let normals = gpu.execute(pipeline::trace_frame(&view, cfg)?.compile()?, &pool)?;
    assert!(normals.profile.acceleration_builds >= 2);
    // A sky-only frame cannot contain the upward, +Y normal of the receiver.
    assert!(
        normals
            .pixels
            .chunks_exact(4)
            .filter(|p| p[1] == 255 && p[0] == 128 && p[2] == 128)
            .count()
            > 1000
    );
    cfg.ray_view = RayView::Reflections;
    let reflections = gpu.execute(pipeline::trace_frame(&view, cfg)?.compile()?, &pool)?;
    assert_eq!(
        (
            reflections.profile.acceleration_builds,
            reflections.profile.acceleration_updates
        ),
        (0, 0)
    );
    assert!(
        reflections
            .pixels
            .chunks_exact(4)
            .filter(|p| p[1].saturating_sub(p[2]) > 45 && p[1].saturating_sub(p[0]) > 25)
            .count()
            > 20
    );

    let marker_material = world
        .entities
        .values()
        .find(|entity| entity.name == "Off-camera reflection marker")
        .unwrap()
        .instance
        .as_ref()
        .unwrap()
        .material
        .unwrap();
    let pigment = world
        .entities
        .get_mut(&marker_material)
        .unwrap()
        .material
        .as_mut()
        .unwrap();
    let incoming = pigment.base_color;
    pigment.base_color = [0.98, 0.03, 0.04, 1.];
    let repainted = gpu.execute(
        pipeline::trace_frame(&snapshot(&world)?, cfg)?.compile()?,
        &pool,
    )?;
    assert_eq!(
        (
            repainted.profile.acceleration_builds,
            repainted.profile.acceleration_updates
        ),
        (0, 0)
    );
    assert!(differences(&reflections.pixels, &repainted.pixels) > 20);
    world
        .entities
        .get_mut(&marker_material)
        .unwrap()
        .material
        .as_mut()
        .unwrap()
        .base_color = incoming;
    world
        .entities
        .retain(|_, entity| entity.name != "Off-camera reflection marker");
    let without = gpu.execute(
        pipeline::trace_frame(&snapshot(&world)?, cfg)?.compile()?,
        &pool,
    )?;
    assert!(without.profile.acceleration_builds >= 1);
    assert!(differences(&reflections.pixels, &without.pixels) > 20);
    Ok(())
}
