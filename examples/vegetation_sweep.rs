//! Repeatable forest LOD sweep and fixed-camera wind/TAA measurements.
use anyhow::{Context, Result, ensure};
use render_lab::{console::Console, engine::Engine, experiments, rhi::Frame};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    time::{Duration, Instant},
};

const EXTENT: [u32; 2] = [800, 500];

fn frame(engine: &mut Engine) -> Result<Frame> {
    step(engine, 1. / 60.)
}

fn step(engine: &mut Engine, dt: f32) -> Result<Frame> {
    let start = Instant::now();
    ensure!(engine.tick(dt, EXTENT)?, "frame was not queued");
    loop {
        if let Some(frame) = engine.take_frame() {
            return frame.map_err(anyhow::Error::msg);
        }
        ensure!(start.elapsed() < Duration::from_secs(30), "frame timed out");
        std::thread::sleep(Duration::from_micros(200));
    }
}

fn delta(before: &[u8], after: &[u8]) -> f64 {
    let total: u64 = before
        .chunks_exact(4)
        .zip(after.chunks_exact(4))
        .map(|(a, b)| (0..3).map(|c| u64::from(a[c].abs_diff(b[c]))).sum::<u64>())
        .sum();
    total as f64 / ((EXTENT[0] * EXTENT[1] * 3) as f64)
}

fn capture(path: &Path, frame: &Frame) -> Result<()> {
    std::fs::create_dir_all(path.parent().context("capture directory")?)?;
    let mut rgba = frame.pixels.clone();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    image::save_buffer(path, &rgba, EXTENT[0], EXTENT[1], image::ColorType::Rgba8)?;
    Ok(())
}

fn write_foliage_mask(file: &mut BufWriter<File>, frame: &Frame) -> Result<()> {
    // The path view uses these two opaque BGRA colors. Branches stay uncolored
    // in this trial, but still participate in the depth test.
    let mut covered = 0u64;
    let mask: Vec<u8> = frame
        .pixels
        .chunks(32)
        .map(|chunk| {
            let mut bits = 0;
            for (i, bgra) in chunk.chunks_exact(4).enumerate() {
                if matches!(bgra, [79, 160, 234, 255] | [234, 198, 81, 255]) {
                    bits |= 1 << i;
                    covered += 1;
                }
            }
            bits
        })
        .collect();
    ensure!(
        covered == frame.counters["Vegetation pixels"],
        "path colors did not match foliage coverage"
    );
    file.write_all(&mask)?;
    Ok(())
}

/// Same seeded forest, wind phase and camera path for both temporal backends.
fn record_aa(engine: &mut Engine, console: &mut Console, mode: &str, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    for command in [
        "vegetation camera near",
        "vegetation wind 1",
        "vegetation time 0",
        &format!("post {mode}"),
        "post status",
    ] {
        println!("{}", console.execute(engine, &[], command)?);
    }
    let origin = engine.camera;
    let expected_pass = match mode {
        "taa" => "Temporal AA resolve",
        "dlss" => "NVIDIA DLSS Quality",
        _ => unreachable!(),
    };
    let mut encoder =
        render_lab::recording::Encoder::open(&dir.join(format!("forest-{mode}.mp4")), EXTENT, 30)?;
    for i in 0..120 {
        let t = ((i as f32 - 48.) / 71.).clamp(0., 1.);
        let ease = t * t * (3. - 2. * t);
        engine.camera.target.x = origin.target.x + 1.8 * ease;
        engine.camera.yaw = origin.yaw + 0.06 * ease;
        let current = step(engine, 1. / 30.)?;
        if i >= 24 {
            ensure!(
                current.extent == EXTENT && current.passes.iter().any(|p| p.name == expected_pass),
                "missing {expected_pass} at frame {i}"
            );
            let first = &current.pixels[..3];
            ensure!(
                current.pixels.chunks_exact(4).any(|p| &p[..3] != first),
                "uniform forest output at frame {i}"
            );
            if i == 39 {
                capture(&dir.join(format!("forest-{mode}.png")), &current)?;
            }
            encoder.write(&current)?;
        }
    }
    encoder.finish()?;
    println!("Recorded 96 frames with {expected_pass}");
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let trial = args
        .next()
        .context("choose auto, reference, wind, auto-mask, reference-mask, aa-taa, or aa-dlss")?;
    let capture_dir = args.next();
    let pixel_error = args.next().map(|arg| arg.parse::<f32>()).transpose()?;
    ensure!(args.next().is_none(), "unexpected argument");
    let mut world = experiments::vegetation::forest()?;
    let mut mask_file = if trial.ends_with("-mask") {
        let dir = Path::new(
            capture_dir
                .as_deref()
                .context("mask output directory required")?,
        );
        std::fs::create_dir_all(dir)?;
        for entity in world
            .entities
            .values_mut()
            .filter(|e| e.name.ends_with(" branches"))
        {
            entity.plant = None;
        }
        Some(BufWriter::new(File::create(
            dir.join(format!("{trial}.bits")),
        )?))
    } else {
        None
    };
    let mut engine = experiments::open(world)?;
    let mut console = Console::default();
    if matches!(trial.as_str(), "aa-taa" | "aa-dlss") {
        let dir = capture_dir
            .as_deref()
            .context("AA recording needs an output directory")?;
        return record_aa(&mut engine, &mut console, &trial[3..], Path::new(dir));
    }
    if let Some(pixels) = pixel_error {
        console.execute(&mut engine, &[], &format!("vegetation pixels {pixels}"))?;
    }
    if trial == "wind" {
        println!("wind,taa,frame,rgb_delta,gpu_ms,leaf_pixels");
        for wind in [0, 1] {
            for post in ["off", "taa"] {
                console.execute(&mut engine, &[], "vegetation camera near")?;
                console.execute(&mut engine, &[], "vegetation time 0")?;
                console.execute(&mut engine, &[], &format!("vegetation wind {wind}"))?;
                console.execute(&mut engine, &[], &format!("post {post}"))?;
                console.execute(&mut engine, &[], "post reset")?;
                for _ in 0..24 {
                    frame(&mut engine)?;
                }
                let mut previous = frame(&mut engine)?;
                for i in 0..48 {
                    let current = frame(&mut engine)?;
                    println!(
                        "{wind},{post},{i},{:.6},{:.4},{}",
                        delta(&previous.pixels, &current.pixels),
                        current.profile.gpu_span_ms.unwrap_or_default(),
                        current.counters["Vegetation pixels"]
                    );
                    previous = current;
                }
            }
        }
        return Ok(());
    }

    let render_mode = trial.strip_suffix("-mask").unwrap_or(&trial);
    ensure!(
        matches!(render_mode, "auto" | "reference"),
        "unknown trial {trial}"
    );
    for command in ["vegetation wind 0", "post off"] {
        console.execute(&mut engine, &[], command)?;
    }
    if mask_file.is_some() {
        console.execute(&mut engine, &[], "vegetation view paths")?;
    }
    if render_mode == "reference" {
        for command in ["vegetation triangles", "lod 0"] {
            console.execute(&mut engine, &[], command)?;
        }
    }
    engine.camera.distance = 110.;
    for _ in 0..16 {
        frame(&mut engine)?;
    }
    let mut previous = frame(&mut engine)?;
    println!(
        "frame,distance,leaf_pixels,rgb_delta,gpu_ms,device_mib,tri_clusters,voxel_clusters,capacity"
    );
    for i in 0..=120 {
        engine.camera.distance = 110. + 2.5 * i as f32;
        let current = frame(&mut engine)?;
        let n = |key| current.counters[key];
        println!(
            "{i},{:.1},{},{:.6},{:.4},{:.3},{},{},{}",
            engine.camera.distance,
            n("Vegetation pixels"),
            delta(&previous.pixels, &current.pixels),
            current.profile.gpu_span_ms.unwrap_or_default(),
            current.memory.device_local_bytes as f64 / 1048576.,
            n("Triangle plant clusters"),
            n("Voxel plant clusters"),
            n("Potential clusters")
        );
        if let Some(dir) = &capture_dir
            && matches!(i, 0 | 30 | 65 | 100 | 120)
        {
            capture(
                &Path::new(dir).join(format!("{trial}-{i:03}.png")),
                &current,
            )?;
        }
        if let Some(file) = &mut mask_file {
            write_foliage_mask(file, &current)?;
        }
        previous = current;
    }
    if let Some(file) = &mut mask_file {
        file.flush()?;
    }
    Ok(())
}
