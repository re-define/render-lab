//! Bind the forest light and probe programs to the renderer's selected ray scene.
use crate::{
    rdg::{Access, PassPhase, Resource, Shader},
    render::{
        FrameGraph,
        rays::{NativeScene, RayScene},
    },
};
use anyhow::{Context, Result};

impl RayScene {
    pub(in crate::experiments) fn probe_inputs(&self, atlas: Resource) -> Result<Vec<Resource>> {
        Ok(match self {
            Self::Software {
                nodes,
                triangles,
                deformed,
            } => vec![
                *nodes,
                *triangles,
                atlas,
                deformed.nodes,
                deformed.triangles,
            ],
            Self::Hardware(scene) => {
                let [streams, slots] = scene
                    .hit_streams
                    .context("closest-hit queries need the published surface streams")?;
                vec![streams, slots, atlas]
            }
        })
    }
}

macro_rules! shader {
    ($name:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/", $name, ".spv"))
    };
}

impl NativeScene {
    pub(super) fn shadows(&self, g: &mut FrameGraph, mut bindings: Vec<(Resource, Access)>) {
        let miss = shader!("mega_shadow.rmiss");
        let (name, shader) = if let Some(cluster) = self.cluster {
            bindings.extend([
                (cluster.metadata, Access::RayRead),
                (cluster.view, Access::RayRead),
            ]);
            (
                "Trace Mega Geometry shadow rays",
                Shader::ClusterShadows(
                    shader!("mega_shadow.rgen"),
                    miss,
                    shader!("mega_shadow.rahit"),
                ),
            )
        } else {
            (
                "Trace hardware proxy shadow rays",
                Shader::RayTracing(
                    shader!("proxy_shadow.rgen"),
                    miss,
                    shader!("opaque_shadow.rchit"),
                ),
            )
        };
        let trace = self.trace(g.extent, shader, bindings);
        g.pass_in(PassPhase::Lighting, name, trace);
    }

    pub(in crate::experiments) fn probes(
        &self,
        g: &mut FrameGraph,
        extent: [u32; 2],
        mut bindings: Vec<(Resource, Access)>,
        raster: [Resource; 3],
    ) {
        let miss = shader!("gi12_hit.rmiss");
        let (name, shader) = if let Some(cluster) = self.cluster {
            bindings.extend(
                [cluster.metadata, cluster.view]
                    .into_iter()
                    .chain(raster)
                    .map(|r| (r, Access::RayRead)),
            );
            (
                "GI-1.2 trace hardware cluster probe paths",
                Shader::ClusterRays(
                    shader!("gi12_mega.rgen"),
                    miss,
                    shader!("mega_gi.rahit"),
                    shader!("gi12_cluster.rchit"),
                ),
            )
        } else {
            (
                "GI-1.2 trace hardware probe paths",
                Shader::RayTracing(shader!("gi12_proxy.rgen"), miss, shader!("gi12_hit.rchit")),
            )
        };
        g.pass_in(
            PassPhase::Lighting,
            name,
            self.trace(extent, shader, bindings),
        );
    }
}
