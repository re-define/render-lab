mod ui;
use anyhow::{Context, Result, bail};
use render_lab::{
    experiments::{
        self, cartoon, clusters, gaussian, ibl, lighting, mixed, motion, restir, texture,
        vegetation,
    },
    import,
    scene::World,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let mut capture = None;
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
            Some("--help") => {
                println!(
                    "render-lab [--experiment clusters|vegetation|vegetation-scale|river-forest|forest-karst|confluence|water|destruction|motion|toon|texture|gaussian|gi|shadow|rt|restir|forest-mega|gi12-forest|gi12-silver|ibl|mixed] [--scene FILE] [--command COMMAND] [--capture FILE.png [--size WIDTHxHEIGHT] [--warmup FRAMES] | --benchmark FRAMES | --record FILE.mp4|FILE.gif [--size WIDTHxHEIGHT] [--duration SEC] [--fps N] [--at SEC COMMAND]]"
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
    let world = match (experiment.as_str(), scene) {
        ("", None) => World::demo(),
        ("clusters", None) => clusters::scene()?,
        ("confluence", None) => experiments::composition::scene()?,
        ("water", None) => experiments::effects::water()?,
        ("destruction", None) => experiments::effects::destruction()?,
        ("vegetation", None) => vegetation::scene()?,
        ("vegetation-scale", None) => vegetation::forest()?,
        ("river-forest", None) => vegetation::river_forest()?,
        ("motion", None) => motion::silver::scene()?,
        ("toon", None) => cartoon::scene()?,
        ("gaussian", path) => gaussian::scene(path.as_deref())?,
        ("texture", None) => texture::scene()?,
        ("gi", None) => lighting::scene(lighting::Kind::Gi)?,
        ("shadow", None) => lighting::scene(lighting::Kind::Shadow)?,
        ("rt", None) => lighting::scene(lighting::Kind::Rt)?,
        ("ibl", None) => ibl::scene()?,
        ("restir", None) => restir::scene()?,
        ("gi12-forest" | "forest-mega", None) => restir::forest_study()?,
        ("forest-karst", None) => restir::karst_study()?,
        ("gi12-silver", None) => restir::silver_study()?,
        ("mixed", None) => mixed::scene()?,
        ("toon", Some(p)) => {
            if p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("json"))
            {
                let w = World::load(&p)?;
                anyhow::ensure!(
                    w.first_raw_component(cartoon::SETTING).is_some(),
                    "scene has no cartoon settings"
                );
                w
            } else {
                cartoon::from_gltf(&p)?
            }
        }
        (kind @ ("" | "clusters"), Some(p)) => {
            let mut w = if p
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("json"))
            {
                World::load(&p)?
            } else {
                let mut w = World::default();
                import::import(&mut w, &p)?;
                w
            };
            if kind == "clusters" {
                clusters::prepare(&mut w)?;
            }
            w
        }
        _ => bail!("use --help for experiment names and scene options"),
    };
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
    if experiment == "forest-mega" {
        console.execute(&mut engine, &[], "vegetation shadows mega")?;
        console.execute(&mut engine, &[], "restir gi direct")?;
    }
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
            let frame = read_frame(
                &mut engine,
                output_size,
                if i == 0 || console.paused {
                    0.
                } else {
                    1. / fps as f32
                },
            )?;
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
        for encoder in encoders {
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
        for _ in 0..warmup.unwrap_or(1) {
            read_frame(&mut engine, output_size, 1. / 60.)?;
        }
        let f = read_frame(&mut engine, output_size, 0.)?;
        println!("{:?}", f.counters);
        let mut rgba = f.pixels;
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        image::save_buffer(
            path,
            &rgba,
            f.extent[0],
            f.extent[1],
            image::ColorType::Rgba8,
        )?;
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

fn read_frame(
    engine: &mut render_lab::engine::Engine,
    size: [u32; 2],
    dt: f32,
) -> Result<render_lab::rhi::Frame> {
    anyhow::ensure!(engine.tick(dt, size)?, "capture queue unexpectedly full");
    let start = Instant::now();
    loop {
        if let Some(result) = engine.take_frame() {
            return result.map_err(anyhow::Error::msg);
        }
        anyhow::ensure!(
            start.elapsed() < Duration::from_secs(120),
            "frame timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
