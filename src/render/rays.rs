//! GPU geometry after skinning, procedural simulation or proxy deformation.
//! The model transforms the published vertex positions to world space exactly once.
use super::*;
use crate::rdg::PassPhase;
use anyhow::Context;
mod scene;
pub(crate) use scene::{NativeScene, RayScene};
#[derive(Clone, Copy, Debug)]
pub struct Surface {
    pub geometry: SurfaceBuffers,
    /// Immutable source identifying the triangle indices, independent of vertex pose.
    pub(crate) topology: Resource,
    pub color: [f32; 4],
    pub dynamic: bool,
    pub material_slot: u32,
}
impl Surface {
    pub fn new(
        geometry: SurfaceBuffers,
        mut color: [f32; 4],
        metallic: f32,
        dynamic: bool,
        material_slot: u32,
    ) -> Self {
        color[3] = metallic;
        Self {
            topology: geometry.faces,
            geometry,
            color,
            dynamic,
            material_slot,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClusterShadow {
    pub blas: Resource,
    pub metadata: Resource,
    pub view: Resource,
    pub proxy_start: usize,
    pub proxy_end: usize,
}
pub(crate) struct Software {
    pub triangles: Resource,
    pub nodes: Resource,
    pub count: u32,
}
pub(crate) fn buffer(g: &mut FrameGraph, name: &str, bytes: usize) -> Resource {
    let r = g.resource(
        name,
        Storage::Buffer {
            bytes: bytes.max(16),
            initial: None,
        },
    );
    g.prefer_local_buffer(r);
    r
}
pub(crate) fn dispatch(
    g: &mut FrameGraph,
    name: &str,
    shader: &'static [u8],
    bindings: Vec<(Resource, Access)>,
    push: Vec<u8>,
    count: u32,
) {
    let groups = count.div_ceil(64);
    g.pass_in(
        PassPhase::Lighting,
        name,
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(shader),
                bindings,
                push,
            },
            groups: [groups.min(32768), groups.div_ceil(32768), 1],
        },
    );
}
pub(crate) fn software(g: &mut FrameGraph, dynamic_only: bool) -> Result<Software> {
    let surfaces: Vec<_> = g
        .products
        .ray_surfaces
        .iter()
        .copied()
        .filter(|s| !dynamic_only || s.dynamic)
        .collect();
    let count = surfaces
        .iter()
        .try_fold(0u32, |n, s| n.checked_add(s.geometry.count))
        .context("ray primitive count overflow")?;
    if count == 0 {
        return Ok(Software {
            triangles: g.input("Empty ray triangles", vec![]),
            nodes: g.input("Empty ray nodes", vec![]),
            count,
        });
    }
    let (triangles_name, nodes_name) = if dynamic_only {
        ("Deformed ray triangles", "Deformed shadow BVH")
    } else {
        ("Scene ray triangles", "Scene ray BVH")
    };
    let triangles = gather(g, &surfaces, count, triangles_name);
    let leaves = count
        .div_ceil(8)
        .checked_next_power_of_two()
        .context("ray BVH exceeds capacity")?;
    let nodes = g.scratch_buffer(nodes_name, 0, (2 * leaves as usize - 1) * 32);
    let height = leaves.ilog2();
    for depth in (0..=height).rev() {
        let width = 1u32 << depth;
        dispatch(
            g,
            if depth == height {
                "Bound deformed triangles"
            } else {
                "Refit ray BVH level"
            },
            include_bytes!(concat!(env!("OUT_DIR"), "/ray_refit.comp.spv")),
            vec![
                (triangles, Access::ComputeRead),
                (
                    nodes,
                    if depth == height {
                        Access::ComputeWrite
                    } else {
                        Access::ComputeReadWrite
                    },
                ),
            ],
            bytemuck::bytes_of(&[depth, height, count]).to_vec(),
            width,
        );
    }
    Ok(Software {
        triangles,
        nodes,
        count,
    })
}

fn gather(g: &mut FrameGraph, surfaces: &[Surface], count: u32, name: &str) -> Resource {
    let triangles = g.scratch_buffer(name, 0, count as usize * 128);
    let mut first = 0;
    for source in surfaces {
        let s = source.geometry;
        if s.count == 0 {
            continue;
        }
        let mut push = vec![0; 4]; // mat4 starts at offset 16, after three descriptor indices
        push.extend(bytemuck::bytes_of(&s.model.to_cols_array()));
        push.extend(bytemuck::bytes_of(&source.color));
        push.extend(bytemuck::bytes_of(&[first, s.count, source.material_slot]));
        dispatch(
            g,
            "Gather shared ray triangles",
            include_bytes!(concat!(env!("OUT_DIR"), "/ray_gather.comp.spv")),
            vec![
                (s.vertices, Access::ComputeRead),
                (s.faces, Access::ComputeRead),
                (
                    triangles,
                    if first == 0 {
                        Access::ComputeWrite
                    } else {
                        Access::ComputeReadWrite
                    },
                ),
            ],
            push,
            s.count,
        );
        first += s.count;
    }
    triangles
}

pub(crate) enum RayHits {
    /// Keep the native instance and primitive IDs for geometry lookup.
    InstanceGeometry,
    IndexedTriangles,
}

pub(crate) fn native_scene(
    g: &mut FrameGraph,
    surfaces: &[Surface],
    cluster: Option<Resource>,
    hits: RayHits,
) -> Result<(Resource, Vec<Resource>, u32)> {
    let mut meshes = BTreeMap::new();
    let mut instances = Vec::new();
    let mut first = 0u32;
    for source in surfaces {
        let surface = source.geometry;
        let key = (surface.vertices, surface.faces, surface.count);
        let blas = match meshes.entry(key) {
            std::collections::btree_map::Entry::Occupied(e) => *e.get(),
            std::collections::btree_map::Entry::Vacant(e) => {
                let Storage::Buffer { bytes, .. } = g.resources[surface.vertices.0].1 else {
                    anyhow::bail!("ray surface needs vertex buffer")
                };
                let indices = buffer(g, "Packed native ray indices", surface.count as usize * 12);
                dispatch(
                    g,
                    "Pack native ray indices",
                    include_bytes!(concat!(env!("OUT_DIR"), "/ray_indices.comp.spv")),
                    vec![
                        (surface.faces, Access::ComputeRead),
                        (indices, Access::ComputeWrite),
                    ],
                    surface.count.to_ne_bytes().to_vec(),
                    surface.count,
                );
                let blas = g.resource(
                    "Mesh BLAS",
                    Storage::Acceleration(AccelerationShape::Triangles {
                        count: surface.count,
                        vertices: (bytes / 32).try_into()?,
                    }),
                );
                g.pass(
                    "Build triangle BLAS",
                    Op::BuildBlas {
                        vertices: surface.vertices,
                        indices,
                        topology: source.topology,
                        output: blas,
                    },
                );
                *e.insert(blas)
            }
        };
        let transform = surface.model.transpose().to_cols_array()[..12].try_into()?;
        let first_triangle = match hits {
            RayHits::InstanceGeometry => 0,
            RayHits::IndexedTriangles => {
                ensure!(
                    first <= 0x00ff_ffff,
                    "ray hit table exceeds 24-bit instance custom index"
                );
                let base = first;
                first = first
                    .checked_add(surface.count)
                    .context("ray hit table overflow")?;
                base
            }
        };
        instances.push(RayInstance {
            blas,
            transform,
            first_triangle,
        });
    }
    let mut blases: Vec<_> = meshes.into_values().collect();
    if let Some(blas) = cluster {
        instances.push(RayInstance {
            blas,
            transform: Mat4::IDENTITY.transpose().to_cols_array()[..12].try_into()?,
            first_triangle: 0,
        });
        blases.push(blas);
    }
    ensure!(!instances.is_empty(), "native rays require geometry");
    let tlas = g.resource(
        "Scene TLAS",
        Storage::Acceleration(AccelerationShape::Instances {
            count: instances.len().try_into()?,
        }),
    );
    g.pass(
        "Build scene TLAS",
        Op::BuildTlas {
            instances,
            output: tlas,
        },
    );
    Ok((tlas, blases, first))
}

/// The index stream used by mesh draws contains an extra outline word per triangle.
/// Keep the shared vertex buffer intact, adapting only these indices for native BLAS.
pub(crate) fn hardware(g: &mut FrameGraph, s: &Snapshot, view: u32) -> Result<Resource> {
    let surfaces: Vec<_> = g
        .products
        .ray_surfaces
        .iter()
        .copied()
        .filter(|s| s.geometry.count > 0)
        .collect();
    let (tlas, blases, first) = native_scene(g, &surfaces, None, RayHits::IndexedTriangles)?;
    let triangles = gather(g, &surfaces, first, "Hardware inspection triangles");
    let mut camera = bytemuck::bytes_of(&s.view_projection().inverse().to_cols_array()).to_vec();
    camera.extend(bytemuck::bytes_of(&s.camera.eye().extend(1.).to_array()));
    camera.extend(bytemuck::bytes_of(&[view, 0, 0, 0]));
    let camera = g.input("Ray camera and view mode", camera);
    let color = buffer(
        g,
        "Ray-traced BGRA pixels",
        g.extent[0] as usize * g.extent[1] as usize * 4,
    );
    let extent = g.extent;
    g.pass(
        "Trace primary, shadow and reflection rays",
        Op::TraceRays {
            extent,
            program: Dispatch {
                shader: Shader::RayTracing(
                    include_bytes!(concat!(env!("OUT_DIR"), "/ray_preview.rgen.spv")),
                    include_bytes!(concat!(env!("OUT_DIR"), "/ray_preview.rmiss.spv")),
                    include_bytes!(concat!(env!("OUT_DIR"), "/ray_preview.rchit.spv")),
                ),
                bindings: vec![
                    (triangles, Access::RayRead),
                    (camera, Access::RayRead),
                    (color, Access::RayWrite),
                ],
                push: vec![0; 4],
            },
            tlas,
            blases,
        },
    );
    Ok(color)
}

pub(crate) const VIEW: &str = "lab.rays.view";
pub(crate) fn view(s: &Snapshot) -> Option<&str> {
    s.scene
        .setting(VIEW)?
        .as_str()
        .filter(|&v| matches!(v, "hardware" | "software" | "normals" | "distance"))
}
pub(crate) fn inspect(g: &mut FrameGraph, s: &Snapshot, mode: &str) -> Result<Resource> {
    if mode != "software" {
        return hardware(
            g,
            s,
            match mode {
                "normals" => 1,
                "distance" => 2,
                _ => 4,
            },
        );
    }
    let scene = software(g, false)?;
    let [w, h] = g.extent;
    let output = buffer(g, "Software ray inspection", w as usize * h as usize * 4);
    let mut params = s.view_projection().inverse().to_cols_array().to_vec();
    params.extend(s.camera.eye().extend(1.).to_array());
    let cam = g.input("Inspection camera", bytemuck::cast_slice(&params).to_vec());
    dispatch(
        g,
        "Trace software inspection rays",
        include_bytes!(concat!(env!("OUT_DIR"), "/ray_inspect.comp.spv")),
        vec![
            (scene.nodes, Access::ComputeRead),
            (scene.triangles, Access::ComputeRead),
            (cam, Access::ComputeRead),
            (output, Access::ComputeWrite),
        ],
        bytemuck::bytes_of(&[w, h]).to_vec(),
        w.checked_mul(h).context("inspection extent overflow")?,
    );
    Ok(output)
}
