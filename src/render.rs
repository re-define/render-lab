use crate::{rdg::*, scene::*};
use anyhow::{Result, ensure};
use bytemuck::{Pod, Zeroable};
use glam::{Mat3, Mat4, Vec3};
use rayon::prelude::*;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Copy)]
pub struct Camera {
    pub target: Vec3,
    pub distance: f32,
    pub yaw: f32,
    pub pitch: f32,
}
impl Default for Camera {
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            distance: 5.5,
            yaw: 0.35,
            pitch: 0.2,
        }
    }
}
impl Camera {
    pub fn eye(self) -> Vec3 {
        self.target
            + self.distance
                * Vec3::new(
                    self.yaw.sin() * self.pitch.cos(),
                    self.pitch.sin(),
                    self.yaw.cos() * self.pitch.cos(),
                )
    }
    pub fn translate(&mut self, side: f32, forward: f32, dt: f32) {
        let direction = (self.target - self.eye()).normalize_or_zero();
        let right = direction.cross(Vec3::Y).normalize_or_zero();
        let delta = (direction * forward + right * side).clamp_length_max(1.);
        self.target += delta * (self.distance * 0.6).max(0.1) * dt;
    }
    pub fn look(&mut self, dx: f32, dy: f32) {
        let eye = self.eye();
        self.yaw -= dx;
        self.pitch = (self.pitch + dy).clamp(-1.55, 1.55);
        self.target += eye - self.eye();
    }
    pub fn view_projection(self, [w, h]: [u32; 2]) -> Mat4 {
        let mut p = Mat4::perspective_rh(
            50f32.to_radians(),
            w as f32 / h as f32,
            (self.distance * 0.001).max(0.0001),
            self.distance * 1000.,
        );
        p.y_axis.y *= -1.;
        p * Mat4::look_at_rh(self.eye(), self.target, Vec3::Y)
    }
    pub fn fit(world: &World) -> Result<Self> {
        let transforms = world.transforms()?;
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = -lo;
        for (id, e) in &world.entities {
            if let Some(i) = &e.instance {
                let mesh = world.entities[&i.mesh].mesh.as_ref().unwrap();
                let palette = joint_palette(world, e, &transforms);
                for (k, &v) in mesh.positions.iter().enumerate() {
                    let p = vertex_matrix(mesh, k, transforms[id], palette.as_deref())?
                        .transform_point3(v.into());
                    lo = lo.min(p);
                    hi = hi.max(p);
                }
            }
        }
        Ok(if lo.is_finite() {
            Self {
                target: (lo + hi) * 0.5,
                distance: (hi - lo).length().max(0.1) * 1.4,
                ..Default::default()
            }
        } else {
            Self::default()
        })
    }
}
fn joint_palette(w: &World, e: &Entity, transforms: &BTreeMap<Id, Mat4>) -> Option<Vec<Mat4>> {
    e.skin_binding
        .and_then(|s| w.entities[&s].skin.as_ref())
        .map(|skin| {
            skin.joints
                .iter()
                .zip(&skin.inverse_bind)
                .map(|(j, ib)| transforms[j] * *ib)
                .collect::<Vec<_>>()
        })
}
fn vertex_matrix(mesh: &Mesh, k: usize, model: Mat4, palette: Option<&[Mat4]>) -> Result<Mat4> {
    if let Some(p) = palette
        && let Some(weights) = mesh.weights.get(k)
    {
        let mut sum = 0.;
        let mut m = Mat4::ZERO;
        for (&j, &v) in mesh.joints[k].iter().zip(weights) {
            if v != 0. {
                ensure!((j as usize) < p.len(), "joint index out of range");
                m += p[j as usize] * v;
                sum += v;
            }
        }
        if sum > 0. {
            return Ok(m * (1. / sum));
        }
    }
    Ok(model)
}
mod materials;
pub use materials::MaterialPrograms;
pub struct Snapshot {
    pub materials: Arc<MaterialPrograms>,
    pub world: Arc<World>,
    pub camera: Camera,
    pub extent: [u32; 2],
}
pub trait Technique: Send + Sync {
    fn build(&self, snapshot: &Snapshot) -> Result<Graph>;
}
/// Returns triangle IDs in the mesh's index stream. Plugins can select a cluster cut per instance.
pub trait GeometrySelection: Send + Sync {
    fn triangles(&self, mesh: &Entity, model: Mat4, view: &Snapshot) -> Result<Vec<u32>>;
}
pub struct AllTriangles;
impl GeometrySelection for AllTriangles {
    fn triangles(&self, e: &Entity, _: Mat4, _: &Snapshot) -> Result<Vec<u32>> {
        if let Some(clusters) = &e.meshlets {
            return Ok(clusters
                .iter()
                .filter(|c| c.refined_group.is_none())
                .flat_map(|c| c.triangles.iter().copied())
                .collect());
        }
        Ok((0..(e.mesh.as_ref().unwrap().indices.len() / 3).try_into()?).collect())
    }
}
/// Ordinary geometry front end for the visibility renderer.
pub type Forward = VisibilityRenderer;
pub struct VisibilityRenderer {
    pub selection: Arc<dyn GeometrySelection>,
}
impl Default for VisibilityRenderer {
    fn default() -> Self {
        Self {
            selection: Arc::new(AllTriangles),
        }
    }
}
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub(crate) struct SurfaceInstance {
    pub model: [f32; 16],
    pub color: [f32; 4],
    pub params: [f32; 4],
    pub tex: [u32; 4],
    pub routing: [u32; 4],
}
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct ShadeItem {
    instance: SurfaceInstance,
    refs: [u32; 4],
}
#[derive(Clone, Copy)]
pub(crate) struct Geometry {
    pub vertices: Resource,
    pub indices: Resource,
    pub clusters: Option<Resource>,
}
pub(crate) struct Targets {
    pub visibility: Resource,
    pub depth: Resource,
    pub clear: bool,
    texture: Resource,
    pub(crate) background: u32,
    atlas: Arc<Atlas>,
    items: Vec<ShadeItem>,
    addresses: Vec<Resource>,
}
impl Targets {
    pub fn material(&self, s: &Snapshot, i: &Instance, model: Mat4) -> Result<SurfaceInstance> {
        let m = i
            .material
            .and_then(|id| s.world.entities[&id].material.as_ref())
            .cloned()
            .unwrap_or_default();
        let tex = m.texture.map_or([0, 1, 1], |k| self.atlas.locations[&k]);
        Ok(SurfaceInstance {
            model: model.to_cols_array(),
            color: m.base_color,
            params: [m.metallic, m.roughness, 1., 0.],
            tex: [tex[0], tex[1], tex[2], u32::from(i.outline)],
            routing: [s.materials.bin(&m.shading)?, 0, 0, 0],
        })
    }
    pub(crate) fn texture_region(&self, id: Id) -> Option<[u32; 3]> {
        self.atlas.locations.get(&id).copied()
    }
    pub fn register(&mut self, instance: SurfaceInstance, geometry: Geometry) -> Result<u32> {
        let mut address = |r| -> Result<u32> {
            if let Some(i) = self.addresses.iter().position(|&v| v == r) {
                return Ok(i.try_into()?);
            }
            let i = self.addresses.len().try_into()?;
            self.addresses.push(r);
            Ok(i)
        };
        let refs = [
            address(geometry.vertices)?,
            address(geometry.indices)?,
            geometry
                .clusters
                .map(&mut address)
                .transpose()?
                .unwrap_or(0),
            geometry.clusters.is_some() as u32,
        ];
        let id = self.items.len().try_into()?;
        self.items.push(ShadeItem { instance, refs });
        Ok(id)
    }
}
pub(crate) struct Atlas {
    locations: BTreeMap<Id, [u32; 3]>,
    pixels: Arc<[u8]>,
    sources: Vec<(Id, Arc<Texture>)>,
    label: &'static str,
}
impl Atlas {
    pub(crate) fn matches(&self, world: &World) -> bool {
        self.sources
            .iter()
            .map(|(id, t)| (*id, Arc::as_ptr(t)))
            .eq(world
                .entities
                .iter()
                .filter_map(|(&id, e)| Some((id, Arc::as_ptr(e.texture.as_ref()?)))))
    }
    pub(crate) fn from(world: &World) -> Result<Self> {
        Self::with_payloads(world, &BTreeMap::new(), "Base color texels")
    }
    /// Material kernels can sample their own word-aligned representation from the same atlas.
    pub(crate) fn with_payloads(
        world: &World,
        encoded: &BTreeMap<Id, &[u8]>,
        label: &'static str,
    ) -> Result<Self> {
        let mut locations = BTreeMap::new();
        let mut pixels = vec![255; 4];
        let mut sources = vec![];
        for (&id, e) in &world.entities {
            if let Some(t) = &e.texture {
                locations.insert(id, [(pixels.len() / 4).try_into()?, t.width, t.height]);
                let data = encoded.get(&id).copied().unwrap_or(&t.rgba);
                ensure!(data.len().is_multiple_of(4), "unaligned material payload");
                pixels.extend_from_slice(data);
                sources.push((id, t.clone()));
            }
        }
        Ok(Self {
            locations,
            pixels: pixels.into(),
            sources,
            label: if encoded.is_empty() {
                "Base color texels"
            } else {
                label
            },
        })
    }
}
pub(crate) struct PosedGeometry {
    vertices: Vec<[f32; 8]>,
    faces: Vec<[u32; 4]>,
}
impl VisibilityRenderer {
    fn geometry(
        &self,
        s: &Snapshot,
        transforms: &BTreeMap<Id, Mat4>,
        id: Id,
        e: &Entity,
        i: &Instance,
    ) -> Result<PosedGeometry> {
        let w = &s.world;
        let geom = &w.entities[&i.mesh];
        let mesh = geom.mesh.as_ref().unwrap();
        let model = transforms[&id];
        let palette = joint_palette(w, e, transforms);
        // Transform each shared vertex once, including its skin and normal matrix.
        let rigid_normal = Mat3::from_mat4(model).inverse().transpose();
        let posed = mesh
            .positions
            .iter()
            .enumerate()
            .map(|(k, p)| {
                let matrix = vertex_matrix(mesh, k, model, palette.as_deref())?;
                let position = matrix.transform_point3((*p).into());
                let normal = mesh
                    .normals
                    .get(k)
                    .map(|n| {
                        let normal_matrix = if palette.is_none() {
                            rigid_normal
                        } else {
                            Mat3::from_mat4(matrix).inverse().transpose()
                        };
                        (normal_matrix * Vec3::from(*n)).normalize_or_zero()
                    })
                    .unwrap_or_default();
                Ok((position, normal))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut edge_masks = BTreeMap::<u32, u32>::new();
        if i.outline
            && let Some(edges) = &geom.topology
        {
            let mut facing = Vec::with_capacity(mesh.indices.len() / 3);
            for tri in mesh.indices.chunks_exact(3) {
                let mut points = [Vec3::ZERO; 3];
                for (dst, &v) in points.iter_mut().zip(tri) {
                    *dst = posed[v as usize].0;
                }
                let n = (points[1] - points[0])
                    .cross(points[2] - points[0])
                    .normalize_or_zero();
                facing.push((n, n.dot(s.camera.eye() - points[0]) >= 0.));
            }
            for edge in edges.iter() {
                if let [a, b] = edge.faces.as_slice() {
                    let (na, fa) = facing[*a as usize];
                    let (nb, fb) = facing[*b as usize];
                    if fa == fb && na.dot(nb) > 0.75 {
                        continue;
                    }
                }
                for &face in &edge.faces {
                    let tri = &mesh.indices[face as usize * 3..][..3];
                    if let Some(j) = tri.iter().position(|v| !edge.vertices.contains(v)) {
                        *edge_masks.entry(face).or_default() |= 1 << j;
                    }
                }
            }
        }
        let triangles = self.selection.triangles(geom, model, s)?;
        let faces = triangles
            .into_iter()
            .map(|t| -> Result<[u32; 4]> {
                let mask = if geom.topology.is_some() {
                    edge_masks.get(&t).copied().unwrap_or(0)
                } else {
                    7
                };
                let face = mesh
                    .indices
                    .get(t as usize * 3..)
                    .and_then(|v| v.get(..3))
                    .ok_or_else(|| anyhow::anyhow!("invalid selected triangle {t}"))?;
                Ok([face[0], face[1], face[2], mask])
            })
            .collect::<Result<_>>()?;
        let vertices = posed
            .into_iter()
            .enumerate()
            .map(|(k, (p, n))| {
                let [u, v] = mesh.uv.get(k).copied().unwrap_or_default();
                [p.x, p.y, p.z, u, n.x, n.y, n.z, v]
            })
            .collect();
        Ok(PosedGeometry { vertices, faces })
    }
    pub(crate) fn draw_subset(
        &self,
        g: &mut Graph,
        t: &mut Targets,
        s: &Snapshot,
        transforms: &BTreeMap<Id, Mat4>,
        include: impl Fn(&Entity, &Instance) -> bool,
    ) -> Result<()> {
        for (i, posed) in self.posed_surfaces(s, transforms, include)? {
            let material = t.material(s, i, Mat4::IDENTITY)?;
            posed.draw(g, t, s, material)?;
        }
        Ok(())
    }

    pub(crate) fn posed_surfaces<'a>(
        &self,
        s: &'a Snapshot,
        transforms: &BTreeMap<Id, Mat4>,
        include: impl Fn(&Entity, &Instance) -> bool,
    ) -> Result<Vec<(&'a Instance, PosedGeometry)>> {
        let instances = s
            .world
            .entities
            .iter()
            .filter_map(|(&id, e)| {
                e.instance
                    .as_ref()
                    .filter(|i| include(e, i))
                    .map(|i| (id, e, i))
            })
            .collect::<Vec<_>>();
        instances
            .par_iter()
            .map(|&(id, e, i)| Ok((i, self.geometry(s, transforms, id, e, i)?)))
            .collect()
    }
}
pub(crate) struct SurfaceBuffers {
    pub vertices: Resource,
    pub faces: Resource,
    pub count: u32,
}
impl PosedGeometry {
    pub(crate) fn draw(
        self,
        g: &mut Graph,
        t: &mut Targets,
        s: &Snapshot,
        material: SurfaceInstance,
    ) -> Result<SurfaceBuffers> {
        let count = self.faces.len().try_into()?;
        let vertices = g.input(
            "Posed vertices",
            bytemuck::cast_slice(&self.vertices).to_vec(),
        );
        let faces = g.input(
            "Triangle indices and edge masks",
            bytemuck::cast_slice(&self.faces).to_vec(),
        );
        let item = t.register(
            material,
            Geometry {
                vertices,
                indices: faces,
                clusters: None,
            },
        )?;
        raster_plain(g, t, s, vertices, faces, item, count);
        Ok(SurfaceBuffers {
            vertices,
            faces,
            count,
        })
    }
}
fn raster_plain(
    g: &mut Graph,
    t: &mut Targets,
    s: &Snapshot,
    vertices: Resource,
    indices: Resource,
    item: u32,
    count: u32,
) {
    let mut push = vec![0; 8];
    push.extend(bytemuck::bytes_of(
        &s.camera.view_projection(s.extent).to_cols_array(),
    ));
    push.extend(item.to_ne_bytes());
    push.extend(count.to_ne_bytes());
    let groups = count.div_ceil(32);
    let program = Dispatch {
        shader: Shader::Mesh(
            include_bytes!(concat!(env!("OUT_DIR"), "/plain.mesh.spv")),
            include_bytes!(concat!(env!("OUT_DIR"), "/visibility.frag.spv")),
        ),
        bindings: vec![(vertices, Access::Mesh), (indices, Access::Mesh)],
        push,
    };
    g.pass(
        "Ordinary mesh visibility",
        Op::Draw {
            program,
            color: t.visibility,
            depth: t.depth,
            clear: t.clear,
            count: DrawCount::Mesh([groups.min(32768), groups.div_ceil(32768), 1]),
        },
    );
    t.clear = false;
}
impl Technique for VisibilityRenderer {
    fn build(&self, s: &Snapshot) -> Result<Graph> {
        build_frame(s, |g, t, transforms| {
            self.draw_subset(g, t, s, transforms, |_, _| true)
        })
    }
}
pub(crate) fn build_frame(
    s: &Snapshot,
    build: impl FnOnce(&mut Graph, &mut Targets, &BTreeMap<Id, Mat4>) -> Result<()>,
) -> Result<Graph> {
    build_frame_with_post(s, Arc::new(Atlas::from(&s.world)?), build, |_, _, _, _| {
        Ok(())
    })
}

pub(crate) fn build_frame_with_post(
    s: &Snapshot,
    atlas: Arc<Atlas>,
    build: impl FnOnce(&mut Graph, &mut Targets, &BTreeMap<Id, Mat4>) -> Result<()>,
    post: impl FnOnce(&mut Graph, Resource, Resource, Resource) -> Result<()>,
) -> Result<Graph> {
    let mut g = Graph::new(s.extent);
    let transforms = s.world.transforms()?;
    let mut t = Targets {
        background: 0xff090d13,
        texture: g.resource(
            atlas.label,
            Storage::Buffer {
                bytes: atlas.pixels.len().max(16),
                initial: Some(atlas.pixels.clone()),
            },
        ),
        atlas,
        visibility: g.resource("Visibility IDs", Storage::Color),
        depth: g.resource("Scene depth", Storage::Depth),
        clear: true,
        items: vec![],
        addresses: vec![],
    };
    g.material_atlas = Some(t.texture);
    build(&mut g, &mut t, &transforms)?;
    if t.clear {
        let empty = g.input("Empty vertices", vec![]);
        let idx = g.input("Empty indices", vec![]);
        raster_plain(&mut g, &mut t, s, empty, idx, 0, 0);
    }
    let depth = t.depth;
    let (ids, color) = visibility::resolve(&mut g, t, s)?;
    post(&mut g, ids, depth, color)?;
    g.pass("Capture viewport", Op::HostRead(color));
    Ok(g)
}
mod visibility;
