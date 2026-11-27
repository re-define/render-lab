mod presets;
mod ui;
use anyhow::{Context, Result, bail};
use render_lab::experiments::{self, motion};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let mut capture = None;
    let mut secondary_capture = None;
    let mut secondary_record = None;
    let mut secondary_yaw = 45.0_f32;
    let mut warmup = None;
    let mut output_size: Option<[u32; 2]> = None;
    let mut benchmark = None;
    let mut commands = Vec::new();
    let mut recordings = Vec::new();
    let mut fps = 30u32;
    let mut duration = 5f64;
    let mut events = Vec::<(f64, String)>::new();
    let mut scene = None;
    let mut experiment = String::new();
    while let Some(option) = args.next() {
        match option.to_str() {
            Some("--record") => recordings.push(PathBuf::from(
                args.next().context("--record needs a GIF or MP4 path")?,
            )),
            Some("--secondary-capture") => {
                secondary_capture = Some(PathBuf::from(
                    args.next().context("missing secondary PNG path")?,
                ))
            }
            Some("--secondary-record") => {
                secondary_record = Some(PathBuf::from(
                    args.next().context("missing secondary recording path")?,
                ))
            }
            Some("--secondary-yaw") => {
                secondary_yaw = args
                    .next()
                    .context("missing orbit angle in degrees")?
                    .to_string_lossy()
                    .parse()?
            }
            Some("--fps") => {
                fps = args
                    .next()
                    .context("--fps needs a frame rate")?
                    .to_string_lossy()
                    .parse()?
            }
            Some("--duration") => {
                duration = args
                    .next()
                    .context("--duration needs seconds")?
                    .to_string_lossy()
                    .parse()?
            }
            Some("--at") => {
                let time: f64 = args
                    .next()
                    .context("--at needs seconds")?
                    .to_string_lossy()
                    .parse()?;
                let line = args
                    .next()
                    .context("--at needs a console command")?
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("invalid command"))?;
                events.push((time, line));
            }
            Some("--command") => commands.push(
                args.next()
                    .context("--command needs console text")?
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("invalid command"))?,
            ),
            Some("--benchmark") => {
                let n: usize = args
                    .next()
                    .context("--benchmark needs a frame count")?
                    .to_string_lossy()
                    .parse()?;
                anyhow::ensure!(n > 0, "positive sample count required");
                benchmark = Some(n);
            }
            Some("--capture") => {
                capture = Some(PathBuf::from(
                    args.next().context("--capture needs a PNG path")?,
                ))
            }
            Some("--size") => {
                let size = args.next().context("--size needs WIDTHxHEIGHT")?;
                let size = size.to_str().context("invalid image size")?;
                let (w, h) = size.split_once('x').context("--size needs WIDTHxHEIGHT")?;
                let (w, h) = (w.parse::<u32>()?, h.parse::<u32>()?);
                anyhow::ensure!(
                    w > 0 && h > 0 && w.checked_mul(h).is_some(),
                    "invalid image dimensions"
                );
                output_size = Some([w, h]);
            }
            Some("--warmup") => {
                warmup = Some(
                    args.next()
                        .context("--warmup needs a frame count")?
                        .to_string_lossy()
                        .parse::<u32>()?,
                );
            }
            Some("--scene") => {
                scene = Some(PathBuf::from(args.next().context("--scene needs a file")?))
            }
            Some("--experiment") => {
                experiment = args
                    .next()
                    .context("--experiment needs a scene name")?
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("invalid experiment name"))?
            }
            Some("--list-experiments") => {
                for (name, _) in presets::PRESETS {
                    println!("{name}");
                }
                return Ok(());
            }
            Some("--help") => {
                println!(
                    "render-lab [--list-experiments] [--experiment NAME] [--scene FILE] [--command COMMAND] [--capture FILE.png [--size WIDTHxHEIGHT] [--warmup FRAMES] [--secondary-capture FILE.png] | --benchmark FRAMES | --record FILE.mp4|FILE.gif [--size WIDTHxHEIGHT] [--secondary-record FILE.mp4|FILE.gif] [--secondary-yaw DEGREES] [--duration SEC] [--fps N] [--at SEC COMMAND]]"
                );
                return Ok(());
            }
            _ => bail!("unknown option {option:?}"),
        }
    }
    anyhow::ensure!(
        usize::from(capture.is_some())
            + usize::from(benchmark.is_some())
            + usize::from(!recordings.is_empty())
            <= 1,
        "choose capture, benchmark or recording"
    );
    anyhow::ensure!(
        warmup.is_none() || capture.is_some(),
        "--warmup requires --capture"
    );
    anyhow::ensure!(
        output_size.is_none() || capture.is_some() || !recordings.is_empty(),
        "--size applies to captured images and recordings"
    );
    anyhow::ensure!(
        secondary_capture.is_none() || capture.is_some(),
        "a secondary capture needs a distinct primary capture"
    );
    anyhow::ensure!(
        secondary_record.is_none() || !recordings.is_empty(),
        "a secondary recording needs a distinct primary recording"
    );
    validate_outputs(
        capture
            .iter()
            .chain(&secondary_capture)
            .chain(&recordings)
            .chain(&secondary_record),
    )?;
    anyhow::ensure!(secondary_yaw.is_finite(), "invalid secondary camera yaw");
    let output_size = output_size.unwrap_or([800, 500]);
    anyhow::ensure!(
        (1..=100).contains(&fps) && duration.is_finite() && duration > 0.,
        "invalid recording duration or fps"
    );
    anyhow::ensure!(
        events.is_empty() || !recordings.is_empty(),
        "--at requires --record"
    );
    anyhow::ensure!(
        events
            .iter()
            .all(|(t, _)| t.is_finite() && *t >= 0. && *t < duration),
        "command time outside recording"
    );
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let startup = Instant::now();
    let dragon_demo = experiment == "clusters" && scene.is_none();
    let world = presets::load(&experiment, scene.as_deref())?;
    let scene_ms = startup.elapsed().as_secs_f64() * 1000.;
    let init = Instant::now();
    let mut engine = experiments::open(world)?;
    if benchmark.is_some() {
        println!(
            "STARTUP scene_ms={scene_ms:.3} engine_ms={:.3}",
            init.elapsed().as_secs_f64() * 1000.
        );
    }
    if dragon_demo {
        engine.camera.pitch = 0.48;
        engine.camera.distance *= 0.59;
    }
    let mut console = render_lab::console::Console::default();
    if experiment == "forest-karst" {
        console.execute(&mut engine, &[], "post taa")?;
    }
    for line in commands {
        println!("{}", console.execute(&mut engine, &[], &line)?);
    }
    if !recordings.is_empty() {
        let mut encoders = recordings
            .iter()
            .map(|p| render_lab::recording::Encoder::open(p, output_size, fps))
            .collect::<Result<Vec<_>>>()?;
        let mut second_encoder = secondary_record
            .as_ref()
            .map(|p| render_lab::recording::Encoder::open(p, output_size, fps))
            .transpose()?;
        let mut events = events.into_iter().peekable();
        let n = (duration * fps as f64).ceil() as u64;
        let mut previous_clip = String::new();
        for i in 0..n {
            let time = i as f64 / fps as f64;
            while events.peek().is_some_and(|(t, _)| *t <= time) {
                println!(
                    "{time:.2}s {}",
                    console.execute(&mut engine, &[], &events.next().unwrap().1)?
                );
            }
            let (frame, secondary) = read_views(
                &mut engine,
                output_size,
                second_encoder.as_ref().map(|_| secondary_yaw),
                if i == 0 || console.paused {
                    0.
                } else {
                    1. / fps as f32
                },
            )?;
            if let (Some(encoder), Some(view)) = (&mut second_encoder, secondary) {
                encoder.write(&view)?;
            }
            for encoder in &mut encoders {
                encoder.write(&frame)?;
            }
            if let Some(state) = motion::silver::status(&engine.world)?
                && state.clip != previous_clip
            {
                println!(
                    "{time:.2}s matched {} at sample {}",
                    state.clip, state.sample
                );
                previous_clip = state.clip;
            }
        }
        for encoder in encoders.into_iter().chain(second_encoder) {
            encoder.finish()?;
        }
        println!("Recorded {n} frames at {fps} fps");
    } else if let Some(path) = capture {
        // Capture the same reproducible animated pose on every run.
        for _ in 0..12 {
            engine
                .schedule
                .tick(&mut engine.world, 1. / 60., &engine.tasks)?;
        }
        let orbit = secondary_capture.as_ref().map(|_| secondary_yaw);
        for i in 0..warmup.unwrap_or(1) {
            let (frame, _) = read_views(&mut engine, output_size, orbit, 1. / 60.)?;
            if i == 0 {
                println!(
                    "First frame pipelines={} create_ms={:.3}",
                    frame.profile.pipeline_creations, frame.profile.pipeline_create_ms
                );
            }
        }
        let (f, secondary) = read_views(&mut engine, output_size, orbit, 0.)?;
        if let (Some(p), Some(s)) = (secondary_capture, secondary) {
            save_capture(p, s.extent, s.pixels)?;
        }
        println!(
            "Capture pipelines={} create_ms={:.3} culled_passes={}",
            f.profile.pipeline_creations, f.profile.pipeline_create_ms, f.profile.culled_passes
        );
        println!("{:?}", f.counters);
        save_capture(path, f.extent, f.pixels)?;
        println!("{}", f.gpu);
        for p in &f.passes {
            let parents = p
                .parents
                .iter()
                .map(|&i| f.passes[i].name.as_str())
                .collect::<Vec<_>>();
            println!("{}   {:.3} ms   inputs {:?}", p.name, p.ms, parents);
        }
    } else {
        ui::run(engine, console, benchmark);
    }
    Ok(())
}

fn validate_outputs<'a>(outputs: impl Iterator<Item = &'a PathBuf>) -> Result<()> {
    use std::{collections::BTreeSet, path::Path};
    let mut seen = BTreeSet::new();
    for path in outputs {
        let file = path.file_name().context("output needs a filename")?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let resolved = if path.exists() {
            path.canonicalize()?
        } else {
            parent.canonicalize()?.join(file)
        };
        #[cfg(windows)]
        let resolved = PathBuf::from(resolved.to_string_lossy().to_lowercase());
        anyhow::ensure!(
            seen.insert(resolved),
            "multiple outputs resolve to {}",
            path.display()
        );
    }
    Ok(())
}

fn save_capture(path: PathBuf, extent: [u32; 2], mut pixels: Vec<u8>) -> Result<()> {
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    image::save_buffer(path, &pixels, extent[0], extent[1], image::ColorType::Rgba8)?;
    Ok(())
}

fn read_views(
    engine: &mut render_lab::engine::Engine,
    size: [u32; 2],
    orbit_degrees: Option<f32>,
    dt: f32,
) -> Result<(render_lab::rhi::Frame, Option<render_lab::rhi::Frame>)> {
    let secondary = orbit_degrees.map(|yaw| {
        let mut camera = engine.camera;
        camera.yaw += yaw.to_radians();
        let mut post = render_lab::render::post::Settings::default();
        post.select(render_lab::render::post::Mode::Taa);
        render_lab::engine::OffscreenView {
            camera,
            extent: size,
            post,
        }
    });
    anyhow::ensure!(
        engine.tick_with_view(dt, size, secondary)?,
        "capture queue unexpectedly full"
    );
    let start = Instant::now();
    let mut primary = None;
    let mut other = None;
    loop {
        if let Some(result) = engine.take_frame() {
            primary = Some(result.map_err(anyhow::Error::msg)?);
        }
        if let Some(result) = engine.take_secondary_frame() {
            other = Some(result.map_err(anyhow::Error::msg)?);
        }
        if primary.is_some() && (secondary.is_none() || other.is_some()) {
            return Ok((primary.take().unwrap(), other));
        }
        anyhow::ensure!(
            start.elapsed() < Duration::from_secs(120),
            "frame timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;

    #[test]
    fn output_aliases_cannot_overwrite_another_view() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let first = dir.path().join("frame.png");
        let second = dir.path().join("nested/../frame.png");
        assert!(validate_outputs([&first, &second].into_iter()).is_err());
        #[cfg(windows)]
        assert!(validate_outputs([&first, &dir.path().join("FRAME.png")].into_iter()).is_err());
        Ok(())
    }
}
