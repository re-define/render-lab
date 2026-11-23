use crate::{rdg::*, scene::*};
use anyhow::{Result, ensure};
use bytemuck::{Pod, Zeroable};
use glam::{Mat3, Mat4, Vec3};
use rayon::prelude::*;
use std::{collections::BTreeMap, sync::Arc};
pub(crate) use visibility::{FrameLighting, MaterialInputs, ResolveInputs};
mod frame_graph;
pub use frame_graph::{FrameGraph, FramePlan, FrameProducts};
mod atlas;
pub(crate) use atlas::{Atlas, AtlasCache};
mod geometry;
mod lights;
mod motion;
pub(crate) mod rays;
pub use geometry::SurfaceBuffers;
pub(crate) use geometry::{MeshCache, PosedSurface};
pub use lights::RenderLight;
pub(crate) use lights::{GpuLight, pack_lights};
pub(crate) use motion::MotionSource;
pub use rays::Surface as RaySurface;
mod scene;
pub use scene::{
    ExtractionStats, GeometryPath, MaterialId, MeshAsset, MeshId, RenderInstance, RenderScene,
    SceneExtractor,
};
pub use scene::{SnapshotComponent, SnapshotContext, SnapshotValue};

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
pub use materials::{LightingInput, MaterialPrograms};
pub mod post;
pub struct Snapshot {
    /// Capabilities of the backend which will execute this snapshot.
    pub buffer_atomic64: bool,
    pub materials: Arc<MaterialPrograms>,
    pub scene: Arc<RenderScene>,
    pub camera: Camera,
    pub extent: [u32; 2],
    pub(crate) temporal: Option<post::TemporalFrame>,
}
impl Snapshot {
    pub fn new(
        world: &World,
        camera: Camera,
        extent: [u32; 2],
        materials: Arc<MaterialPrograms>,
    ) -> Result<Self> {
        SceneExtractor::default().snapshot(world, camera, extent, materials)
    }

    pub fn view_projection(&self) -> Mat4 {
        let vp = self.camera.view_projection(self.output_extent());
        if let Some(frame) = &self.temporal {
            Mat4::from_translation(Vec3::new(
                2. * frame.jitter[0] / self.extent[0] as f32,
                2. * frame.jitter[1] / self.extent[1] as f32,
                0.,
            )) * vp
        } else {
            vp
        }
    }

    pub fn output_extent(&self) -> [u32; 2] {
        self.temporal
            .as_ref()
            .map_or(self.extent, |frame| frame.output_extent)
    }
}
pub trait Technique: Send + Sync {
    fn build(&self, snapshot: &Snapshot) -> Result<FrameGraph>;
    /// Request surface motion even when the post-processing backend is disabled.
    fn needs_surface_motion(&self, _snapshot: &Snapshot) -> bool {
        false
    }
    fn accepts_temporal_inputs(&self, _snapshot: &Snapshot) -> bool {
        true
    }
}
/// Returns triangle IDs in the mesh's index stream. Plugins can select a cluster cut per instance.
pub trait GeometrySelection: Send + Sync {
    /// A contiguous full-mesh selection uses the shared index buffer.
    fn triangles(&self, mesh: &MeshAsset, model: Mat4, view: &Snapshot) -> Result<Vec<u32>>;

    fn selected_triangles(
        &self,
        mesh: &MeshAsset,
        model: Mat4,
        view: &Snapshot,
    ) -> Result<Option<Vec<u32>>> {
        self.triangles(mesh, model, view).map(Some)
    }
}
pub struct AllTriangles;
impl GeometrySelection for AllTriangles {
    fn triangles(&self, e: &MeshAsset, _: Mat4, _: &Snapshot) -> Result<Vec<u32>> {
        if let Some(clusters) = &e.meshlets {
            return Ok(clusters
                .iter()
                .filter(|c| c.refined_group.is_none())
                .flat_map(|c| c.triangles.iter().copied())
                .collect());
        }
        Ok((0..(e.mesh.indices.len() / 3).try_into()?).collect())
    }

    fn selected_triangles(
        &self,
        mesh: &MeshAsset,
        model: Mat4,
        view: &Snapshot,
    ) -> Result<Option<Vec<u32>>> {
        if mesh.meshlets.is_some() {
            self.triangles(mesh, model, view).map(Some)
        } else {
            Ok(None)
        }
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
impl SurfaceInstance {
    /// Bit in tex.w shared with shaders/cluster.glsl.
    pub(crate) const TEMPORAL_REACTIVE: u32 = 1024;
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
pub(crate) struct SurfacePrefix {
    pub count: u32,
    pub shade: Resource,
    pub motion: Option<Resource>,
}
pub(crate) struct Targets {
    pub visibility: Resource,
    pub depth: Resource,
    pub clear: bool,
    texture: Resource,
    pub(crate) background: u32,
    atlas: Arc<Atlas>,
    items: Vec<ShadeItem>,
    ordinary: tables::OrdinarySlots,
    shared_materials: Option<crate::rdg::rows::Rows>,
    tables: Option<(Resource, Resource)>,
    pub(crate) full_upload: bool,
    pub(crate) prefix: Option<SurfacePrefix>,
    motion: Option<motion::Table>,
    addresses: Vec<Resource>,
    meshes: BTreeMap<MeshId, geometry::MeshInputs>,
}
impl Targets {
    pub fn material(
        &self,
        s: &Snapshot,
        i: &RenderInstance,
        model: Mat4,
    ) -> Result<SurfaceInstance> {
        let fallback = Material::default();
        let m = i
            .material
            .and_then(|id| s.scene.materials.get(&id))
            .map(Arc::as_ref)
            .unwrap_or(&fallback);
        let reactive = s
            .temporal
            .as_ref()
            .is_some_and(|frame| i.is_skinned() || frame.moved(i.id));
        let mut result = self.material_value(s, m)?;
        result.model = model.to_cols_array();
        result.tex[3] = u32::from(i.outline)
            | if reactive {
                SurfaceInstance::TEMPORAL_REACTIVE
            } else {
                0
            };
        Ok(result)
    }
    pub(crate) fn material_value(&self, s: &Snapshot, m: &Material) -> Result<SurfaceInstance> {
        let tex = m.texture.map_or([0, 1, 1], |k| self.atlas.locations[&k]);
        Ok(SurfaceInstance {
            model: Mat4::IDENTITY.to_cols_array(),
            color: m.base_color,
            params: [m.metallic, m.roughness, 1., 0.],
            tex: [tex[0], tex[1], tex[2], 0],
            routing: [s.materials.bin(&m.shading)?, 0, 0, 0],
        })
    }
    pub(crate) fn texture_region(&self, id: Id) -> Option<[u32; 3]> {
        self.atlas.locations.get(&id).copied()
    }
    pub fn register(
        &mut self,
        instance: SurfaceInstance,
        geometry: Geometry,
        motion: Option<(motion::SurfaceHistory<'_>, MotionSource)>,
    ) -> Result<u32> {
        let refs = self.geometry_addresses(geometry)?;
        let id = u32::try_from(self.items.len())?
            .checked_add(self.prefix.as_ref().map_or(0, |p| p.count))
            .ok_or_else(|| anyhow::anyhow!("surface ID overflow"))?;
        if let Some(table) = &mut self.motion {
            let (history, source) =
                motion.ok_or_else(|| anyhow::anyhow!("missing temporal surface registration"))?;
            table.register(history, source)?;
        }
        self.items.push(ShadeItem { instance, refs });
        Ok(id)
    }
    fn geometry_addresses(&mut self, geometry: Geometry) -> Result<[u32; 4]> {
        Ok([
            self.address(geometry.vertices)?,
            self.address(geometry.indices)?,
            geometry
                .clusters
                .map(|r| self.address(r))
                .transpose()?
                .unwrap_or(0),
            geometry.clusters.is_some() as u32,
        ])
    }
    pub(crate) fn motion_address(&mut self, r: Resource) -> Result<u32> {
        self.motion.as_mut().expect("temporal geometry").address(r)
    }
    pub(crate) fn address(&mut self, r: Resource) -> Result<u32> {
        if let Some(i) = self.addresses.iter().position(|&v| v == r) {
            return Ok(i.try_into()?);
        }
        let i = self.addresses.len().try_into()?;
        self.addresses.push(r);
        Ok(i)
    }
}
pub(crate) fn raster_plain(
    g: &mut FrameGraph,
    t: &mut Targets,
    s: &Snapshot,
    surface: &SurfaceBuffers,
    item: u32,
) {
    raster_surface(
        g,
        t,
        s,
        surface,
        item,
        Shader::Mesh(
            include_bytes!(concat!(env!("OUT_DIR"), "/plain.mesh.spv")),
            include_bytes!(concat!(env!("OUT_DIR"), "/visibility.frag.spv")),
        ),
        &[],
    );
}

pub(crate) fn raster_surface(
    g: &mut FrameGraph,
    t: &mut Targets,
    s: &Snapshot,
    surface: &SurfaceBuffers,
    item: u32,
    shader: Shader,
    parameters: &[u8],
) {
    let mut push = vec![0; 8];
    push.extend(bytemuck::bytes_of(
        &(s.view_projection() * surface.model).to_cols_array(),
    ));
    push.extend(item.to_ne_bytes());
    push.extend(surface.count.to_ne_bytes());
    push.extend_from_slice(parameters);
    let groups = surface.count.div_ceil(32);
    let program = Dispatch {
        shader,
        bindings: vec![
            (surface.vertices, Access::Mesh),
            (surface.faces, Access::Mesh),
        ],
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
impl VisibilityRenderer {
    pub(crate) fn build_with_atlas(&self, s: &Snapshot, atlas: Arc<Atlas>) -> Result<FrameGraph> {
        build_frame_with_post(
            s,
            atlas,
            |g, t| self.draw_subset(g, t, s, |_| true),
            |_, _, _, _| Ok(()),
        )
    }
}

impl Technique for VisibilityRenderer {
    fn build(&self, s: &Snapshot) -> Result<FrameGraph> {
        self.build_with_atlas(s, Atlas::cached(&s.scene)?)
    }
}

pub(crate) fn build_frame_with_post(
    s: &Snapshot,
    atlas: Arc<Atlas>,
    build: impl FnOnce(&mut FrameGraph, &mut Targets) -> Result<()>,
    post: impl FnOnce(&mut FrameGraph, Resource, Resource, Resource) -> Result<()>,
) -> Result<FrameGraph> {
    build_frame_with_lighting(s, atlas, build, |_, _| Ok(FrameLighting::default()), post)
}

pub(crate) fn build_frame_with_lighting(
    s: &Snapshot,
    atlas: Arc<Atlas>,
    build: impl FnOnce(&mut FrameGraph, &mut Targets) -> Result<()>,
    lighting: impl FnOnce(&mut FrameGraph, ResolveInputs) -> Result<FrameLighting>,
    post: impl FnOnce(&mut FrameGraph, Resource, Resource, Resource) -> Result<()>,
) -> Result<FrameGraph> {
    let mut g = FrameGraph::new(s.extent);
    g.display_extent = (s.output_extent() != s.extent).then(|| s.output_extent());
    let mut t = Targets {
        background: 0xff090d13,
        texture: g.resident_input(atlas.label, atlas.pixels.clone()),
        atlas,
        visibility: g.resource("Visibility IDs", Storage::Color),
        depth: g.resource("Scene depth", Storage::Depth),
        clear: true,
        items: vec![],
        ordinary: Default::default(),
        tables: None,
        shared_materials: None,
        full_upload: false,
        prefix: None,
        motion: s.temporal.as_ref().map(|_| motion::Table::default()),
        addresses: vec![],
        meshes: BTreeMap::new(),
    };
    let ray_view = rays::view(s);
    g.products.request_ray_proxies = ray_view.is_some();
    g.products.material_atlas = Some(t.texture);
    build(&mut g, &mut t)?;
    if let Some(mode) = ray_view {
        let output = rays::inspect(&mut g, s, mode)?;
        g.set_output(output);
        return Ok(g);
    }
    if t.clear {
        let empty = g.input("Empty vertices", vec![]);
        let idx = g.input("Empty indices", vec![]);
        raster_plain(
            &mut g,
            &mut t,
            s,
            &SurfaceBuffers {
                vertices: empty,
                faces: idx,
                count: 0,
                model: Mat4::IDENTITY,
            },
            0,
        );
    }
    let depth = t.depth;
    let (inputs, color, lighting) = visibility::resolve(&mut g, t, s, lighting)?;
    g.products.visibility = Some(inputs.ids);
    g.products.depth = Some(depth);
    g.products.motion = inputs.motion;
    post(&mut g, inputs.ids, depth, color)?;
    let mut color = post::ColorInput {
        resource: color,
        encoding: if lighting.linear_hdr {
            post::ColorEncoding::LinearRgba32
        } else {
            post::ColorEncoding::Bgra8
        },
    };
    if let Some((motion, frame)) = inputs
        .motion
        .zip(s.temporal.as_ref())
        .filter(|(_, f)| f.has_post())
    {
        color = post::resolve(
            &mut g,
            frame,
            post::Inputs {
                color,
                depth,
                motion,
            },
        )?;
    }
    let color = if color.encoding == post::ColorEncoding::LinearRgba32 {
        visibility::tone_map(&mut g, color.resource)
    } else {
        color.resource
    };
    g.set_output(color);
    Ok(g)
}
mod tables;
mod visibility;
