//! Capture and time the same deterministic forest wind sequence for both caster LODs and suns.
#[path = "forest_shadows/proxy_images.rs"]
mod proxy_images;
#[path = "forest_shadows/zoom.rs"]
mod zoom;
use anyhow::{Context, Result, ensure};
use render_lab::{console::Console, engine::Engine, experiments, rhi::Frame};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

fn frame(e: &mut Engine, dt: f32, extent: [u32; 2]) -> Result<Frame> {
    let start = Instant::now();
    ensure!(e.tick(dt, extent)?, "frame queue full");
    loop {
        if let Some(result) = e.take_frame() {
            return result.map_err(anyhow::Error::msg);
        }
        ensure!(
            start.elapsed() < Duration::from_secs(120),
            "frame timed out"
        );
        std::thread::sleep(Duration::from_micros(200));
    }
}
fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().context(
        "usage: forest_shadows OUTPUT reference|realtime [frames] [rounds], proxy-images, or zoom [laps] [purposes] [--size WIDTHxHEIGHT] [--raster auto|mesh|compute|linear] [--rays auto|software|hardware|inline] [--gi direct|single|multi]",
    )?);
    let sampling = args.next().context("missing sampling mode")?;
    if sampling == "proxy-images" {
        return proxy_images::run(&dir);
    }
    if sampling == "zoom" {
        let laps = args.next().as_deref().unwrap_or("1").parse()?;
        let mut trace = false;
        let mut extent = [1616, 878];
        let mut raster = String::from("auto");
        let mut ray_backend = None;
        let mut gi = None;
        while let Some(option) = args.next() {
            match option.as_str() {
                "purposes" => trace = true,
                "--gi" => {
                    let value = args.next().context("--gi needs direct, single or multi")?;
                    ensure!(
                        matches!(value.as_str(), "direct" | "single" | "multi"),
                        "invalid GI mode"
                    );
                    gi = Some(value);
                }
                "--rays" | "--proxy-rays" => {
                    let value = args
                        .next()
                        .context("--rays needs auto, software, hardware or inline")?;
                    ensure!(
                        matches!(value.as_str(), "hardware" | "inline" | "auto" | "software"),
                        "invalid ray backend"
                    );
                    ray_backend = Some(value);
                }
                "--raster" => {
                    raster = args
                        .next()
                        .context("--raster needs auto, mesh, compute or linear")?;
                    ensure!(
                        matches!(raster.as_str(), "auto" | "mesh" | "compute" | "linear"),
                        "invalid raster mode"
                    );
                }
                "--size" => {
                    let size = args.next().context("--size needs WIDTHxHEIGHT")?;
                    let (w, h) = size.split_once('x').context("--size needs WIDTHxHEIGHT")?;
                    extent = [w.parse::<u32>()?, h.parse::<u32>()?];
                    ensure!(
                        extent.iter().all(|&n| n > 0 && n <= i32::MAX as u32)
                            && extent[0].checked_mul(extent[1]).is_some(),
                        "invalid viewport size"
                    );
                }
                _ => anyhow::bail!("unexpected zoom option {option}"),
            }
        }
        return zoom::run(
            &dir,
            laps,
            trace,
            extent,
            &raster,
            ray_backend.as_deref(),
            gi.as_deref(),
        );
    }
    ensure!(
        matches!(sampling.as_str(), "reference" | "realtime"),
        "expected reference or realtime"
    );
    let frames: u32 = args.next().as_deref().unwrap_or("64").parse()?;
    let rounds: u32 = args.next().as_deref().unwrap_or("2").parse()?;
    let compare = args.next();
    ensure!(
        compare.as_deref().is_none_or(|mode| mode == "mega"),
        "expected optional mega backend comparison"
    );
    ensure!(frames > 0 && rounds > 0, "empty benchmark");
    std::fs::create_dir_all(&dir)?;
    let world = experiments::restir::forest_study()?;
    let sun = world
        .entities
        .iter()
        .find(|(_, e)| e.name == "Late afternoon sun")
        .context("missing forest sun")?
        .0;
    let mut csv = BufWriter::new(File::create(dir.join(format!("{sampling}.csv")))?);
    writeln!(
        csv,
        "round,geometry,sun,frame,gpu_span_ms,shadow_query_ms,ray_geometry_ms,fine_instances,fine_triangles,coarse_instances,coarse_triangles,as_build_ms,geometry_prepare_ms,clas_ms,cluster_blas_ms,tlas_ms,clusters,offscreen_clusters,transition_clusters,voxel_clusters,cluster_triangles,frame_local_device_mib"
    )?;
    let (baseline, study) = if compare.is_some() {
        ("adaptive", "mega")
    } else {
        ("coarse", "adaptive")
    };
    let mut cases = [
        (baseline, "hard", 0.),
        (study, "soft", 0.27),
        (study, "hard", 0.),
        (baseline, "soft", 0.27),
    ];
    for round in 0..rounds {
        for (geometry, source, radius) in cases {
            let mut engine = experiments::open(world.clone())?;
            let mut console = Console::default();
            for command in [
                format!("vegetation shadows {geometry}"),
                format!("light angular-radius {sun} {radius}"),
                "restir gi direct".into(),
            ] {
                console.execute(&mut engine, &[], &command)?;
            }
            if sampling == "reference" {
                console.execute(&mut engine, &[], "restir reference")?;
                console.execute(&mut engine, &[], "restir denoise off")?;
            }
            // Match the CLI capture start, fixed camera, wind phase and frame number.
            for _ in 0..12 {
                engine
                    .schedule
                    .tick(&mut engine.world, 1. / 60., &engine.tasks)?;
            }
            for _ in 0..48 {
                frame(&mut engine, 1. / 60., [800, 500])?;
            }
            let image = frame(&mut engine, 0., [800, 500])?;
            if round == 0 {
                let prefix = if sampling == "realtime" {
                    "realtime-"
                } else {
                    ""
                };
                let mut rgba = image.pixels;
                for p in rgba.chunks_exact_mut(4) {
                    p.swap(0, 2);
                }
                image::save_buffer(
                    dir.join(format!("{prefix}{geometry}-{source}.png")),
                    &rgba,
                    800,
                    500,
                    image::ColorType::Rgba8,
                )?;
            }
            for index in 0..frames {
                let f = frame(&mut engine, 1. / 60., [800, 500])?;
                let span = f
                    .profile
                    .gpu_span_ms
                    .context("device lacks common GPU timestamp clock")?;
                let query = f
                    .passes
                    .iter()
                    .find(|p| {
                        matches!(
                            p.name.as_str(),
                            "Stratified direct-light reference"
                                | "Trace one shadow ray per sampled pixel"
                                | "Trace Mega Geometry shadow rays"
                        )
                    })
                    .context("missing shadow query")?
                    .ms;
                let time = |names: &[&str]| {
                    f.passes
                        .iter()
                        .filter(|p| names.contains(&p.name.as_str()))
                        .map(|p| p.ms)
                        .sum::<f64>()
                };
                let clas = time(&["Build selected triangle CLAS"]);
                let cluster_blas = time(&["Build forest cluster BLAS"]);
                let tlas = time(&["Build scene TLAS"]);
                let build = clas
                    + cluster_blas
                    + tlas
                    + time(&[
                        "Build triangle BLAS",
                        "Bound deformed triangles",
                        "Refit ray BVH level",
                    ]);
                let prepare = time(&[
                    "Deform detailed vegetation shadow triangles",
                    "Deform offscreen and visible ray proxies",
                    "Gather shared ray triangles",
                    "Pose selected shadow clusters",
                    "Finalize cluster build records",
                    "Pack native ray indices",
                ]);
                let acceleration = build + prepare;
                let c = &f.counters;
                writeln!(
                    csv,
                    "{round},{geometry},{source},{index},{span:.6},{query:.6},{acceleration:.6},{},{},{},{},{build:.6},{prepare:.6},{clas:.6},{cluster_blas:.6},{tlas:.6},{},{},{},{},{},{:.2}",
                    c["Detailed shadow instances"],
                    c["Detailed shadow triangles"],
                    c["Coarse shadow instances"],
                    c["Coarse shadow triangles"],
                    c.get("CLAS including empty sentinel")
                        .copied()
                        .unwrap_or(0)
                        .saturating_sub(1),
                    c.get("Offscreen shadow clusters").copied().unwrap_or(0),
                    c.get("Transition shadow clusters").copied().unwrap_or(0),
                    c.get("Voxel shadow clusters").copied().unwrap_or(0),
                    c.get("CLAS triangles including sentinel")
                        .copied()
                        .unwrap_or(0)
                        .saturating_sub(1),
                    f.memory.device_local_bytes as f64 / (1024. * 1024.)
                )?;
            }
            csv.flush()?;
            println!(
                "{} {sampling} {round} {geometry} {source} complete",
                image.gpu
            );
        }
        cases.reverse();
    }
    Ok(())
}
