//! Measure native CLAS reuse and forced rebuilds on identical deterministic forest paths.
use anyhow::{Context, Result, ensure};
use render_lab::{console::Console, engine::Engine, experiments, rhi::Frame};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

fn render(engine: &mut Engine) -> Result<Frame> {
    ensure!(engine.tick(0., [800, 500])?, "forest render queue full");
    let start = Instant::now();
    loop {
        if let Some(frame) = engine.take_frame() {
            return frame.map_err(anyhow::Error::msg);
        }
        ensure!(
            start.elapsed() < Duration::from_secs(120),
            "forest GPU frame timed out"
        );
        std::thread::sleep(Duration::from_micros(200));
    }
}
fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(
        args.next()
            .context("usage: forest_cluster_cache OUTPUT.csv [frames] [rounds]")?,
    );
    let frames: usize = args.next().as_deref().unwrap_or("32").parse()?;
    let rounds: usize = args.next().as_deref().unwrap_or("2").parse()?;
    ensure!(
        frames >= 4 && rounds > 0,
        "require at least four samples and one round"
    );
    if let Some(dir) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut csv = BufWriter::new(File::create(out)?);
    writeln!(
        csv,
        "round,case,cache,frame,selected,new,transform,pose,reused,prepare_ms,clas_ms,blas_ms,tlas_ms,trace_ms,gpu_ms,local_bytes,local_peak_bytes,as_bytes,as_peak_bytes,scratch_bytes,scratch_peak_bytes,nonlocal_bytes"
    )?;
    let world = experiments::restir::forest_study()?;
    let mut reference = BTreeMap::new();
    for round in 0..rounds {
        let modes = if round % 2 == 0 {
            ["off", "on"]
        } else {
            ["on", "off"]
        };
        for cache in modes {
            for case in ["fixed", "camera", "wind", "cut"] {
                let mut engine = experiments::open(world.clone())?;
                let mut console = Console::default();
                for cmd in [
                    "vegetation shadows mega",
                    "restir gi direct",
                    "vegetation wind 0",
                ] {
                    console.execute(&mut engine, &[], cmd)?;
                }
                console.execute(&mut engine, &[], &format!("vegetation cache {cache}"))?;
                if case == "wind" {
                    console.execute(&mut engine, &[], "vegetation wind 1")?;
                }
                let original = engine.camera;
                for _ in 0..12 {
                    render(&mut engine)?;
                }
                let mut spans = Vec::new();
                for i in 0..frames {
                    let t = (i + 1) as f32 / frames as f32;
                    match case {
                        "camera" => engine.camera.yaw = original.yaw + 0.18 * t,
                        "wind" => {
                            console.execute(
                                &mut engine,
                                &[],
                                &format!("vegetation time {}", (i + 1) as f32 / 60.),
                            )?;
                        }
                        "cut" => {
                            let stage = (4 * i / frames).min(3);
                            engine.camera.distance =
                                original.distance * [0.46, 1.55, 0.73, 1.0][stage];
                            engine.camera.yaw = original.yaw + [0.41, -0.26, 0.18, 0.0][stage];
                        }
                        _ => {}
                    }
                    let f = render(&mut engine)?;
                    let c = &f.counters;
                    let sel = c["CLAS including empty sentinel"] - 1;
                    let (new, transform, pose, reused) = (
                        c["New CLAS"],
                        c["Transform invalidated CLAS"],
                        c["Pose invalidated CLAS"],
                        c["Reused CLAS"],
                    );
                    ensure!(
                        sel == new + transform + pose + reused,
                        "CLAS accounting mismatch"
                    );
                    ensure!(
                        cache != "off" || new == sel,
                        "forced baseline reused native CLAS"
                    );
                    if let Some(old) = reference.insert((case, i), sel) {
                        ensure!(old == sel, "mismatched forest paths");
                    }
                    let ms = |name: &str| -> Result<f64> {
                        Ok(f.passes
                            .iter()
                            .find(|p| p.name == name)
                            .with_context(|| format!("missing pass {name}"))?
                            .ms)
                    };
                    let prepare = ms("Pose selected shadow clusters")?
                        + ms("Finalize cluster build records")?;
                    let clas = ms("Build selected triangle CLAS")?;
                    let blas = ms("Build forest cluster BLAS")?;
                    let tlas = ms("Build scene TLAS")?;
                    let trace = ms("Trace Mega Geometry shadow rays")?;
                    let span = f
                        .profile
                        .gpu_span_ms
                        .context("GPU does not expose a common timestamp clock")?;
                    let m = &f.memory.managed;
                    writeln!(
                        csv,
                        "{round},{case},{cache},{i},{sel},{new},{transform},{pose},{reused},{prepare:.6},{clas:.6},{blas:.6},{tlas:.6},{trace:.6},{span:.6},{},{},{},{},{},{},{}",
                        m.device_local.total(),
                        m.peak_device_local_bytes,
                        m.device_local.acceleration,
                        m.class_peaks.acceleration,
                        m.device_local.build_scratch,
                        m.class_peaks.build_scratch,
                        m.nonlocal.total()
                    )?;
                    spans.push(span);
                }
                csv.flush()?;
                spans.sort_by(f64::total_cmp);
                eprintln!(
                    "{case} cache={cache} GPU P95 {:.3} ms",
                    spans[(spans.len() * 95).div_ceil(100) - 1]
                );
            }
        }
    }
    Ok(())
}
