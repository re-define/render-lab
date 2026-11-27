use super::*;
use crate::rdg::PassPhase;
use Access::{ComputeRead as R, ComputeReadWrite as RW, ComputeSample as S, ComputeWrite as W};
#[derive(Clone, Copy)]
pub(crate) struct ResolveInputs {
    pub ids: Resource,
    pub objects: Resource,
    pub addresses: Resource,
    pub camera: Resource,
    pub depth: Resource,
    pub motion: Option<Resource>,
}

pub(crate) struct MaterialInputs {
    pub bin: u32,
    pub bindings: Vec<(Resource, Access)>,
}

#[derive(Default)]
pub(crate) struct FrameLighting {
    pub materials: Vec<MaterialInputs>,
    pub linear_hdr: bool,
}

impl FrameLighting {
    pub(crate) fn single(inputs: MaterialInputs) -> Self {
        Self {
            materials: vec![inputs],
            linear_hdr: false,
        }
    }
}

pub(super) fn surface_table<T: bytemuck::Pod>(
    g: &mut FrameGraph,
    name: &str,
    tail: &[T],
    prefix: Option<(Resource, u32)>,
) -> Resource {
    let Some((buffer, count)) = prefix else {
        return g.input(name, bytemuck::cast_slice(tail).to_vec());
    };
    if !tail.is_empty() {
        let data = g.input(name, bytemuck::cast_slice(tail).to_vec());
        let words = (size_of::<T>() / 4) as u32;
        let n = tail.len() as u32 * words;
        g.pass_in(
            PassPhase::Bins,
            "Append ordinary surface records",
            Op::Compute {
                program: Dispatch {
                    shader: Shader::Compute(include_bytes!(concat!(
                        env!("OUT_DIR"),
                        "/surface_tail.comp.spv"
                    ))),
                    bindings: vec![(data, R), (buffer, RW)],
                    push: bytemuck::bytes_of(&[count * words, n]).to_vec(),
                },
                groups: [n.div_ceil(64).min(32768), n.div_ceil(64).div_ceil(32768), 1],
            },
        );
    }
    buffer
}
pub(crate) fn resolve(
    g: &mut FrameGraph,
    mut t: Targets,
    s: &Snapshot,
    lighting: impl FnOnce(&mut FrameGraph, ResolveInputs) -> Result<FrameLighting>,
) -> Result<(ResolveInputs, Resource, FrameLighting)> {
    let [w, h] = s.extent;
    let count = w
        .checked_mul(h)
        .ok_or_else(|| anyhow::anyhow!("viewport overflow"))? as usize;
    let ids = t.visibility;
    let objects = t.shading_table(g, s)?;
    let addresses = g.address_table(t.addresses);
    let mut camera = s.view_projection().to_cols_array().to_vec();
    camera.extend(s.camera.eye().extend(1.).to_array());
    let mut view = bytemuck::cast_slice(&camera).to_vec();
    view.extend(bytemuck::cast_slice(&s.extent));
    let view = g.input("Resolve camera", view);
    let mut inputs = ResolveInputs {
        ids,
        objects,
        addresses,
        camera: view,
        depth: t.depth,
        motion: None,
    };
    if let Some((table, frame)) = t.motion.take().zip(s.temporal.as_ref()) {
        inputs.motion = Some(
            table.resolve(
                g,
                s,
                frame,
                inputs,
                t.prefix
                    .as_ref()
                    .and_then(|p| p.motion.map(|r| (r, p.count))),
            ),
        );
    }
    let lighting = lighting(g, inputs)?;
    let output = g.resource(
        if lighting.linear_hdr {
            "Linear HDR scene color"
        } else {
            "Shaded BGRA pixels"
        },
        Storage::Buffer {
            bytes: count * if lighting.linear_hdr { 16 } else { 4 },
            initial: None,
        },
    );
    let models = &s.materials.kernels;
    let bins = g.input("Material shading bins", vec![0; 16 * models.len()]);
    // Classification and scattering contend on these counters for every
    // visible pixel. Keep atomics and the scattered pixel list on the GPU heap.
    g.prefer_local_buffer(bins);
    let args = g.local_buffer("Shading dispatches", 16 * models.len());
    let list = g.local_buffer("Visible pixel list", 4 * count);
    let classify = |shader, destination| Op::Compute {
        program: Dispatch {
            shader: Shader::Compute(shader),
            bindings: vec![(ids, S), (objects, R), (bins, RW), (destination, W)],
            push: bytemuck::bytes_of(&[w, h, t.background]).to_vec(),
        },
        groups: [w.div_ceil(8), h.div_ceil(8), 1],
    };
    g.pass_in(
        PassPhase::Bins,
        "Classify visible pixels",
        classify(
            if lighting.linear_hdr {
                include_bytes!(concat!(env!("OUT_DIR"), "/classify_hdr.comp.spv"))
            } else {
                include_bytes!(concat!(env!("OUT_DIR"), "/classify.comp.spv"))
            },
            output,
        ),
    );
    g.pass_in(
        PassPhase::Bins,
        "Allocate material bins",
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/allocate.comp.spv"
                ))),
                bindings: vec![(bins, RW), (args, W)],
                push: (models.len() as u32).to_ne_bytes().to_vec(),
            },
            groups: [1; 3],
        },
    );
    g.pass_in(
        PassPhase::Bins,
        "Scatter visible pixels",
        classify(
            include_bytes!(concat!(env!("OUT_DIR"), "/scatter.comp.spv")),
            list,
        ),
    );
    let scene_lights = (s.scene.lights.present && models.iter().any(|k| k.scene_lit.is_some()))
        .then(|| g.resident_input("Scene light instances", s.scene.lights.bytes.clone()));
    for (index, model) in models.iter().enumerate() {
        let name = model.label;
        let mut shader = model.shader;
        let bin = u32::try_from(index)?;
        let mut bindings = vec![
            (ids, S),
            (objects, R),
            (list, R),
            (addresses, R),
            (view, R),
            (t.texture, R),
            (output, RW),
            (bins, R),
        ];
        if let Some(inputs) = lighting.materials.iter().find(|inputs| inputs.bin == bin) {
            bindings.extend_from_slice(&inputs.bindings);
        } else if let (Some(table), Some(scene_lit)) = (scene_lights, model.scene_lit) {
            shader = scene_lit;
            bindings.push((table, R));
        }
        g.pass_in(
            PassPhase::Shade,
            &format!("Shade {name}"),
            Op::ComputeIndirect {
                program: Dispatch {
                    shader: Shader::Compute(shader.select(lighting.linear_hdr, name)?),
                    bindings,
                    push: bin.to_ne_bytes().to_vec(),
                },
                args,
                offset: u64::from(bin) * 16,
            },
        );
        g.pass(
            &format!("Count {name}"),
            Op::Counters {
                buffer: bins,
                first: bin as usize * 4,
                labels: vec![name],
            },
        );
    }
    Ok((inputs, output, lighting))
}

pub(super) fn tone_map(g: &mut FrameGraph, output: Resource) -> Resource {
    let [w, h] = g.output_extent();
    let ldr = g.resource(
        "Tone-mapped BGRA pixels",
        Storage::Buffer {
            bytes: w as usize * h as usize * 4,
            initial: None,
        },
    );
    g.prefer_local_buffer(output);
    g.pass_in(
        PassPhase::Post,
        "Tone map linear scene color",
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/tone_map.comp.spv"
                ))),
                bindings: vec![(output, R), (ldr, W)],
                push: bytemuck::bytes_of(&[w, h]).to_vec(),
            },
            groups: [w.div_ceil(8), h.div_ceil(8), 1],
        },
    );
    ldr
}
