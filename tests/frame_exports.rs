use anyhow::{Context, Result};
use render_lab::{
    experiments::{self, lighting},
    rdg::{Op, Resource},
    render::{Camera, Forward, FrameGraph, Snapshot, Technique},
    rhi::Backend,
    scene::World,
};

const SIZE: [u32; 2] = [160, 100];

fn add_diagnostic(frame: &mut FrameGraph) -> Resource {
    // The small readback cannot contain viewport pixels. Its insertion must leave the
    // diagnostic observable without affecting the independent display/capture output.
    let resource = frame.input("Debug probe", vec![0x55; 16]);
    frame.pass("Read debug probe", Op::HostRead(resource));
    resource
}

fn check_window_export(mut frame: FrameGraph, raster: bool) -> Result<()> {
    let color = frame
        .products()
        .final_color
        .context("missing final color")?;
    assert_eq!(frame.products().visibility.is_some(), raster);
    assert_eq!(frame.products().depth.is_some(), raster);
    let diagnostic = add_diagnostic(&mut frame);
    assert_eq!(frame.present()?, color);
    let plan = frame.compile()?;
    let passes = &plan.graph().passes;
    assert!(
        passes
            .iter()
            .any(|p| matches!(p.op, Op::Present(r) if r == color))
    );
    assert!(
        passes
            .iter()
            .any(|p| matches!(p.op, Op::HostRead(r) if r == diagnostic))
    );
    assert!(
        !passes
            .iter()
            .any(|p| matches!(p.op, Op::HostRead(r) if r == color))
    );
    Ok(())
}

#[test]
fn window_export_preserves_an_unrelated_readback() -> Result<()> {
    let world = World::demo();
    let view = Snapshot::new(&world, Camera::fit(&world)?, SIZE, Default::default())?;
    check_window_export(Forward::default().build(&view)?, true)
}

#[test]
fn frame_output_must_be_explicit_and_cover_the_full_display() {
    let mut frame = FrameGraph::new(SIZE);
    add_diagnostic(&mut frame);
    assert!(frame.present().is_err());
    assert!(frame.compile().is_err());

    let mut frame = FrameGraph::new(SIZE);
    let undersized = add_diagnostic(&mut frame);
    frame.set_output(undersized);
    assert!(frame.compile().is_err());
}

#[test]
fn replacing_the_selected_window_color_requires_reselecting_it() -> Result<()> {
    let mut frame = FrameGraph::new([1, 1]);
    let original = frame.input("original", vec![0; 16]);
    let replacement = frame.input("replacement", vec![1; 16]);
    frame.set_output(original);
    assert_eq!(frame.present()?, original);
    frame.set_output(replacement);
    assert!(frame.compile().is_err());
    Ok(())
}

#[test]
#[ignore = "requires Vulkan 1.4, mesh shaders and a hardware ray tracing pipeline"]
fn raster_and_ray_capture_export_the_same_pixels_with_extra_readbacks() -> Result<()> {
    let mut gpu = Backend::open()?;
    let tasks = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    for (world, raster) in [
        (World::demo(), true),
        (lighting::scene(lighting::Kind::Rt)?, false),
    ] {
        let engine = experiments::open(world)?;
        let view = Snapshot::new(&engine.world, engine.camera, SIZE, engine.materials.clone())?;
        check_window_export(engine.technique.build(&view)?, raster)?;
        let reference = gpu.execute(engine.technique.build(&view)?.compile()?, &tasks)?;
        let mut frame = engine.technique.build(&view)?;
        let color = frame
            .products()
            .final_color
            .context("missing capture color")?;
        let diagnostic = add_diagnostic(&mut frame);
        let plan = frame.compile()?;
        for resource in [color, diagnostic] {
            assert!(
                plan.graph()
                    .passes
                    .iter()
                    .any(|p| matches!(p.op, Op::HostRead(r) if r == resource))
            );
        }
        let captured = gpu.execute(plan, &tasks)?;
        assert_eq!(captured.extent, SIZE);
        assert_eq!(captured.pixels.len(), (SIZE[0] * SIZE[1] * 4) as usize);
        assert!(
            captured
                .pixels
                .chunks_exact(4)
                .any(|pixel| pixel != &captured.pixels[..4])
        );
        assert_eq!(captured.pixels, reference.pixels);
    }
    Ok(())
}
