//! Fixed receiver/light/camera; edit one Silver Wolf joint and compare both ray backends.
use anyhow::{Context, Result, ensure};
use glam::{Mat4, Vec3};
use render_lab::{
    console::Console,
    experiments,
    render::{Camera, SceneExtractor},
    rhi::Backend,
};
use std::{path::PathBuf, sync::Arc};

fn main() -> Result<()> {
    let dir = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("output directory required")?,
    );
    std::fs::create_dir_all(&dir)?;
    let mut e = experiments::open(experiments::composition::scene()?)?;
    let mut console = Console::default();
    // The stone platform remains as a fixed receiver; isolate the character's shadow.
    for cmd in [
        "feature forest off",
        "feature water off",
        "feature restir off",
    ] {
        console.execute(&mut e, &[], cmd)?;
    }
    e.camera = Camera {
        target: Vec3::new(1., 1.0, 2.4),
        distance: 3.5,
        yaw: 0.34,
        pitch: 0.25,
    };
    let (arm, bind) = e
        .world
        .entities
        .iter()
        .find(|(_, v)| v.name == "Right arm_059")
        .map(|(&id, v)| (id, v.local))
        .context("Silver Wolf arm joint")?;
    let mut extraction = SceneExtractor::default();
    let mut hardware = Backend::open()?;
    let mut software = Backend::open()?;
    let mut distances = vec![];
    let mut shadows = vec![];
    for (label, rotation) in [("rest", 0.), ("raised", 1.13)] {
        Arc::make_mut(&mut e.world).set_local(arm, bind * Mat4::from_rotation_z(rotation))?;
        let mut reference = Vec::new();
        for mode in ["hardware", "software", "distance"] {
            console.execute(&mut e, &[], &format!("rays {mode}"))?;
            let view = extraction.snapshot(&e.world, e.camera, [480, 360], e.materials.clone())?;
            let graph = e.technique.build(&view)?;
            let device = if mode == "software" {
                &mut software
            } else {
                &mut hardware
            };
            let frame = device.execute(graph.compile()?, &e.tasks)?;
            if mode == "hardware" {
                println!(
                    "{label} native builds={} updates={}",
                    frame.profile.acceleration_builds, frame.profile.acceleration_updates
                );
                if label == "raised" {
                    ensure!(
                        frame.profile.acceleration_updates > 0,
                        "no native AS update"
                    );
                }
                reference = frame.pixels.clone();
                shadows.push(frame.pixels.clone());
            } else if mode == "distance" {
                distances.push(frame.pixels.clone());
            } else {
                let diff = frame
                    .pixels
                    .chunks_exact(4)
                    .zip(reference.chunks_exact(4))
                    .filter(|(a, b)| a != b)
                    .count();
                println!("{label} software/native differing pixels={diff}");
                ensure!(diff < 480 * 360 / 100, "ray backends disagree");
            }
            if mode == "hardware" {
                render_lab::recording::save_capture(
                    dir.join(format!("silver-arm-shadow-{label}.png")),
                    frame.extent,
                    frame.pixels,
                )?;
            }
        }
    }
    let mut changed = 0;
    let mut on_receiver = 0;
    for i in (0..shadows[0].len()).step_by(4) {
        if shadows[0][i] != shadows[1][i] {
            changed += 1;
            if distances[0][i] == distances[1][i]
                && [33, 217].contains(&shadows[0][i])
                && [33, 217].contains(&shadows[1][i])
            {
                on_receiver += 1;
            }
        }
    }
    println!(
        "changed pixels={changed}; shadow changes at equal quantized primary depth={on_receiver}"
    );
    ensure!(
        changed > 60 && on_receiver > 10,
        "joint edit did not move the shadow"
    );
    Ok(())
}
