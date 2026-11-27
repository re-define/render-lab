use super::*;
use crate::rdg::PassPhase;
use crate::rdg::{Access, Dispatch, Op, Shader};
use anyhow::ensure;
use bytemuck::{Pod, Zeroable};

pub(super) struct Taa;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct View {
    flags: [u32; 4],
    jitter: [f32; 4],
}

impl Backend for Taa {
    fn resolve(
        &self,
        graph: &mut FrameGraph,
        frame: &TemporalFrame,
        inputs: Inputs,
    ) -> Result<ColorInput> {
        use Access::{ComputeRead as R, ComputeSample as S, ComputeWrite as W};
        let [width, height] = graph.extent;
        let count = width
            .checked_mul(height)
            .ok_or_else(|| anyhow::anyhow!("TAA viewport overflow"))? as usize;
        ensure!(count != 0, "empty TAA viewport");

        let hdr = inputs.color.encoding == ColorEncoding::LinearRgba32;
        let debug = frame.debug;
        let output_hdr = hdr && debug == DebugView::Off;
        let output = graph.local_buffer(
            if debug == DebugView::Off {
                "Temporal AA color"
            } else {
                "Temporal diagnostic color"
            },
            count * if output_hdr { 16 } else { 4 },
        );
        let groups = [width.div_ceil(8), height.div_ceil(8), 1];
        if debug == DebugView::Motion {
            let extent = graph.extent;
            graph.pass_in(
                PassPhase::Post,
                "Visualize motion vectors",
                Op::Compute {
                    program: Dispatch {
                        shader: Shader::Compute(include_bytes!(concat!(
                            env!("OUT_DIR"),
                            "/motion_view.comp.spv"
                        ))),
                        bindings: vec![(inputs.motion, R), (output, W)],
                        push: bytemuck::bytes_of(&extent).to_vec(),
                    },
                    groups,
                },
            );
            return Ok(ColorInput {
                resource: output,
                encoding: ColorEncoding::Bgra8,
            });
        }
        let history = graph.history_buffer(
            "Temporal history color and depth",
            frame.color_key,
            16 + count * 16,
        );
        let tags = graph.history_buffer(
            "Temporal history surface tags",
            frame.color_key ^ (1 << 56),
            count * 4,
        );
        graph.prefer_local_buffer(history);
        graph.prefer_local_buffer(tags);
        let view = View {
            flags: [
                width,
                height,
                u32::from(frame.stationary_camera),
                debug as u32,
            ],
            jitter: [frame.jitter[0], frame.jitter[1], 0., 0.],
        };
        let view = graph.input(
            "Temporal resolve settings",
            bytemuck::bytes_of(&view).to_vec(),
        );
        let shader: &'static [u8] = match (hdr, output_hdr) {
            (true, true) => include_bytes!(concat!(env!("OUT_DIR"), "/taa_hdr.comp.spv")),
            (true, false) => include_bytes!(concat!(env!("OUT_DIR"), "/taa_debug_hdr.comp.spv")),
            _ => include_bytes!(concat!(env!("OUT_DIR"), "/taa.comp.spv")),
        };
        graph.pass_in(
            PassPhase::Post,
            if debug == DebugView::Reprojection {
                "Visualize temporal reprojection error"
            } else {
                "Temporal AA resolve"
            },
            Op::Compute {
                program: Dispatch {
                    shader: Shader::Compute(shader),
                    bindings: vec![
                        (inputs.motion, R),
                        (inputs.depth, S),
                        (view, R),
                        (inputs.color.resource, R),
                        (history, R),
                        (tags, R),
                        (output, W),
                    ],
                    push: vec![],
                },
                groups,
            },
        );
        graph.pass_in(
            PassPhase::Post,
            "Store temporal AA history",
            Op::Compute {
                program: Dispatch {
                    shader: Shader::Compute(if hdr {
                        include_bytes!(concat!(env!("OUT_DIR"), "/taa_store_hdr.comp.spv"))
                    } else {
                        include_bytes!(concat!(env!("OUT_DIR"), "/taa_store.comp.spv"))
                    }),
                    bindings: vec![
                        (inputs.motion, R),
                        (inputs.depth, S),
                        (view, R),
                        (
                            if debug == DebugView::Reprojection {
                                inputs.color.resource
                            } else {
                                output
                            },
                            R,
                        ),
                        (history, W),
                        (tags, W),
                    ],
                    push: vec![],
                },
                groups,
            },
        );
        Ok(ColorInput {
            resource: output,
            encoding: if output_hdr {
                ColorEncoding::LinearRgba32
            } else {
                ColorEncoding::Bgra8
            },
        })
    }
}
