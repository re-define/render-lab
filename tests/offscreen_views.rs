use anyhow::{Context, Result, ensure};
use glam::{Mat4, Vec3};
use render_lab::{
    engine::{Edit, Engine, OffscreenView, System},
    render::{Camera, Forward, post},
    rhi::Frame,
    scene::{Id, World},
};
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const SIZE: [u32; 2] = [160, 100];
const DT: f32 = 1. / 60.;
const FRAMES: usize = 6;

struct Lift(Id);
impl System for Lift {
    fn update(&self, world: &World, dt: f32) -> Result<Edit> {
        let id = self.0;
        let moved = Mat4::from_translation(Vec3::Y * dt) * world.entities[&id].local;
        Ok(Box::new(move |world| world.set_local(id, moved).unwrap()))
    }
}

fn camera(secondary: bool, step: usize) -> Camera {
    Camera {
        yaw: if secondary {
            -0.7 - 0.021 * step as f32
        } else {
            0.2 + 0.015 * step as f32
        },
        pitch: if secondary { 0.4 } else { 0.15 },
        ..Camera::default()
    }
}

fn wait(engine: &Engine, secondary: bool) -> Result<Frame> {
    let start = Instant::now();
    loop {
        let result = if secondary {
            engine.take_secondary_frame()
        } else {
            engine.take_frame()
        };
        if let Some(result) = result {
            return result.map_err(anyhow::Error::msg);
        }
        ensure!(
            start.elapsed() < Duration::from_secs(30),
            "offscreen frame timed out"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

type PixelSequence = Vec<Vec<u8>>;

fn run(primary_is_second_camera: bool, paired: bool) -> Result<(PixelSequence, PixelSequence)> {
    let world = World::demo();
    let (&id, specimen) = world
        .entities
        .iter()
        .find(|(_, e)| e.instance.is_some())
        .context("demo specimen")?;
    let start_height = specimen.local.w_axis.y;
    let mut engine = Engine::new(world, Arc::new(Forward::default()))?;
    engine.schedule.add_system(Arc::new(Lift(id)));
    engine.select_post(post::Mode::Taa)?;
    let mut frames = (Vec::new(), Vec::new());
    for step in 0..FRAMES {
        engine.camera = camera(primary_is_second_camera, step);
        let secondary = paired.then_some(OffscreenView {
            camera: camera(true, step),
            extent: SIZE,
            post: engine.post,
        });
        ensure!(
            engine.tick_with_view(DT, SIZE, secondary)?,
            "unexpected admission skip"
        );
        let height = engine.world.entities[&id].local.w_axis.y;
        ensure!(
            (height - start_height - DT * (step + 1) as f32).abs() < 1e-5,
            "simulation did not advance exactly once"
        );
        let primary = wait(&engine, false)?;
        ensure!(primary.sequence == step as u64 + 1 && !primary.first_profile.secondary_view);
        ensure!(primary.profile.tick_id == (step + 1) as u64 && primary.profile.extract_ms > 0.);
        ensure!(
            primary
                .passes
                .iter()
                .any(|p| p.name == "Temporal AA resolve")
        );
        ensure!(
            primary
                .passes
                .iter()
                .filter(|p| p.name == "Ordinary mesh visibility")
                .count()
                == 2
        );
        ensure!(
            !primary
                .passes
                .iter()
                .any(|p| p.name == "Cull ordinary surfaces")
        );
        ensure!(primary.pixels.len() == (SIZE[0] * SIZE[1] * 4) as usize);
        ensure!(
            primary
                .pixels
                .chunks_exact(4)
                .any(|p| p != &primary.pixels[..4])
        );
        if paired {
            let second = wait(&engine, true)?;
            ensure!(second.sequence == step as u64 + 1 && second.first_profile.secondary_view);
            ensure!(
                second.profile.snapshot_id == primary.profile.snapshot_id
                    && second.profile.tick_id == primary.profile.tick_id
                    && second.profile.extract_ms == 0.,
                "secondary view extracted another snapshot"
            );
            ensure!(
                second.pixels != primary.pixels,
                "camera paths produced identical frames"
            );
            frames.1.push(second.pixels);
        }
        frames.0.push(primary.pixels);
    }
    Ok(frames)
}

#[test]
#[ignore = "requires Vulkan 1.4 descriptor heaps and mesh shaders"]
fn two_cameras_share_a_tick_and_match_their_own_taa_histories() -> Result<()> {
    let (first, _) = run(false, false)?;
    let (second, _) = run(true, false)?;
    let (paired_first, paired_second) = run(false, true)?;
    for (camera, expected, actual) in [
        ("main", first, paired_first),
        ("offscreen", second, paired_second),
    ] {
        for (i, (a, b)) in expected.iter().zip(&actual).enumerate() {
            ensure!(
                a == b,
                "{camera} TAA frame {i} differs in {} pixels",
                a.chunks_exact(4)
                    .zip(b.chunks_exact(4))
                    .filter(|(x, y)| x != y)
                    .count()
            );
        }
    }
    Ok(())
}
