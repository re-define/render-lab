//! Deterministic moving-character comparison against direct lighting from every light.
mod support;
use anyhow::{Context, Result, ensure};
use glam::{Mat4, Vec3};
use render_lab::{
    experiments::{self, cartoon, motion::silver, restir},
    scene::{Entity, Light, Shading},
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let output = PathBuf::from(args.next().context("expected frame output directory")?);
    let mode = args.next().unwrap_or_else(|| "reuse".into());
    if matches!(mode.as_str(), "ibl" | "direct" | "combined") {
        use render_lab::experiments::{ibl, mixed};
        let mut world = mixed::scene()?;
        let camera = experiments::open(world.clone())?.camera;
        for entity in world.entities.values_mut() {
            if mode == "ibl" {
                entity.components.remove(restir::SETTING);
            }
            if mode == "direct" {
                entity.components.remove(ibl::SETTING);
            }
        }
        let mut engine = experiments::open(world)?;
        engine.camera = camera;
        std::fs::create_dir_all(&output)?;
        for i in 0..9 {
            capture(&mut engine, [800, 500], 0., &output, i, i == 8)?;
        }
        return Ok(());
    }
    let reference = match mode.as_str() {
        "reference" => true,
        "reuse" => false,
        _ => anyhow::bail!("expected reuse/reference or ibl/direct/combined"),
    };
    std::fs::create_dir_all(&output)?;
    let mut world = silver::scene()?;
    let mut camera = cartoon::camera(&world, "full")?;
    camera.distance *= 1.30;
    camera.target.x += 0.30;
    for entity in world.entities.values_mut() {
        if let Some(material) = &mut entity.material {
            material.shading = Shading::Custom("Many-light diffuse".into());
        }
    }
    for i in 0..32 {
        let angle = i as f32 * 2.3999631;
        let position = Vec3::new(
            2.0 * angle.cos(),
            0.45 + (i % 5) as f32 * 0.52,
            2.0 * angle.sin(),
        );
        let tint = if i % 3 == 0 {
            [1.0, 0.52, 0.26]
        } else {
            [0.48, 0.74, 1.0]
        };
        world.spawn(Entity {
            light: Some(Light::point(
                tint,
                if i % 7 == 0 { 2.5 } else { 0.16 },
                10.0,
            )),
            local: Mat4::from_translation(position),
            ..Default::default()
        });
    }
    world.spawn(Entity { components: [(restir::SETTING.into(), serde_json::json!({
        "history_stats":true,"lights":32,"candidates":4,"reference_samples":32,"ev":0.0,"light_source":"scene",
        "sampling":if reference {"reference"} else {"restir"}, "denoise":if reference {"off"} else {"svgf"}
    }))].into(), ..Default::default() });
    silver::set_goal(&mut world, 0.35, 0., 0.9)?;
    let mut engine = experiments::open(world)?;
    engine.camera = camera;
    let extent = [400, 320];
    for index in 0..32 {
        capture(&mut engine, extent, 1. / 30., &output, index, true)?;
    }
    Ok(())
}

fn capture(
    engine: &mut render_lab::engine::Engine,
    extent: [u32; 2],
    dt: f32,
    output: &std::path::Path,
    index: usize,
    save: bool,
) -> Result<()> {
    ensure!(engine.tick(dt, extent)?, "frame was not queued");
    let start = Instant::now();
    let frame = support::wait_frame(
        engine,
        start + Duration::from_secs(90),
        Duration::from_micros(200),
    )?;
    if index == 16 {
        println!(
            "{}; {:?}; {:?}",
            frame.gpu,
            silver::status(&engine.world)?.map(|p| p.clip),
            frame.counters
        );
        for pass in &frame.passes {
            let parents: Vec<_> = pass
                .parents
                .iter()
                .map(|&p| frame.passes[p].name.as_str())
                .collect();
            println!("{} <- {:?}", pass.name, parents);
        }
    }
    if !save {
        return Ok(());
    }
    render_lab::recording::save_capture(
        output.join(format!("{index:03}.png")),
        frame.extent,
        frame.pixels,
    )?;
    Ok(())
}
