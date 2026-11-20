//! Shared debug commands for the GPUI console and integration tests.
use crate::{
    engine::Engine,
    experiments::{self, cartoon, clusters, gaussian, motion, texture},
    import,
    render::Camera,
    rhi::PassReport,
    scene::World,
};
use anyhow::{Context, Result, bail, ensure};
use std::{path::Path, sync::Arc};

pub struct Console {
    pub recording: Option<crate::recording::LiveRecording>,
    pub paused: bool,
    pub show_graph: bool,
}
impl Default for Console {
    fn default() -> Self {
        Self {
            recording: None,
            paused: false,
            show_graph: true,
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
            "help" => return Ok("fit · pause · rdg · stats · import/save/load PATH · lod PIXELS · lodview on/off · motion SPEED TURN [STRAFE] · motionsearch kd/scan · record PATH · stoprecord · toon outline/width/light/threshold/sdf/view · texture neural/original/stats · gaussian tile/bvh/compact-tile/compact-bvh/scan/original/stats".into()),
            "record" => {
                ensure!(self.recording.is_none(),"a recording is already running");
                self.recording=Some(crate::recording::LiveRecording::new(Path::new(arg))?);
                engine.capture_viewport=true;
            }
            "stoprecord" => {
                engine.capture_viewport=false;
                self.recording.take().context("no recording is running")?.finish()?;
            }
            "fit" => engine.camera=Camera::fit(&engine.world)?,
            "pause" => self.paused = !self.paused,
            "rdg" => self.show_graph = !self.show_graph,
            "stats" => {
                let total=passes.iter().map(|p|p.ms).sum::<f64>();
                return Ok(format!("{} entities · {} passes · {total:.3} ms GPU scopes",engine.world.entities.len(),passes.len()));
            }
            "save" => engine.world.save(Path::new(arg))?,
            "load" => {
                let world=World::load(Path::new(arg))?;
                engine.camera=Camera::fit(&world)?;
                engine.world=Arc::new(world);
                experiments::configure(engine)?;
            }
            "import" => {
                import::import(Arc::make_mut(&mut engine.world),Path::new(arg))?;
                engine.camera=Camera::fit(&engine.world)?;
            }
            "lodview" => {
                let e=Arc::make_mut(&mut engine.world).entities.values_mut().find(|e|e.components.contains_key(clusters::SETTING)).context("scene has no cluster geometry settings")?;
                let enabled=match arg{"on"=>true,"off"=>false,""=>!e.components.get(clusters::VISUALIZE).and_then(|v|v.as_bool()).unwrap_or(false),_=>bail!("lodview on/off")};
                e.components.insert(clusters::VISUALIZE.into(),enabled.into());
                return Ok(format!("LOD colors {} · L0 green / L1 blue / L2 yellow / L3 orange / L4 violet",if enabled{"on"}else{"off"}));
            }
            "lod" => {
                let pixels:f32=arg.parse()?;
                ensure!(pixels.is_finite() && pixels>=0.,"expected nonnegative pixel error");
                let settings=Arc::make_mut(&mut engine.world).entities.values_mut()
                    .find_map(|e|e.components.get_mut(clusters::SETTING)).context("no cluster LOD settings in this scene")?;
                *settings=pixels.into();
            }
            "toon" => return cartoon::command(engine,arg),
            "gaussian" => return gaussian::command(engine,arg),
            "texture" => return texture::command(engine,arg),
            "motionsearch" => motion::silver::set_search(Arc::make_mut(&mut engine.world), arg)?,
            "motion" => {
                let values:Vec<f32>=arg.split_whitespace().map(str::parse).collect::<Result<_,_>>()?;
                if engine.world.entities.values().any(|e|e.components.contains_key(motion::silver::PLAYER)){
                    ensure!((2..=3).contains(&values.len()),"motion SPEED TURN [STRAFE]");
                    motion::silver::set_goal(Arc::make_mut(&mut engine.world),values[0],values[1],values.get(2).copied().unwrap_or(0.))?;
                }else{
                    ensure!(values.len()==2,"motion SPEED TURN");
                    motion::set_goal(Arc::make_mut(&mut engine.world),values[0],values[1])?;
                }
            }
            _=>bail!("Unknown command '{verb}'. Type help."),
        }
        Ok(format!("{line} ✓"))
    }
}
