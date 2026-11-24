//! Fixed-camera lighting and ray tracing comparisons.
use crate::experiments::assembly::{Primary, Registration as SceneModule};
pub(super) const MODULE: SceneModule = SceneModule {
    key: SETTING,
    primary: Primary::Exclusive,
    restore: None,
    install: |world, a| {
        let (_, cfg) = Settings::read(world)?;
        ensure!(
            cfg.kind != Kind::Rt || a.caps.pipeline,
            "RT preview requires VK_KHR_ray_tracing_pipeline"
        );
        a.camera = Some(configure(world, &mut a.materials)?);
        a.technique = Some(std::sync::Arc::new(Lighting::default()));
        Ok(())
    },
};
use crate::render::FrameGraph;

use crate::{
    engine::Engine,
    rdg::Shader,
    render::{Camera, Forward, MaterialPrograms, Snapshot, Technique},
    scene::*,
};
use anyhow::{Context, Result, bail, ensure};
use glam::{Mat4, Vec3};
pub use scenes::scene;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const SETTING: &str = "lab.lighting";
const PROGRAM: &str = "Lighting experiment";
pub(crate) const EMITTER: &str = "lab.lighting.emitter";
pub(crate) const GRID: &str = "lab.lighting.grid";

macro_rules! spirv {
    ($shader:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/", $shader, ".spv"))
    };
}

pub(super) mod geometry;
mod pipeline;
pub(super) mod scenes;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Gi,
    Shadow,
    Rt,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Gi => "gi",
            Self::Shadow => "shadow",
            Self::Rt => "rt",
        }
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[repr(u32)]
#[serde(rename_all = "lowercase")]
enum Gi {
    Off,
    Ssgi,
    Ray,
    Probes,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[repr(u32)]
#[serde(rename_all = "lowercase")]
enum Shadow {
    Hard,
    Pcf,
    Pcss,
    Ray,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[repr(u32)]
#[serde(rename_all = "lowercase")]
enum View {
    Combined,
    Direct,
    Indirect,
    Albedo,
    Visibility,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[repr(u32)]
#[serde(rename_all = "lowercase")]
enum RayView {
    Beauty,
    Normals,
    Distance,
    Reflections,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct Settings {
    kind: Kind,
    gi: Gi,
    shadow: Shadow,
    view: View,
    ray_view: RayView,
    size: f32,
    bias: f32,
}

impl Settings {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            gi: if kind == Kind::Gi { Gi::Ray } else { Gi::Off },
            shadow: if kind == Kind::Gi {
                Shadow::Ray
            } else {
                Shadow::Pcss
            },
            view: View::Combined,
            ray_view: RayView::Beauty,
            size: if kind == Kind::Gi { 0.68 } else { 0.58 },
            bias: 0.018,
        }
    }

    fn read(world: &World) -> Result<(Id, Self)> {
        let (id, raw) = world
            .first_raw_component(SETTING)
            .context("scene has no lighting experiment settings")?;
        Ok((id, Self::decode(raw)?))
    }

    fn decode(raw: &serde_json::Value) -> Result<Self> {
        let settings = Self::deserialize(raw)?;
        ensure!(
            (0.0..=1.75).contains(&settings.size) && (0.001..=0.12).contains(&settings.bias),
            "invalid light size or shadow bias"
        );
        Ok(settings)
    }
}

fn camera(kind: Kind, view: &str) -> Result<Camera> {
    let (target, distance, yaw, pitch) = match (kind, view) {
        (Kind::Gi, "overview") => ([0., 1.93, -0.8], 8.2, 0., 0.002),
        (Kind::Gi, "detail") => ([1.2, 1.19, -1.85], 5.4, 0.12, 0.12),
        (Kind::Shadow, "overview") => ([0., 0.45, -4.3], 17.4, 0.34, 0.68),
        (Kind::Shadow, "contact") => ([-0.8, 0.48, -0.55], 6.8, 0.39, 0.54),
        (Kind::Shadow, "distance") => ([0.35, 0.52, -7.2], 9., -0.45, 0.55),
        (Kind::Rt, "overview") => ([0.05, 1.11, -1.2], 7.2, -0.31, 0.19),
        (Kind::Rt, "detail") => ([0.64, 1.05, -1.38], 5.1, 0.16, 0.22),
        _ => bail!("unknown {} camera preset {view}", kind.name()),
    };
    Ok(Camera {
        target: target.into(),
        distance,
        yaw,
        pitch,
    })
}

pub(super) fn configure(world: &World, materials: &mut MaterialPrograms) -> Result<Camera> {
    let (_, settings) = Settings::read(world)?;
    if settings.kind != Kind::Rt && materials.bin(&Shading::Custom(PROGRAM.into())).is_err() {
        materials.register(PROGRAM, spirv!("lighting_material.comp"))?;
    }
    camera(settings.kind, "overview")
}

pub fn command(engine: &mut Engine, kind: Kind, args: &str) -> Result<String> {
    let (id, mut cfg) = Settings::read(&engine.world)?;
    ensure!(
        cfg.kind == kind,
        "this command requires --experiment {}",
        kind.name()
    );
    let words: Vec<_> = args.split_whitespace().collect();
    match (kind, words.as_slice()) {
        (_, ["camera", view]) => engine.camera = camera(kind, view)?,
        (Kind::Gi, ["off"]) => cfg.gi = Gi::Off,
        (Kind::Gi, ["ssgi"]) => cfg.gi = Gi::Ssgi,
        (Kind::Gi, ["ray"]) => cfg.gi = Gi::Ray,
        (Kind::Gi, ["probes"]) => cfg.gi = Gi::Probes,
        (Kind::Shadow, ["hard"]) => cfg.shadow = Shadow::Hard,
        (Kind::Shadow, ["pcf"]) => cfg.shadow = Shadow::Pcf,
        (Kind::Shadow, ["pcss"]) => cfg.shadow = Shadow::Pcss,
        (Kind::Shadow, ["ray"]) => cfg.shadow = Shadow::Ray,
        (Kind::Shadow, ["size", value]) => {
            cfg.size = value.parse()?;
            ensure!(
                (0.0..=1.75).contains(&cfg.size),
                "light half-width must be in [0, 1.75] m"
            );
        }
        (Kind::Shadow, ["bias", value]) => {
            cfg.bias = value.parse()?;
            ensure!(
                (0.001..=0.12).contains(&cfg.bias),
                "depth bias must be in [0.001, 0.12] m"
            );
        }
        (Kind::Gi | Kind::Shadow, ["view", view]) => {
            cfg.view = match *view {
                "combined" => View::Combined,
                "direct" => View::Direct,
                "indirect" if kind == Kind::Gi => View::Indirect,
                "albedo" => View::Albedo,
                "visibility" => View::Visibility,
                _ => bail!("view combined/direct/albedo/visibility (GI also supports indirect)"),
            }
        }
        (Kind::Rt, ["view", view]) => {
            cfg.ray_view = match *view {
                "beauty" => RayView::Beauty,
                "normals" => RayView::Normals,
                "distance" => RayView::Distance,
                "reflections" => RayView::Reflections,
                _ => bail!("rt view beauty/normals/distance/reflections"),
            }
        }
        _ => bail!(match kind {
            Kind::Gi =>
                "gi off/ssgi/probes/ray · gi view combined/direct/indirect/albedo/visibility · gi camera overview/detail",
            Kind::Shadow =>
                "shadow hard/pcf/pcss/ray · shadow size M · shadow bias M · shadow view combined/visibility · shadow camera overview/contact/distance",
            Kind::Rt => "rt view beauty/normals/distance/reflections · rt camera overview/detail",
        }),
    }
    Arc::make_mut(&mut engine.world)
        .entities
        .get_mut(&id)
        .unwrap()
        .components
        .insert(SETTING.into(), serde_json::to_value(cfg)?);
    Ok(format!("{} {args}", kind.name()))
}

#[derive(Default)]
pub(super) struct Lighting(geometry::Cache);
impl Technique for Lighting {
    fn accepts_temporal_inputs(&self, snapshot: &Snapshot) -> bool {
        snapshot
            .scene
            .setting(SETTING)
            .and_then(|value| value.get("kind"))
            .and_then(|value| value.as_str())
            != Some("rt")
    }

    fn build(&self, snapshot: &Snapshot) -> Result<FrameGraph> {
        let settings = Settings::decode(
            snapshot
                .scene
                .setting(SETTING)
                .context("scene has no lighting experiment settings")?,
        )?;
        match settings.kind {
            Kind::Rt => pipeline::trace_frame(snapshot, settings),
            _ => pipeline::raster_frame(snapshot, settings, &self.0),
        }
    }
}
