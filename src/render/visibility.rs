use super::*;
use Access::{ComputeRead as R, ComputeReadWrite as RW, ComputeWrite as W};
pub(crate) fn resolve(g: &mut Graph, t: Targets, s: &Snapshot) -> Result<(Resource, Resource)> {
    let [w, h] = s.extent;
    let count = w
        .checked_mul(h)
        .ok_or_else(|| anyhow::anyhow!("viewport overflow"))? as usize;
    let ids = g.resource(
        "Pixel visibility IDs",
        Storage::Buffer {
            bytes: count * 8,
            initial: None,
        },
    );
    g.pass(
        "Read visibility IDs on GPU",
        Op::ReadPixels {
            image: t.visibility,
            buffer: ids,
        },
    );
    let objects = g.input("Surface instances", bytemuck::cast_slice(&t.items).to_vec());
    let addresses = g.address_table(t.addresses);
    let mut camera = s.camera.view_projection(s.extent).to_cols_array().to_vec();
    camera.extend(s.camera.eye().extend(1.).to_array());
    let mut view = bytemuck::cast_slice(&camera).to_vec();
    view.extend(bytemuck::cast_slice(&s.extent));
    let view = g.input("Resolve camera", view);
    let output = g.resource(
        "Shaded BGRA pixels",
        Storage::Buffer {
            bytes: count * 4,
            initial: None,
        },
    );
    let models = &s.materials.kernels;
    let bins = g.input("Material shading bins", vec![0; 16 * models.len()]);
    let args = g.resource(
        "Shading dispatches",
        Storage::Buffer {
            bytes: 16 * models.len(),
            initial: None,
        },
    );
    let list = g.resource(
        "Visible pixel list",
        Storage::Buffer {
            bytes: 4 * count,
            initial: None,
        },
    );
    let classify = |shader, destination| Op::Compute {
        program: Dispatch {
            shader: Shader::Compute(shader),
            bindings: vec![(ids, R), (objects, R), (bins, RW), (destination, W)],
            push: bytemuck::bytes_of(&[w, h, t.background]).to_vec(),
        },
        groups: [w.div_ceil(8), h.div_ceil(8), 1],
    };
    g.pass(
        "Classify visible pixels",
        classify(
            include_bytes!(concat!(env!("OUT_DIR"), "/classify.comp.spv")),
            output,
        ),
    );
    g.pass(
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
    g.pass(
        "Scatter visible pixels",
        classify(
            include_bytes!(concat!(env!("OUT_DIR"), "/scatter.comp.spv")),
            list,
        ),
    );
    for (index, &(_, name, shader)) in models.iter().enumerate() {
        let bin = u32::try_from(index)?;
        g.pass(
            &format!("Shade {name}"),
            Op::ComputeIndirect {
                program: Dispatch {
                    shader: Shader::Compute(shader),
                    bindings: vec![
                        (ids, R),
                        (objects, R),
                        (list, R),
                        (addresses, R),
                        (view, R),
                        (t.texture, R),
                        (output, RW),
                        (bins, R),
                    ],
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
    Ok((ids, output))
}
