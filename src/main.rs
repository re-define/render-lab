mod cli;
mod presets;
mod ui;
use anyhow::Result;
use cli::{Options, Output};
use render_lab::{
    experiments::{self, motion},
    recording::save_capture,
};
use std::time::{Duration, Instant};

fn main() -> Result<()> {
    let Some(Options {
        experiment,
        scene,
        commands,
        output,
    }) = cli::parse(std::env::args_os().skip(1))?
    else {
        return Ok(());
    };
    let startup = Instant::now();
    let dragon_demo = experiment == "clusters" && scene.is_none();
    let world = presets::load(&experiment, scene.as_deref())?;
    let scene_ms = startup.elapsed().as_secs_f64() * 1000.;
    let init = Instant::now();
    let mut engine = experiments::open(world)?;
    if matches!(output, Output::Window { benchmark: Some(_) }) {
        println!(
            "STARTUP scene_ms={scene_ms:.3} engine_ms={:.3}",
            init.elapsed().as_secs_f64() * 1000.
        );
    }
    if dragon_demo {
        engine.camera.pitch = 0.48;
        engine.camera.distance *= 0.59;
    }
    engine
        .audio
        .set_scene_cues(matches!(output, Output::Window { benchmark: None }));
    let mut console = render_lab::console::Console::default();
    if experiment == "forest-karst" {
        console.execute(&mut engine, &[], "post taa")?;
    }
    for line in commands {
        println!("{}", console.execute(&mut engine, &[], &line)?);
    }
    match output {
        Output::Record {
            paths: recordings,
            secondary: secondary_record,
            size: output_size,
            yaw: secondary_yaw,
            duration,
            fps,
            events,
        } => {
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
        }
        Output::Capture {
            path,
            secondary: secondary_capture,
            size: output_size,
            yaw: secondary_yaw,
            warmup,
        } => {
            // Capture the same reproducible animated pose on every run.
            for _ in 0..12 {
                engine
                    .schedule
                    .tick(&mut engine.world, 1. / 60., &engine.tasks)?;
            }
            let orbit = secondary_capture.as_ref().map(|_| secondary_yaw);
            for i in 0..warmup {
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
        }
        Output::Window { benchmark } => ui::run(engine, console, benchmark),
    }
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
