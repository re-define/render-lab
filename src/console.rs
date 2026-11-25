//! Shared debug commands for the GPUI console and integration tests.
use crate::{
    engine::Engine,
    experiments::{
        self, cartoon, clusters, gaussian, ibl, lighting, motion, restir, texture, vegetation,
    },
    import,
    render::{
        Camera,
        post::{DebugView, Mode},
    },
    rhi::PassReport,
    scene::World,
};
use anyhow::{Context, Result, bail, ensure};
use std::{path::Path, sync::Arc};
mod lights;
mod memory;

const HELP: &str = concat!(
    "fit · pause · rdg · timeline FILE.json · hud on/off · post off/taa/dlss/dlaa/reset/status",
    " · post view motion/reprojection/off · stats · schedule · memory idle/alloc/driver [NAME or #OWNER]",
    " · light list/point/sun/move/aim/power/color/enable/angular-radius · import/save/load PATH · lod PIXELS",
    " · lodview on/off · vegetation auto/triangles/voxels/pixels/wind/time/view/camera",
    " · motion SPEED TURN [STRAFE] · motionsearch kd/scan · record PATH · stoprecord",
    " · contact status/physics/auto/manual/target/hand/feet/view/action · contact gravity G · contact impulse J",
    " · toon outline/width/light/threshold/sdf/view · texture neural/original/stats",
    " · gaussian tile/bvh/compact-tile/compact-bvh/scan/original/stats",
    " · gi off/ssgi/probes/ray/view/camera · shadow hard/pcf/pcss/ray/size/bias/view/camera",
    " · rt view/camera",
    " · restir uniform/ris/reuse/reference/denoise/source/lights/candidates/samples/ev/camera/history-stats",
    " · ibl ev/yaw/view/sky · effect restart/time SECONDS",
    " · rays hardware/software/normals/distance/off",
    " · feature forest/water/actor/ibl/restir on/off",
);

pub struct Console {
    pub recording: Option<crate::recording::LiveRecording>,
    pub paused: bool,
    pub show_graph: bool,
    pub show_hud: bool,
}
impl Default for Console {
    fn default() -> Self {
        Self {
            recording: None,
            paused: false,
            show_graph: true,
            show_hud: true,
        }
    }
}
impl Console {
    pub fn execute(
        &mut self,
        engine: &mut Engine,
        passes: &[PassReport],
        line: &str,
    ) -> Result<String> {
        let line = line.trim();
        let (verb, arg) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let arg = arg.trim().trim_matches('"');
        match verb {
            "help" => return Ok(HELP.into()),
            "timeline" => {
                ensure!(!arg.is_empty(), "timeline FILE.json");
                crate::rhi::timeline::export(passes, Path::new(arg))?;
                return Ok(format!("GPU timeline saved to {arg}"));
            }
            "memory" => return memory::describe(engine.last_memory().as_ref(), arg),
            "contact" => return motion::silver::contact::command(engine, arg),
            "schedule" => {
                ensure!(arg.is_empty(), "schedule takes no arguments");
                return Ok(engine.schedule.describe());
            }
            "record" => {
                ensure!(self.recording.is_none(), "a recording is already running");
                self.recording = Some(crate::recording::LiveRecording::new(Path::new(arg))?);
                engine.capture_viewport = true;
            }
            "stoprecord" => {
                engine.capture_viewport = false;
                self.recording
                    .take()
                    .context("no recording is running")?
                    .finish()?;
            }
            "fit" => {
                engine.camera = Camera::fit(&engine.world)?;
                engine.post.reset();
            }
            "pause" => self.paused = !self.paused,
            "rdg" => self.show_graph = !self.show_graph,
            "hud" => {
                self.show_hud = match arg {
                    "on" => true,
                    "off" => false,
                    "" => !self.show_hud,
                    _ => bail!("hud on/off"),
                }
            }
            "post" => match arg {
                "" | "status" => {
                    let dlss = engine.dlss_unavailable().unwrap_or("initialized");
                    return Ok(format!(
                        "Post-processing {:?} · View {:?} · NVIDIA NGX {dlss}",
                        engine.post.mode(),
                        engine.post.view()
                    ));
                }
                "off" => engine.select_post(Mode::Off)?,
                "taa" => engine.select_post(Mode::Taa)?,
                "dlss" | "dlaa" => {
                    let mode = if arg == "dlss" {
                        Mode::Dlss
                    } else {
                        Mode::Dlaa
                    };
                    engine.select_post(mode)?;
                    return Ok(if mode == Mode::Dlss {
                        "NVIDIA DLSS Quality selected"
                    } else {
                        "NVIDIA DLAA selected"
                    }
                    .into());
                }
                "reset" => engine.post.reset(),
                "view motion" => engine.post.select_view(DebugView::Motion),
                "view reprojection" => engine.post.select_view(DebugView::Reprojection),
                "view off" => engine.post.select_view(DebugView::Off),
                _ => {
                    bail!("post off/taa/dlss/dlaa/reset/status · post view motion/reprojection/off")
                }
            },
            "stats" => {
                let total = passes.iter().map(|p| p.ms).sum::<f64>();
                return Ok(format!(
                    "{} entities · {} passes · {total:.3} ms GPU scopes",
                    engine.world.entities.len(),
                    passes.len()
                ));
            }
            "save" => engine.world.save(Path::new(arg))?,
            "load" => {
                let world = World::load(Path::new(arg))?;
                experiments::replace(engine, world)?;
            }
            "import" => {
                import::import(Arc::make_mut(&mut engine.world), Path::new(arg))?;
                engine.camera = Camera::fit(&engine.world)?;
                engine.post.reset();
            }
            "lodview" => {
                let e = Arc::make_mut(&mut engine.world)
                    .entities
                    .values_mut()
                    .find(|e| e.components.contains_key(clusters::SETTING))
                    .context("scene has no cluster geometry settings")?;
                let enabled = match arg {
                    "on" => true,
                    "off" => false,
                    "" => !e
                        .components
                        .get(clusters::VISUALIZE)
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    _ => bail!("lodview on/off"),
                };
                e.components
                    .insert(clusters::VISUALIZE.into(), enabled.into());
                return Ok(format!(
                    "LOD colors {} · L0 green / L1 blue / L2 yellow / L3 orange / L4 violet",
                    if enabled { "on" } else { "off" }
                ));
            }
            "lod" => {
                let pixels: f32 = arg.parse()?;
                ensure!(
                    pixels.is_finite() && pixels >= 0.,
                    "expected nonnegative pixel error"
                );
                let settings = Arc::make_mut(&mut engine.world)
                    .entities
                    .values_mut()
                    .find_map(|e| e.components.get_mut(clusters::SETTING))
                    .context("no cluster LOD settings in this scene")?;
                *settings = pixels.into();
            }
            "rays" => {
                ensure!(
                    matches!(
                        arg,
                        "hardware" | "software" | "normals" | "distance" | "off"
                    ),
                    "rays hardware/software/normals/distance/off"
                );
                let world = Arc::make_mut(&mut engine.world);
                ensure!(
                    arg != "hardware" || engine.ray_caps.pipeline,
                    "hardware inspection requires VK_KHR_ray_tracing_pipeline"
                );
                let key = crate::render::rays::VIEW;
                let id = world
                    .first_raw_component(key)
                    .map(|p| p.0)
                    .unwrap_or_else(|| {
                        world.spawn(crate::scene::Entity {
                            name: "Ray inspection".into(),
                            ..Default::default()
                        })
                    });
                world.set_setting(id, key, arg.into());
                engine.post.reset();
                return Ok(format!("Ray inspection {arg}"));
            }
            "feature" => return experiments::composition::command(engine, arg),
            "effect" => return experiments::effects::command(engine, arg),
            "toon" => return cartoon::command(engine, arg),
            "light" => return lights::command(engine, arg),
            "vegetation" => return vegetation::command(engine, arg),
            "gaussian" => return gaussian::command(engine, arg),
            "texture" => return texture::command(engine, arg),
            "gi" => return lighting::command(engine, lighting::Kind::Gi, arg),
            "shadow" => return lighting::command(engine, lighting::Kind::Shadow, arg),
            "rt" => return lighting::command(engine, lighting::Kind::Rt, arg),
            "ibl" => return ibl::command(engine, arg),
            "restir" => return restir::command(engine, arg),
            "motionsearch" => motion::silver::set_search(Arc::make_mut(&mut engine.world), arg)?,
            "motion" => {
                let values: Vec<f32> = arg
                    .split_whitespace()
                    .map(str::parse)
                    .collect::<Result<_, _>>()?;
                if engine
                    .world
                    .first_raw_component(motion::silver::PLAYER)
                    .is_some()
                {
                    ensure!(
                        (2..=3).contains(&values.len()),
                        "motion SPEED TURN [STRAFE]"
                    );
                    motion::silver::set_goal(
                        Arc::make_mut(&mut engine.world),
                        values[0],
                        values[1],
                        values.get(2).copied().unwrap_or(0.),
                    )?;
                } else {
                    ensure!(values.len() == 2, "motion SPEED TURN");
                    motion::set_goal(Arc::make_mut(&mut engine.world), values[0], values[1])?;
                }
            }
            _ => bail!("Unknown command '{verb}'. Type help."),
        }
        Ok(format!("{line} ✓"))
    }
}
