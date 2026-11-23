//! Five twelve-second takes of the same seeded river, rendered at fixed simulation steps.
use anyhow::{Context, Result, ensure};
use glam::Vec3;
use render_lab::{
    console::Console,
    engine::Engine,
    experiments::{self, restir, vegetation},
    recording::Encoder,
    render::Camera,
    rhi::Frame,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

const SIZE: [u32; 2] = [1920, 1080];
const FPS: u32 = 30;
const FRAMES: u32 = 12 * FPS;
const PRESETS: [&str; 5] = ["basin", "river", "grass", "river", "river"];

fn camera(base: Camera, shot: usize, t: f32) -> Camera {
    let t = t * t * (3.0 - 2.0 * t);
    let (eye, target) = match shot {
        0 => (
            Vec3::new(-11.0 + 25.0 * t, -8.0 * t, -9.0 * t),
            Vec3::new(9.0 * t, 0.0, 14.0 * t),
        ),
        1 => (
            Vec3::new(1.4 - 3.4 * t, -0.7 * t, -9.0 * t),
            Vec3::new(-3.1 * t, -0.15, -8.0 * t),
        ),
        2 => (
            Vec3::new(-0.3 - 0.45 * t, 0.26, -1.5 * t),
            Vec3::new(0.8 - 1.8 * t, 0.05, -1.9 * t),
        ),
        3 => (
            Vec3::new(11.0 - 11.0 * t, 12.2 + 1.6 * t, -6.0 - 9.0 * t),
            Vec3::new(1.5 - 5.5 * t, -0.6, 19.0 - 7.0 * t),
        ),
        _ => (
            Vec3::new(-1.6 - 1.5 * t, -1.15 + 0.16 * t, -8.0 * t),
            Vec3::new(26.0 - 2.0 * t, -1.0, 36.0 - 7.0 * t),
        ),
    };
    let target = base.target + target;
    let delta = base.eye() + eye - target;
    Camera {
        target,
        distance: delta.length(),
        yaw: delta.x.atan2(delta.z),
        pitch: (delta.y / delta.length()).asin(),
    }
}

fn render(engine: &mut Engine, dt: f32, size: [u32; 2]) -> Result<Frame> {
    ensure!(engine.tick(dt, size)?, "frame queue full");
    let start = Instant::now();
    loop {
        if let Some(frame) = engine.take_frame() {
            return frame.map_err(anyhow::Error::msg);
        }
        ensure!(
            start.elapsed() < Duration::from_secs(180),
            "film frame timed out"
        );
        std::thread::sleep(Duration::from_micros(300));
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let output = PathBuf::from(
        args.next()
            .context("expected output.mp4 [preview | shot N]")?,
    );
    let (preview, shots) = match args.next().as_deref() {
        None => (false, 0..5),
        Some("preview") => (true, 0..5),
        Some("shot") => {
            let n: usize = args.next().context("expected shot number 1–5")?.parse()?;
            ensure!((1..=5).contains(&n), "expected shot number 1–5");
            (false, n - 1..n)
        }
        Some(other) => anyhow::bail!("unknown mode {other}"),
    };
    let size = SIZE;
    let mut encoder = (!preview)
        .then(|| Encoder::open(&output, SIZE, FPS))
        .transpose()?;
    for shot in shots {
        let close = shot != 0;
        let world = if close {
            restir::river_shadow_study()?
        } else {
            vegetation::river_forest()?
        };
        let mut engine = experiments::open(world)?;
        let mut console = Console::default();
        if close {
            for cmd in ["vegetation shadows mega", "restir gi direct"] {
                console.execute(&mut engine, &[], cmd)?;
            }
        }
        console.execute(&mut engine, &[], "post taa")?;
        console.execute(
            &mut engine,
            &[],
            &format!("vegetation camera {}", PRESETS[shot]),
        )?;
        let base = engine.camera;
        let time = shot * 12 + if preview { 6 } else { 0 };
        for clock in ["vegetation time", "effect time"] {
            console.execute(&mut engine, &[], &format!("{clock} {time}"))?;
        }
        engine.camera = camera(base, shot, if preview { 0.5 } else { 0.0 });
        engine.post.reset();
        for _ in 0..10 {
            render(&mut engine, 0.0, size)?;
        }
        let frames = if preview { 1 } else { FRAMES };
        for i in 0..frames {
            if !preview {
                engine.camera = camera(base, shot, i as f32 / (FRAMES - 1) as f32);
            }
            let f = render(&mut engine, 1.0 / FPS as f32, size)?;
            if i == 0 {
                ensure!(f.extent == size, "unexpected render extent");
                ensure!(
                    f.passes
                        .iter()
                        .any(|p| p.name == "Advect forest river surface"),
                    "missing GPU river"
                );
                if close {
                    ensure!(
                        f.passes
                            .iter()
                            .any(|p| p.name == "Trace Mega Geometry shadow rays"),
                        "missing hardware shadows"
                    );
                }
                eprintln!(
                    "shot {}: {} x {}, {} GPU passes, {} shadow clusters",
                    shot + 1,
                    size[0],
                    size[1],
                    f.passes.len(),
                    f.counters
                        .get("CLAS including empty sentinel")
                        .copied()
                        .unwrap_or(0)
                        .saturating_sub(1)
                );
            }
            if let Some(encoder) = &mut encoder {
                encoder.write(&f)?;
            } else {
                let mut rgba = f.pixels;
                for p in rgba.chunks_exact_mut(4) {
                    p.swap(0, 2);
                }
                image::save_buffer(
                    output.with_file_name(format!("film-shot-{}.png", shot + 1)),
                    &rgba,
                    size[0],
                    size[1],
                    image::ColorType::Rgba8,
                )?;
            }
        }
        eprintln!("completed shot {}", shot + 1);
    }
    if let Some(encoder) = encoder {
        encoder.finish()?;
    }
    Ok(())
}
