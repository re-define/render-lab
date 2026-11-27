//! Compatible opaque surfaces retain their original order while sharing a mesh draw.
//! Compute writes an ordered interval of mesh workgroups for each surviving surface.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct MeshTemplate {
    vertices: usize,
    min: Vec3,
    max: Vec3,
}

impl MeshTemplate {
    pub(super) fn new(mesh: &Mesh) -> Self {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for &position in &mesh.positions {
            let point = Vec3::from(position);
            min = min.min(point);
            max = max.max(point);
        }
        Self {
            vertices: mesh.positions.len(),
            min,
            max,
        }
    }
}

struct Draw {
    surface: SurfaceBuffers,
    item: u32,
    template: MeshTemplate,
    skinned: bool,
}

#[derive(Default)]
pub(crate) struct OrdinaryDrawList(Vec<Draw>);

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuDraw {
    clip_from_vertex: [f32; 16],
    min: [f32; 4],
    max: [f32; 4],
    info: [u32; 4], // stable surface, triangles, vertex count, world-space skinned stream
}

/// A surface exceeding one dispatch row stays in its original position and uses
/// the direct shader's 2D workgroup index. Split before it when a packet is open.
fn packet_len(triangles: impl Iterator<Item = u32>) -> usize {
    const MAX_SURFACES: usize = 256;
    const MAX_GROUPS: u32 = 32768;
    let mut groups = 0;
    let mut count = 0;
    for triangles in triangles.take(MAX_SURFACES) {
        let additional = triangles.div_ceil(32);
        if additional > MAX_GROUPS - groups {
            return count.max(1);
        }
        groups += additional;
        count += 1;
    }
    count
}

impl OrdinaryDrawList {
    pub(super) fn push(
        &mut self,
        surface: SurfaceBuffers,
        item: u32,
        template: &MeshTemplate,
        skinned: bool,
    ) {
        self.0.push(Draw {
            surface,
            item,
            template: *template,
            skinned,
        });
    }

    pub(crate) fn emit(self, g: &mut FrameGraph, t: &mut Targets, s: &Snapshot) -> Result<()> {
        const MIN_SURFACES: usize = 16;
        let mut rest = self.0.as_slice();
        while !rest.is_empty() {
            let count = packet_len(rest.iter().map(|d| d.surface.count));
            let (batch, tail) = rest.split_at(count);
            if s.reference_draws || batch.len() < MIN_SURFACES {
                for draw in batch {
                    raster_plain(g, t, s, &draw.surface, draw.item);
                }
            } else {
                Self::packet(g, t, s.view_projection(), batch)?;
            }
            rest = tail;
        }
        Ok(())
    }

    fn packet(g: &mut FrameGraph, t: &mut Targets, vp: Mat4, batch: &[Draw]) -> Result<()> {
        let mut vertices = Vec::with_capacity(batch.len());
        let mut faces = Vec::with_capacity(batch.len());
        let mut records = Vec::with_capacity(batch.len());
        for draw in batch {
            vertices.push(draw.surface.vertices);
            faces.push(draw.surface.faces);
            records.push(GpuDraw {
                clip_from_vertex: (vp * draw.surface.model).to_cols_array(),
                min: draw.template.min.extend(0.).to_array(),
                max: draw.template.max.extend(0.).to_array(),
                info: [
                    draw.item,
                    draw.surface.count,
                    draw.template.vertices.try_into()?,
                    u32::from(draw.skinned),
                ],
            });
        }
        let vertices = g.address_table(vertices);
        let faces = g.address_table(faces);
        let records = g.input(
            "Ordinary draw transforms and bounds",
            bytemuck::cast_slice(&records).to_vec(),
        );
        let mut temp = |label, bytes| {
            let r = g.resource(
                label,
                Storage::Buffer {
                    bytes,
                    initial: None,
                },
            );
            g.prefer_local_buffer(r);
            r
        };
        let counts = temp("Ordinary visible group counts", batch.len() * 4);
        let ends = temp("Ordered ordinary group ends", batch.len() * 4);
        let args = temp("Ordinary indirect mesh arguments", 12);
        let count = batch.len() as u32;
        use Access::{ComputeRead as R, ComputeWrite as W, Mesh as M};
        g.pass_in(
            PassPhase::Culling,
            "Cull ordinary surfaces",
            Op::Compute {
                program: Dispatch {
                    shader: Shader::Compute(include_bytes!(concat!(
                        env!("OUT_DIR"),
                        "/ordinary_cull.comp.spv"
                    ))),
                    bindings: vec![(records, R), (vertices, R), (counts, W)],
                    push: vec![],
                },
                groups: [count, 1, 1],
            },
        );
        g.pass_in(
            PassPhase::Culling,
            "Build ordered ordinary mesh work",
            Op::Compute {
                program: Dispatch {
                    shader: Shader::Compute(include_bytes!(concat!(
                        env!("OUT_DIR"),
                        "/ordinary_work.comp.spv"
                    ))),
                    bindings: vec![(counts, R), (ends, W), (args, W)],
                    push: count.to_ne_bytes().to_vec(),
                },
                groups: [1; 3],
            },
        );
        g.pass(
            "Batched ordinary mesh visibility",
            Op::Draw {
                program: Dispatch {
                    shader: Shader::Mesh(
                        include_bytes!(concat!(env!("OUT_DIR"), "/ordinary.mesh.spv")),
                        include_bytes!(concat!(env!("OUT_DIR"), "/visibility.frag.spv")),
                    ),
                    bindings: vec![(records, M), (vertices, M), (faces, M), (ends, M)],
                    push: count.to_ne_bytes().to_vec(),
                },
                color: t.visibility,
                depth: t.depth,
                clear: t.clear,
                count: DrawCount::MeshIndirect { args, offset: 0 },
            },
        );
        t.clear = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::packet_len;

    #[test]
    fn packet_limits_preserve_oversized_surfaces_and_keep_the_complete_tail() {
        let row = 32768 * 32;
        assert_eq!(packet_len([row - 32, 1, 1].into_iter()), 2);
        assert_eq!(packet_len([row + 1, 1].into_iter()), 1);
        assert_eq!(packet_len([1, 1, u32::MAX].into_iter()), 2);
        assert_eq!(packet_len([0, 0, 1].into_iter()), 3);
        assert_eq!(packet_len(std::iter::repeat_n(1, 257)), 256);
    }
}
