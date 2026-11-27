//! End-to-end GPU upload audit with deterministic edits and a full CPU packing reference.
mod support;
use anyhow::{Context, Result, ensure};
use render_lab::{
    console::Console,
    engine::Engine,
    experiments::{self, clusters::ClusterLod, vegetation},
    rhi::Frame,
};
use std::{
    io::{BufWriter, Write},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
fn frame(engine: &mut Engine) -> Result<Frame> {
    ensure!(engine.tick(0., [800, 500])?, "frame queue full");
    let start = Instant::now();
    support::wait_frame(
        engine,
        start + Duration::from_secs(90),
        Duration::from_millis(1),
    )
}
fn main() -> Result<()> {
    let output = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("output CSV path required")?,
    );
    let mut csv = BufWriter::new(std::fs::File::create(&output)?);
    writeln!(
        csv,
        "scenario,path,sample,extract_entities,updated_instances,render_visits,dynamic_bytes,table_bytes,extract_us,build_ms,gpu_ms,pixel_differences,rgb_mae"
    )?;
    let forest = vegetation::forest()?;
    let tree = forest
        .entities
        .values()
        .find(|e| e.name == "Forest tree")
        .unwrap()
        .local;
    let parts: Vec<_> = forest
        .entities
        .iter()
        .filter(|(_, e)| e.name == "Forest tree" && e.local == tree)
        .map(|(&id, _)| id)
        .collect();
    ensure!(parts.len() == 2, "expected a whole tree");
    let bark = forest.entities[&parts[0]]
        .instance
        .as_ref()
        .unwrap()
        .material
        .unwrap();
    let source = forest.entities[&bark].material.clone().unwrap();
    for scenario in ["camera", "tree", "bark"] {
        let mut reference = Vec::new();
        for full in [true, false] {
            let mut engine = experiments::open(forest.clone())?;
            if full {
                engine.technique = Arc::new(ClusterLod::full_upload_reference());
            }
            let mut console = Console::default();
            for command in [
                "vegetation wind 0",
                "vegetation time 0",
                "vegetation camera near",
                "post off",
            ] {
                console.execute(&mut engine, &[], command)?;
            }
            let camera = engine.camera;
            for step in 0..72 {
                let phase = (step % 7) as f32 / 6.;
                let world = Arc::make_mut(&mut engine.world);
                match scenario {
                    "camera" => engine.camera.yaw = camera.yaw + phase * 0.018,
                    "tree" => {
                        for &id in &parts {
                            let mut model = tree;
                            model.w_axis.x += phase * 0.35;
                            world.set_local(id, model)?;
                        }
                    }
                    _ => {
                        let mut m = source.clone();
                        m.base_color[0] = source.base_color[0] * (1. + phase * 0.9);
                        world.set_material(bark, m)?;
                    }
                }
                let f = frame(&mut engine)?;
                if step < 8 {
                    continue;
                }
                ensure!(f.counters["Triangle plant clusters"] > 0, "empty forest");
                let p = &f.profile;
                let sample = step - 8;
                let (mut different, mut error) = (0usize, 0u64);
                if full {
                    reference.push(f.pixels);
                } else {
                    if scenario == "bark" && phase == 1. && sample < 7 {
                        for (label, pixels) in
                            [("full", &reference[sample]), ("resident", &f.pixels)]
                        {
                            render_lab::recording::save_capture(
                                output.with_file_name(format!("forest-bark-{label}.png")),
                                f.extent,
                                pixels.clone(),
                            )?;
                        }
                    }
                    for (a, b) in f
                        .pixels
                        .chunks_exact(4)
                        .zip(reference[sample].chunks_exact(4))
                    {
                        different += usize::from(a != b);
                        error += (0..3).map(|i| u64::from(a[i].abs_diff(b[i]))).sum::<u64>();
                    }
                }
                let mae = error as f64 / (800. * 500. * 3.);
                ensure!(mae < 0.15, "{scenario} sample {sample} diverged: {mae}");
                writeln!(
                    csv,
                    "{scenario},{},{sample},{},{},{},{},{},{:.3},{:.5},{:.5},{different},{mae:.6}",
                    if full { "full" } else { "resident" },
                    p.extracted_entities,
                    p.updated_instances,
                    p.geometry_instance_visits,
                    p.dynamic_upload_bytes,
                    p.scene_table_upload_bytes,
                    p.extract_ms * 1000.,
                    p.build_ms,
                    p.gpu_span_ms.unwrap_or(0.)
                )?;
            }
            eprintln!(
                "{scenario} {} complete",
                if full { "full" } else { "resident" }
            );
            csv.flush()?;
        }
    }
    Ok(())
}
