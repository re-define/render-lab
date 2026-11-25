//! Render-owned frame products. Logical graph resources carry no scene or viewport semantics.
use super::{ExtractionStats, geometry::SkinBatches, rays};
use crate::rdg::{Graph, Op, Plan, Resource, Storage};
use anyhow::{Context, Result, ensure};
use std::ops::{Deref, DerefMut};

#[derive(Default)]
pub struct FrameProducts {
    pub final_color: Option<Resource>,
    pub visibility: Option<Resource>,
    pub depth: Option<Resource>,
    pub motion: Option<Resource>,
    /// Final posed triangle inputs, in draw order, before optional acceleration builds.
    pub ray_surfaces: Vec<super::RaySurface>,
    pub(crate) request_ray_proxies: bool,
    pub(crate) cluster_shadow: Option<rays::ClusterShadow>,
    pub(crate) material_atlas: Option<Resource>,
    pub(crate) atlas_build: (u64, usize),
    pub(crate) instance_visits: usize,
}
impl FrameProducts {
    pub fn cluster_blas(&self) -> Option<Resource> {
        self.cluster_shadow.map(|cluster| cluster.blas)
    }
}

pub struct FrameGraph {
    pub(super) graph: Graph,
    pub(super) skins: SkinBatches,
    pub(crate) products: FrameProducts,
    present: Option<Resource>,
}
impl FrameGraph {
    pub fn new(extent: [u32; 2]) -> Self {
        Self {
            graph: Graph::new(extent),
            skins: SkinBatches::default(),
            products: FrameProducts::default(),
            present: None,
        }
    }
    pub fn products(&self) -> &FrameProducts {
        &self.products
    }
    pub fn products_mut(&mut self) -> &mut FrameProducts {
        &mut self.products
    }
    /// Select the BGRA display buffer or the explicit byte buffer of an offscreen probe.
    pub fn set_output(&mut self, resource: Resource) {
        self.products.final_color = Some(resource);
    }
    pub fn present(&mut self) -> Result<Resource> {
        let output = self
            .products
            .final_color
            .context("frame has no final color")?;
        self.present = Some(output);
        Ok(output)
    }
    pub fn compile(mut self) -> Result<FramePlan> {
        std::mem::take(&mut self.skins).finish(&mut self.graph);
        let output = self
            .products
            .final_color
            .context("frame has no explicit output")?;
        let [w, h] = self.output_extent();
        let bytes = u64::from(w) * u64::from(h);
        ensure!(
            matches!(self.resources.get(output.0), Some((_, Storage::Buffer{bytes:size,..})) if (*size as u64)/4>=bytes),
            "frame output must be a buffer covering the output extent"
        );
        ensure!(
            self.graph.host_readable_buffer(output),
            "select an owned output buffer for the frame"
        );
        ensure!(
            !self
                .graph
                .passes
                .iter()
                .any(|pass| matches!(pass.op, Op::Present(_))),
            "select presentation through FrameGraph::present"
        );
        if let Some(selected) = self.present {
            ensure!(
                selected == output,
                "final output changed after selection for presentation"
            );
            self.graph
                .pass("Present swapchain image", Op::Present(output));
        } else {
            self.graph.pass("Capture viewport", Op::HostRead(output));
        }
        if let Some(atlas) = self.products.material_atlas {
            self.graph.prefer_local_buffer(atlas);
        }
        Ok(FramePlan {
            stamp: Default::default(),
            commands: self.graph.compile()?,
            output,
            instance_visits: self.products.instance_visits,
            material_atlas: self.products.material_atlas,
            atlas_build: self.products.atlas_build,
            build_ms: 0.,
            main_ms: 0.,
            extract_ms: 0.,
            extraction: Default::default(),
        })
    }
}
impl Deref for FrameGraph {
    type Target = Graph;
    fn deref(&self) -> &Graph {
        &self.graph
    }
}
impl DerefMut for FrameGraph {
    fn deref_mut(&mut self) -> &mut Graph {
        &mut self.graph
    }
}
/// Monotonic CPU timestamps travel with their admitted render snapshot.
#[derive(Default, Clone, Copy)]
pub(crate) struct FrameStamp {
    pub tick_id: u64,
    pub snapshot_id: u64,
    pub skipped_ticks: u64,
    pub generation: u64,
    pub origin: Option<std::time::Instant>,
    pub tick_started: Option<std::time::Instant>,
    pub snapshot_ready: Option<std::time::Instant>,
    pub render_dequeued: Option<std::time::Instant>,
    pub plan_ready: Option<std::time::Instant>,
}
pub struct FramePlan {
    pub(crate) stamp: FrameStamp,
    pub(crate) commands: Plan,
    pub(crate) output: Resource,
    pub(crate) instance_visits: usize,
    pub(crate) material_atlas: Option<Resource>,
    pub(crate) atlas_build: (u64, usize),
    pub(crate) build_ms: f64,
    pub(crate) main_ms: f64,
    pub(crate) extract_ms: f64,
    pub(crate) extraction: ExtractionStats,
}
impl Deref for FramePlan {
    type Target = Plan;
    fn deref(&self) -> &Plan {
        &self.commands
    }
}
