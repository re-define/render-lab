//! Frame-local acceleration and hit data shared by visibility and closest-hit queries.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Traversal {
    Software,
    Pipeline,
    Inline,
}
pub(crate) enum RayScene {
    Software {
        nodes: Resource,
        triangles: Resource,
        deformed: Software,
    },
    Hardware(NativeScene),
}

pub(crate) struct NativeScene {
    pub(crate) inline_queries: bool,
    tlas: Resource,
    blases: Vec<Resource>,
    pub(crate) cluster: Option<ClusterShadow>,
    // Vertex/index addresses and material slots in TLAS instance order.
    pub(crate) hit_streams: Option<[Resource; 2]>,
}

impl RayScene {
    pub(crate) fn build(
        g: &mut FrameGraph,
        traversal: Traversal,
        hit_attributes: bool,
        rigid: Option<[Arc<[u8]>; 2]>,
    ) -> Result<Self> {
        Self::build_geometry(g, traversal, hit_attributes, rigid, false)
    }

    /// Purely rigid proxies leave the GPU-deformed geometry identical for both ray purposes.
    pub(crate) fn into_material_hits(
        self,
        g: &mut FrameGraph,
        traversal: Traversal,
        rigid: Option<[Arc<[u8]>; 2]>,
    ) -> Result<Self> {
        if let Self::Software { deformed, .. } = self {
            let [nodes, triangles] = rigid.unwrap_or_default();
            Ok(Self::Software {
                nodes: g.resident_input("GI visual BVH", nodes),
                triangles: g.resident_input("GI visual triangles", triangles),
                deformed,
            })
        } else {
            Self::build(g, traversal, true, rigid)
        }
    }

    pub(crate) fn occluders(
        g: &mut FrameGraph,
        traversal: Traversal,
        rigid: Option<[Arc<[u8]>; 2]>,
    ) -> Result<Self> {
        Self::build_geometry(g, traversal, false, rigid, true)
    }

    fn build_geometry(
        g: &mut FrameGraph,
        traversal: Traversal,
        hit_attributes: bool,
        rigid: Option<[Arc<[u8]>; 2]>,
        occlusion: bool,
    ) -> Result<Self> {
        if traversal != Traversal::Software {
            let cluster = g.products.cluster_shadow;
            let surfaces: Vec<_> = g
                .products
                .ray_surfaces
                .iter()
                .enumerate()
                .filter(|(i, s)| {
                    s.geometry.count > 0
                        && !cluster.is_some_and(|c| (c.proxy_start..c.proxy_end).contains(i))
                })
                .map(|(_, s)| if occlusion { s.occluder() } else { *s })
                .collect();
            if !surfaces.is_empty() || cluster.is_some() {
                let (tlas, blases, _) = native_scene(
                    g,
                    &surfaces,
                    cluster.map(|c| c.blas),
                    RayHits::InstanceGeometry,
                )?;
                let hit_streams = if hit_attributes {
                    let pointers = surfaces
                        .iter()
                        .flat_map(|s| [s.geometry.vertices, s.geometry.faces])
                        .collect();
                    let mut slots = vec![u32::try_from(surfaces.len())?];
                    slots.extend(surfaces.iter().map(|s| s.material_slot));
                    Some([
                        g.address_table(pointers),
                        g.input(
                            "GI ray instance material slots",
                            bytemuck::cast_slice(&slots).to_vec(),
                        ),
                    ])
                } else {
                    None
                };
                return Ok(Self::Hardware(NativeScene {
                    inline_queries: traversal == Traversal::Inline,
                    tlas,
                    blases,
                    cluster,
                    hit_streams,
                }));
            }
        }
        // An empty scene needs no TLAS. Zero-length node arrays make every ray miss.
        let [nodes, triangles] = rigid.unwrap_or_default();
        Ok(Self::Software {
            nodes: g.resident_input("Many-light shadow BVH", nodes),
            triangles: g.resident_input("Shadow caster triangles", triangles),
            deformed: software(g, true)?,
        })
    }
}

impl NativeScene {
    pub(crate) fn inline(
        &self,
        groups: [u32; 3],
        shader: &'static [u8],
        bindings: Vec<(Resource, Access)>,
    ) -> Op {
        Op::RayQuery {
            groups,
            tlas: self.tlas,
            blases: self.blases.clone(),
            program: Dispatch {
                shader: Shader::Compute(shader),
                push: vec![0; 4 * (bindings.len() % 2)],
                bindings,
            },
        }
    }
    pub(crate) fn trace(
        &self,
        extent: [u32; 2],
        shader: Shader,
        bindings: Vec<(Resource, Access)>,
    ) -> Op {
        // The TLAS device address follows the heap indices at an eight-byte boundary.
        let push = vec![0; 4 * (bindings.len() % 2)];
        Op::TraceRays {
            extent,
            tlas: self.tlas,
            blases: self.blases.clone(),
            program: Dispatch {
                shader,
                bindings,
                push,
            },
        }
    }
}
