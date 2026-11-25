use super::*;
use crate::rdg::PassPhase;
use std::sync::{Mutex, Weak};

mod skin;
pub(super) use skin::SkinBatches;

#[derive(Default)]
pub(crate) struct MeshCache(Mutex<BTreeMap<MeshId, MeshSource>>);

struct MeshSource {
    mesh: Weak<Mesh>,
    vertices: Arc<[u8]>,
    faces: Arc<[u8]>,
    influences: Arc<[u8]>,
    last_joint: Option<u16>,
}

#[derive(Clone, Copy)]
pub(crate) struct MeshInputs {
    vertices: Resource,
    faces: Resource,
    influences: Option<Resource>,
    last_joint: Option<u16>,
}

fn packed_vertex(mesh: &Mesh, vertex: usize, position: Vec3, normal: Vec3) -> [f32; 8] {
    let [u, v] = mesh.uv.get(vertex).copied().unwrap_or_default();
    [
        position.x, position.y, position.z, u, normal.x, normal.y, normal.z, v,
    ]
}

impl MeshSource {
    fn new(mesh: &Arc<Mesh>) -> Self {
        let vertices: Vec<_> = mesh
            .positions
            .iter()
            .enumerate()
            .map(|(index, &position)| {
                packed_vertex(
                    mesh,
                    index,
                    position.into(),
                    mesh.normals.get(index).copied().unwrap_or_default().into(),
                )
            })
            .collect();
        let faces: Vec<_> = mesh
            .indices
            .chunks_exact(3)
            .map(|f| [f[0], f[1], f[2], 7])
            .collect();
        let mut last_joint = None;
        let influences: Vec<_> = mesh
            .joints
            .iter()
            .zip(&mesh.weights)
            .map(|(joints, weights)| {
                let mut vertex = [0u32; 8];
                for i in 0..4 {
                    vertex[i] = u32::from(joints[i]);
                    vertex[4 + i] = weights[i].to_bits();
                    if weights[i] != 0. {
                        last_joint = last_joint.max(Some(joints[i]));
                    }
                }
                vertex
            })
            .collect();
        Self {
            mesh: Arc::downgrade(mesh),
            vertices: bytemuck::cast_slice(&vertices).into(),
            faces: bytemuck::cast_slice(&faces).into(),
            influences: bytemuck::cast_slice(&influences).into(),
            last_joint,
        }
    }
}

impl MeshCache {
    pub(crate) fn rigid_streams(
        &self,
        id: MeshId,
        mesh: &Arc<Mesh>,
        graph: &mut FrameGraph,
    ) -> (Resource, Resource) {
        let streams = self.import(id, mesh, graph);
        (streams.vertices, streams.faces)
    }

    fn import(&self, id: MeshId, mesh: &Arc<Mesh>, g: &mut FrameGraph) -> MeshInputs {
        let mut cache = self.0.lock().unwrap();
        let current = Arc::downgrade(mesh);
        if !cache
            .get(&id)
            .is_some_and(|entry| entry.mesh.ptr_eq(&current))
        {
            cache.retain(|_, entry| entry.mesh.strong_count() != 0);
            cache.insert(id, MeshSource::new(mesh));
        }
        let entry = &cache[&id];
        let mut input = |label, data: &Arc<[u8]>| g.resident_input(label, data.clone());
        MeshInputs {
            vertices: input("Resident mesh vertices", &entry.vertices),
            faces: input("Resident mesh faces", &entry.faces),
            influences: (!entry.influences.is_empty())
                .then(|| input("Resident joint indices and weights", &entry.influences)),
            last_joint: entry.last_joint,
        }
    }
}

pub(crate) struct PosedSurface<'a> {
    pub instance: &'a RenderInstance,
    pub geometry: SurfaceGeometry<'a>,
}

impl PosedSurface<'_> {
    pub(crate) fn draw(
        self,
        g: &mut FrameGraph,
        t: &mut Targets,
        s: &Snapshot,
        material: SurfaceInstance,
    ) -> Result<SurfaceBuffers> {
        self.geometry.draw(g, t, s, self.instance, material)
    }
}

pub(crate) struct SurfaceGeometry<'a> {
    mesh_id: MeshId,
    pub(crate) mesh: &'a Arc<Mesh>,
    model: Mat4,
    palette: Option<&'a Arc<[Mat4]>>,
    pub(crate) selected: Option<Vec<u32>>,
    edges: Option<&'a [Edge]>,
}

#[derive(Clone, Copy, Debug)]
pub struct SurfaceBuffers {
    pub vertices: Resource,
    pub faces: Resource,
    pub count: u32,
    pub model: Mat4,
}

impl VisibilityRenderer {
    pub(crate) fn posed_surfaces<'a>(
        &self,
        s: &'a Snapshot,
        include: impl Fn(&RenderInstance) -> bool + Sync,
    ) -> Result<Vec<PosedSurface<'a>>> {
        // Collect instances in slot order so equal-depth primitives keep their draw order.
        let instances: Vec<_> = s
            .scene
            .instances
            .iter()
            .filter(|instance| include(instance))
            .collect();
        self.prepare_surfaces(s, instances)
    }
    pub(crate) fn prepare_surfaces<'a>(
        &self,
        s: &'a Snapshot,
        instances: Vec<&'a Arc<RenderInstance>>,
    ) -> Result<Vec<PosedSurface<'a>>> {
        instances
            .par_iter()
            .map(|&instance| {
                let asset = &s.scene.meshes[&instance.mesh];
                let mesh = &asset.mesh;
                let model = instance.model;
                let mut selected = self.selection.selected_triangles(asset, model, s)?;
                if let Some(chosen) = &selected {
                    let count: u32 = (mesh.indices.len() / 3).try_into()?;
                    ensure!(
                        chosen.iter().all(|&tri| tri < count),
                        "invalid selected triangle"
                    );
                    if chosen.iter().copied().eq(0..count) {
                        selected = None;
                    }
                }
                Ok(PosedSurface {
                    instance,
                    geometry: SurfaceGeometry {
                        mesh_id: instance.mesh,
                        mesh,
                        model,
                        selected,
                        palette: instance.palette.as_ref(),
                        edges: if instance.outline {
                            asset.topology.as_ref().map(|edges| edges.as_slice())
                        } else {
                            None
                        },
                    },
                })
            })
            .collect()
    }

    pub(crate) fn draw_subset(
        &self,
        g: &mut FrameGraph,
        t: &mut Targets,
        s: &Snapshot,
        include: impl Fn(&RenderInstance) -> bool + Sync,
    ) -> Result<()> {
        g.products.instance_visits += s.scene.instances.len();
        for surface in self.posed_surfaces(s, include)? {
            let material = t.material(s, surface.instance, Mat4::IDENTITY)?;
            surface.draw(g, t, s, material)?;
        }
        Ok(())
    }
}

impl SurfaceGeometry<'_> {
    pub(crate) fn ray_stream(
        &self,
        g: &mut FrameGraph,
        s: &Snapshot,
        imports: &mut BTreeMap<MeshId, MeshInputs>,
    ) -> Result<SurfaceBuffers> {
        let inputs = *imports
            .entry(self.mesh_id)
            .or_insert_with(|| s.scene.mesh_cache.import(self.mesh_id, self.mesh, g));
        let (vertices, model) = self.current(g, inputs)?;
        let faces = if self.selected.is_some() {
            let faces: Vec<_> = self
                .selected()
                .map(|f| {
                    let [a, b, c] = self.indices(f);
                    [a, b, c, 7u32]
                })
                .collect();
            g.input("Selected ray faces", bytemuck::cast_slice(&faces).to_vec())
        } else {
            inputs.faces
        };
        Ok(SurfaceBuffers {
            vertices,
            faces,
            count: self.count().try_into()?,
            model,
        })
    }
    fn count(&self) -> usize {
        self.selected
            .as_ref()
            .map_or(self.mesh.indices.len() / 3, Vec::len)
    }

    fn selected(&self) -> impl Iterator<Item = u32> + '_ {
        (0..self.count()).map(|i| {
            self.selected
                .as_ref()
                .map_or(i as u32, |selected| selected[i])
        })
    }

    fn indices(&self, face: u32) -> [u32; 3] {
        let start = face as usize * 3;
        self.mesh.indices[start..start + 3].try_into().unwrap()
    }

    /// Cached rigid triangles for static software lighting and shadow casters.
    /// The visibility renderer consumes the GPU streams from draw().
    pub(crate) fn triangles(&self) -> Result<impl Iterator<Item = [[f32; 8]; 3]> + '_> {
        ensure!(
            self.palette.is_none(),
            "deformed ray geometry must use GPU surface streams"
        );
        let rigid_normal = Mat3::from_mat4(self.model).inverse().transpose();
        let vertices: Vec<_> = self
            .mesh
            .positions
            .iter()
            .enumerate()
            .map(|(index, &position)| {
                let position = self.model.transform_point3(position.into());
                let normal = self
                    .mesh
                    .normals
                    .get(index)
                    .map(|&normal| (rigid_normal * Vec3::from(normal)).normalize_or_zero())
                    .unwrap_or_default();
                Ok(packed_vertex(self.mesh, index, position, normal))
            })
            .collect::<Result<_>>()?;
        Ok(self
            .selected()
            .map(move |face| self.indices(face).map(|vertex| vertices[vertex as usize])))
    }

    fn current(&self, g: &mut FrameGraph, inputs: MeshInputs) -> Result<(Resource, Mat4)> {
        Ok(if let Some(palette) = self.palette {
            ensure!(
                inputs
                    .last_joint
                    .is_none_or(|joint| usize::from(joint) < palette.len()),
                "joint index out of range"
            );
            (
                self.skin(g, inputs, self.model, palette, false)?,
                Mat4::IDENTITY,
            )
        } else {
            (inputs.vertices, self.model)
        })
    }

    fn draw(
        self,
        g: &mut FrameGraph,
        t: &mut Targets,
        s: &Snapshot,
        instance: &RenderInstance,
        mut material: SurfaceInstance,
    ) -> Result<SurfaceBuffers> {
        let inputs = *t
            .meshes
            .entry(self.mesh_id)
            .or_insert_with(|| s.scene.mesh_cache.import(self.mesh_id, self.mesh, g));
        let count = self.count().try_into()?;
        let (vertices, model) = self.current(g, inputs)?;
        let faces = if let Some(edges) = self.edges {
            self.outline_masks(g, s, vertices, model, edges)?
        } else if self.selected.is_some() {
            let selected: Vec<_> = self
                .selected()
                .map(|id| {
                    let [a, b, c] = self.indices(id);
                    [a, b, c, 7]
                })
                .collect();
            g.input(
                "Selected mesh faces",
                bytemuck::cast_slice(&selected).to_vec(),
            )
        } else {
            inputs.faces
        };
        let motion = if let Some(history) = s.surface_history(instance)? {
            let source = match (history.previous, self.palette) {
                (Some(prior), Some(_)) => {
                    let palette = prior.palette.as_ref().unwrap();
                    if inputs
                        .last_joint
                        .is_none_or(|joint| usize::from(joint) < palette.len())
                    {
                        MotionSource::Skinned(self.skin(g, inputs, prior.model, palette, true)?)
                    } else {
                        MotionSource::Unavailable
                    }
                }
                (Some(prior), None) => MotionSource::rigid(self.model, prior.model),
                (None, _) => MotionSource::Unavailable,
            };
            Some((history, source))
        } else {
            None
        };
        material.model = model.to_cols_array();
        let item = t.register_scene(
            s,
            instance,
            material,
            Geometry {
                vertices,
                indices: faces,
                clusters: None,
            },
            motion,
        )?;
        let surface = SurfaceBuffers {
            vertices,
            faces,
            count,
            model,
        };
        if !instance.emitter {
            g.products.ray_surfaces.push(rays::Surface::new(
                SurfaceBuffers {
                    faces: if self.selected.is_none() {
                        inputs.faces
                    } else {
                        faces
                    },
                    ..surface
                },
                material.color,
                material.params[0],
                self.palette.is_some(),
                instance
                    .material
                    .map_or(0, |m| s.scene.routes.material_slots[&m] as u32),
            ));
        }
        raster_plain(g, t, s, &surface, item);
        Ok(surface)
    }

    fn skin(
        &self,
        g: &mut FrameGraph,
        input: MeshInputs,
        model: Mat4,
        palette: &Arc<[Mat4]>,
        previous: bool,
    ) -> Result<Resource> {
        let count = self.mesh.positions.len().try_into()?;
        g.skins
            .add(&mut g.graph, input, count, model, palette, previous)
    }

    fn outline_masks(
        &self,
        g: &mut FrameGraph,
        s: &Snapshot,
        vertices: Resource,
        model: Mat4,
        edges: &[Edge],
    ) -> Result<Resource> {
        let count: u32 = self.count().try_into()?;
        let mut tests = BTreeMap::<u32, Vec<[u32; 4]>>::new();
        let mut boundary = BTreeMap::<u32, u32>::new();
        for edge in edges {
            for &id in &edge.faces {
                let tri = self.indices(id);
                if let Some(corner) = tri.iter().position(|i| !edge.vertices.contains(i)) {
                    let bit = 1u32 << corner;
                    if let [a, b] = edge.faces.as_slice() {
                        let [x, y, z] = self.indices(if id == *a { *b } else { *a });
                        tests.entry(id).or_default().push([x, y, z, bit]);
                    } else {
                        *boundary.entry(id).or_default() |= bit;
                    }
                }
            }
        }
        let mut records = vec![[0u32; 4]; count as usize * 2];
        for (i, id) in self.selected().enumerate() {
            let [a, b, c] = self.indices(id);
            records[2 * i] = [a, b, c, boundary.get(&id).copied().unwrap_or_default()];
            if let Some(neighbors) = tests.get(&id) {
                records[2 * i + 1] = [records.len().try_into()?, neighbors.len().try_into()?, 0, 0];
                records.extend_from_slice(neighbors);
            }
        }
        let records = g.input(
            "Outline neighbor triangles",
            bytemuck::cast_slice(&records).to_vec(),
        );
        let mut params = model.to_cols_array().to_vec();
        params.extend(s.camera.eye().extend(1.).to_array());
        let view = g.input(
            "Outline model and eye",
            bytemuck::cast_slice(&params).to_vec(),
        );
        let output = g.resource(
            "Faces with GPU outline masks",
            Storage::Buffer {
                bytes: (count as usize * 16).max(16),
                initial: None,
            },
        );
        let groups = count.div_ceil(64);
        g.pass_in(
            PassPhase::Overlay,
            "Classify mesh outline edges",
            Op::Compute {
                program: Dispatch {
                    shader: Shader::Compute(include_bytes!(concat!(
                        env!("OUT_DIR"),
                        "/outline_edges.comp.spv"
                    ))),
                    bindings: vec![
                        (vertices, Access::ComputeRead),
                        (records, Access::ComputeRead),
                        (view, Access::ComputeRead),
                        (output, Access::ComputeWrite),
                    ],
                    push: count.to_ne_bytes().to_vec(),
                },
                groups: [groups.min(32768), groups.div_ceil(32768), 1],
            },
        );
        Ok(output)
    }
}
