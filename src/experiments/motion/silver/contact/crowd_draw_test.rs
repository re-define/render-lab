use super::*;
use crate::{
    rdg::{Access, Dispatch, Op, Shader, Storage},
    render::{SceneExtractor, Snapshot, Technique, post},
    rhi::Backend,
};

enum Product {
    Ids,
    Depth,
    Motion,
}

fn capture(
    gpu: &mut Backend,
    pool: &rayon::ThreadPool,
    tech: &dyn Technique,
    view: &Snapshot,
    product: &Product,
) -> Result<Vec<u8>> {
    let mut g = tech.build(view)?;
    let pixels = view.extent[0] * view.extent[1];
    let (src, words) = match product {
        Product::Ids => (g.products().visibility.context("no visibility")?, 2),
        Product::Depth => (g.products().depth.context("no depth")?, 1),
        Product::Motion => (g.products().motion.context("no motion")?, 6),
    };
    let out = g.resource(
        "Reference comparison",
        Storage::Buffer {
            bytes: (pixels * words) as usize * 4,
            initial: None,
        },
    );
    if matches!(product, Product::Motion) {
        let n = pixels * words;
        let groups = n.div_ceil(64);
        g.pass(
            "Read motion samples",
            Op::Compute {
                program: Dispatch {
                    shader: Shader::Compute(include_bytes!(concat!(
                        env!("OUT_DIR"),
                        "/surface_tail.comp.spv"
                    ))),
                    bindings: vec![(src, Access::ComputeRead), (out, Access::ComputeWrite)],
                    push: bytemuck::bytes_of(&[0u32, n]).to_vec(),
                },
                groups: [groups.min(32768), groups.div_ceil(32768), 1],
            },
        );
    } else {
        g.pass(
            "Read visibility attachment",
            Op::ReadPixels {
                image: src,
                buffer: out,
            },
        );
    }
    g.display_extent = Some([view.extent[0], view.extent[1] * words]);
    g.set_output(out);
    let frame = gpu.execute(g.compile()?, pool)?;
    let scopes = frame
        .passes
        .iter()
        .filter(|p| p.name == "Batched ordinary mesh visibility")
        .count();
    if matches!(product, Product::Ids) {
        let direct = frame
            .passes
            .iter()
            .filter(|p| p.name == "Ordinary mesh visibility")
            .count();
        println!("Ordinary visibility: {direct} direct scopes, {scopes} indirect scopes");
    }
    if !view.reference_draws {
        ensure!(
            (1..64).contains(&scopes),
            "unexpected indirect packet count: {scopes}"
        );
    }
    Ok(frame.pixels)
}

#[test]
#[ignore = "1080p frozen Silver Wolf crowd comparison; requires Vulkan and character assets"]
fn frozen_crowd_draws_match_ids_depth_and_motion() -> Result<()> {
    let engine = crate::experiments::open(scene()?)?;
    let (pool, tech, materials) = (
        engine.tasks.clone(),
        engine.technique.clone(),
        engine.materials.clone(),
    );
    drop(engine);
    // Reopen the same saved crowd before constructing either view. Resident geometry
    // and animation caches must be rebuilt from the loaded scene, too.
    let (world, _) = population(300, &pool)?;
    let dir = tempfile::tempdir()?;
    let save = dir.path().join("frozen-crowd.json");
    world.save(&save)?;
    drop(world);
    let mut world = Arc::new(World::load(&save)?);
    let mut schedule = Schedule::default();
    install(&world, &mut schedule)?;
    let camera = Camera {
        target: Vec3::ZERO,
        distance: 91.,
        yaw: 0.10,
        pitch: 0.89,
    };
    // A real previous pose and the exact same jitter, camera, and independent current poses
    // reach both draws. The motion product includes correspondence and validity as well as XY.
    let mut seq = post::Sequence::default();
    let mut extractor = SceneExtractor::default();
    let mut settings = post::Settings::default();
    settings.select_view(post::DebugView::Motion);
    let mut old = extractor.snapshot(&world, camera, [1920, 1080], materials.clone())?;
    seq.assign(settings, &tech, &mut old);
    drop(old);
    for _ in 0..6 {
        schedule.tick(&mut world, 1. / 60., &pool)?;
    }
    let mut view = extractor.snapshot(&world, camera, [1920, 1080], materials)?;
    let mut gpu = Backend::open()?;
    view.buffer_atomic64 = gpu.buffer_atomic64();
    view.ray_caps = gpu.ray_capabilities();
    seq.assign(settings, &tech, &mut view);
    for product in [Product::Ids, Product::Depth, Product::Motion] {
        view.reference_draws = true;
        let baseline = capture(&mut gpu, &pool, tech.as_ref(), &view, &product)?;
        view.reference_draws = false;
        let actual = capture(&mut gpu, &pool, tech.as_ref(), &view, &product)?;
        let (name, stride) = match product {
            Product::Ids => ("surface/triangle IDs", 8),
            Product::Depth => ("depth", 4),
            Product::Motion => ("motion and correspondence", 24),
        };
        let changed = actual
            .chunks_exact(stride)
            .zip(baseline.chunks_exact(stride))
            .filter(|(a, b)| a != b)
            .count();
        ensure!(
            actual.len() == 1920 * 1080 * stride && actual.len() == baseline.len(),
            "short {name} readback"
        );
        ensure!(changed == 0, "{name}: {changed} different pixels");
        let covered = actual
            .chunks_exact(stride)
            .filter(|v| match product {
                Product::Ids => v[..4] != [0; 4],
                Product::Depth => v[..4] != 1f32.to_le_bytes(),
                Product::Motion => {
                    v[..8] != [0; 8] && u32::from_le_bytes(v[20..24].try_into().unwrap()) & 1 != 0
                }
            })
            .count();
        ensure!(
            covered > 100,
            "{name}: empty product ({covered} covered samples)"
        );
        println!("{name}: {covered} covered/tracked samples, {changed} different pixels");
    }
    Ok(())
}

#[test]
#[ignore = "requires Vulkan 1.4 mesh shaders and descriptor heaps"]
fn coplanar_draws_preserve_order_and_offscreen_shadow_geometry() -> Result<()> {
    use crate::render::{Forward, FrameGraph};
    let mut world = World::default();
    let mesh = world.spawn(Entity {
        mesh: Some(Arc::new(Mesh {
            positions: vec![[-1., -1., 0.], [1., -1., 0.], [0., 1., 0.]],
            indices: vec![0, 1, 2],
            normals: vec![[0., 0., 1.]; 3],
            uv: vec![],
            joints: vec![],
            weights: vec![],
        })),
        ..Default::default()
    });
    for i in 0..24 {
        world.spawn(Entity {
            instance: Some(Instance::new(mesh, None)),
            local: Mat4::from_translation(if i % 6 == 0 {
                Vec3::X * 1000.
            } else if i == 4 {
                Vec3::new(0.4, 0., 0.25) // Partial, strictly nearer occluder.
            } else {
                Vec3::ZERO
            }),
            ..Default::default()
        });
    }
    let extent = [96, 64];
    let mut view = Snapshot::new(
        &world,
        Camera {
            target: Vec3::ZERO,
            distance: 4.,
            yaw: 0.,
            pitch: 0.,
        },
        extent,
        Default::default(),
    )?;
    let tech = Forward::default();
    let mut gpu = Backend::open()?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let ids = |mut g: FrameGraph, gpu: &mut Backend| -> Result<Vec<u8>> {
        let src = g.products().visibility.context("no visibility")?;
        assert_eq!(g.products().ray_surfaces.len(), 24);
        let out = g.resource(
            "Overlapping triangles",
            Storage::Buffer {
                bytes: (96 * 64 * 8),
                initial: None,
            },
        );
        g.pass(
            "Read overlapping triangle IDs",
            Op::ReadPixels {
                image: src,
                buffer: out,
            },
        );
        g.display_extent = Some([192, 64]);
        g.set_output(out);
        Ok(gpu.execute(g.compile()?, &pool)?.pixels)
    };
    view.reference_draws = true;
    let reference = ids(tech.build(&view)?, &mut gpu)?;
    view.reference_draws = false;
    let visible: BTreeSet<_> = reference
        .chunks_exact(8)
        .map(|p| u32::from_le_bytes(p[..4].try_into().unwrap()))
        .filter(|&id| id != 0)
        .collect();
    assert_eq!(
        visible.len(),
        2,
        "near triangle must only occlude part of the coplanar stack"
    );
    assert!(
        reference == ids(tech.build(&view)?, &mut gpu)?,
        "depth ties changed owners"
    );

    let mut g = tech.build(&view)?;
    let args = g
        .passes
        .iter()
        .find_map(|pass| {
            if pass.name == "Build ordered ordinary mesh work" {
                return pass
                    .op
                    .program()
                    .and_then(|op| op.bindings.last())
                    .map(|(r, _)| *r);
            }
            None
        })
        .context("missing indirect command")?;
    let out = g.resource(
        "Read indirect work",
        Storage::Buffer {
            bytes: 12,
            initial: None,
        },
    );
    g.pass(
        "Copy argument slots",
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/surface_tail.comp.spv"
                ))),
                bindings: vec![(args, Access::ComputeRead), (out, Access::ComputeWrite)],
                push: bytemuck::bytes_of(&[0u32, 3]).to_vec(),
            },
            groups: [1; 3],
        },
    );
    g.display_extent = Some([3, 1]);
    g.set_output(out);
    let frame = gpu.execute(g.compile()?, &pool)?;
    assert_eq!(&frame.pixels[..], bytemuck::bytes_of(&[20u32, 1, 1]));
    Ok(())
}
