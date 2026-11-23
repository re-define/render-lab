//! Same geometry, wind phase and deterministic solar disk samples on both shadow backends.
use super::{frame, zoom::camera_at};
use anyhow::{Context, Result};
use render_lab::{console::Console, experiments};
use std::path::Path;

pub(super) fn run(directory: &Path) -> Result<()> {
    const EXTENT: [u32; 2] = [1920, 1080];
    std::fs::create_dir_all(directory)?;
    let world = experiments::restir::karst_study()?;
    let sun = world
        .entities
        .iter()
        .find(|(_, e)| e.name == "Late afternoon sun")
        .context("missing karst sun")?
        .0;
    for backend in ["software", "hardware"] {
        let mut engine = experiments::open(world.clone())?;
        let mut console = Console::default();
        for command in [
            "vegetation shadows adaptive",
            "vegetation raster compute",
            "restir gi direct",
            "restir reference",
            "restir denoise off",
            "post off",
            "vegetation time 1.7",
        ] {
            console.execute(&mut engine, &[], command)?;
        }
        console.execute(&mut engine, &[], &format!("restir proxy-rays {backend}"))?;
        let origin = engine.camera;
        for (view, steps) in [("overview", 0.), ("close", 72.), ("distant", -36.)] {
            engine.camera = camera_at(origin, steps);
            for (disk, radius) in [("hard", 0.), ("soft", 0.27)] {
                console.execute(
                    &mut engine,
                    &[],
                    &format!("light angular-radius {sun} {radius}"),
                )?;
                let image = frame(&mut engine, 0., EXTENT)?;
                let mut rgba = image.pixels;
                for pixel in rgba.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                }
                image::save_buffer(
                    directory.join(format!("{view}-{disk}-{backend}.png")),
                    &rgba,
                    EXTENT[0],
                    EXTENT[1],
                    image::ColorType::Rgba8,
                )?;
            }
        }
    }
    Ok(())
}
