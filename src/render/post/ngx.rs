use super::*;
use crate::rdg::PassPhase;
use crate::rdg::{Access, Dispatch, Dlss, ImageDesc, ImageFormat, Op, Shader, Storage};

pub(super) struct Nvidia;

impl Backend for Nvidia {
    fn resolve(
        &self,
        graph: &mut FrameGraph,
        frame: &TemporalFrame,
        inputs: Inputs,
    ) -> Result<ColorInput> {
        use Access::{ComputeRead as R, ComputeWrite as W};
        let input_extent = graph.extent;
        let output_extent = graph.output_extent();
        let hdr = inputs.color.encoding == ColorEncoding::LinearRgba32;
        let image = |g: &mut FrameGraph, name, format, extent| {
            g.resource(name, Storage::Image(ImageDesc { extent, format }))
        };
        let buffer = |g: &mut FrameGraph, name, [width, height]: [u32; 2], stride| {
            let r = g.resource(
                name,
                Storage::Buffer {
                    bytes: width as usize * height as usize * stride,
                    initial: None,
                },
            );
            g.prefer_local_buffer(r);
            r
        };
        let packed = buffer(graph, "DLSS packed input color", input_extent, 16);
        let vectors = buffer(graph, "DLSS motion vectors", input_extent, 4);
        let color = image(
            graph,
            "DLSS input color",
            ImageFormat::Rgba32Float,
            input_extent,
        );
        let motion = image(
            graph,
            "DLSS input motion",
            ImageFormat::Rg16Float,
            input_extent,
        );
        let output = image(
            graph,
            "DLSS output",
            ImageFormat::Rgba32Float,
            output_extent,
        );
        let groups = |extent: [u32; 2]| [extent[0].div_ceil(8), extent[1].div_ceil(8), 1];
        graph.pass_in(
            PassPhase::Post,
            "Prepare DLSS color and motion",
            Op::Compute {
                program: Dispatch {
                    shader: Shader::Compute(if hdr {
                        include_bytes!(concat!(env!("OUT_DIR"), "/dlss_input_hdr.comp.spv"))
                    } else {
                        include_bytes!(concat!(env!("OUT_DIR"), "/dlss_input.comp.spv"))
                    }),
                    bindings: vec![
                        (inputs.color.resource, R),
                        (inputs.motion, R),
                        (packed, W),
                        (vectors, W),
                    ],
                    push: bytemuck::bytes_of(&input_extent).to_vec(),
                },
                groups: groups(input_extent),
            },
        );
        graph.pass(
            "Copy DLSS color image",
            Op::WritePixels {
                buffer: packed,
                image: color,
            },
        );
        graph.pass(
            "Copy DLSS motion image",
            Op::WritePixels {
                buffer: vectors,
                image: motion,
            },
        );
        graph.pass_in(
            PassPhase::Post,
            if frame.mode == Mode::Dlaa {
                "NVIDIA DLAA"
            } else {
                "NVIDIA DLSS Quality"
            },
            Op::Dlss(Dlss {
                color,
                depth: inputs.depth,
                motion,
                output,
                hdr,
                reset: frame.reset,
                jitter: frame.jitter,
            }),
        );
        let unpack = buffer(graph, "DLSS packed output color", output_extent, 16);
        graph.pass(
            "Copy DLSS output to frame color",
            Op::ReadPixels {
                image: output,
                buffer: unpack,
            },
        );
        if hdr {
            return Ok(ColorInput {
                resource: unpack,
                encoding: inputs.color.encoding,
            });
        }
        let resolved = buffer(graph, "DLSS resolved color", output_extent, 4);
        graph.pass_in(
            PassPhase::Post,
            "Unpack DLSS frame color",
            Op::Compute {
                program: Dispatch {
                    shader: Shader::Compute(include_bytes!(concat!(
                        env!("OUT_DIR"),
                        "/dlss_output.comp.spv"
                    ))),
                    bindings: vec![(unpack, R), (resolved, W)],
                    push: bytemuck::bytes_of(&output_extent).to_vec(),
                },
                groups: groups(output_extent),
            },
        );
        Ok(ColorInput {
            resource: resolved,
            encoding: inputs.color.encoding,
        })
    }
}
