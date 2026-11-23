use super::*;
use crate::render::{PosedSurface, RenderScene};
use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use std::sync::Mutex;

pub(in crate::experiments) struct Packed {
    pub triangles: Arc<[u8]>,
    pub acceleration: Arc<[u8]>,
    pub count: u32,
}

#[derive(Default)]
pub(in crate::experiments) struct Cache(Mutex<Option<Cached>>);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::experiments) enum Packing {
    Shadow,
    SoftwareLighting,
}

struct Cached {
    sources: Vec<Source>,
    packing: Packing,
    triangle_sources: Vec<usize>,
    packed: Arc<Packed>,
}

struct Source {
    mesh: Arc<Mesh>,
    model: Mat4,
    selected: Option<Vec<u32>>,
    color: [f32; 4],
    material_slot: u32,
}

impl Source {
    fn geometry_matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.mesh, &other.mesh)
            && self.model == other.model
            && self.selected == other.selected
    }
}

impl Cache {
    pub fn get(
        &self,
        scene: &RenderScene,
        surfaces: &[&PosedSurface<'_>],
        packing: Packing,
    ) -> Result<Arc<Packed>> {
        let sources = surfaces
            .iter()
            .map(|surface| {
                let instance = surface.instance;
                let material = instance
                    .material
                    .and_then(|id| scene.materials.get(&id))
                    .filter(|_| packing != Packing::Shadow);
                let mut color = material.map_or(Material::default().base_color, |m| m.base_color);
                color[3] = material.map_or(0., |m| m.metallic);
                Source {
                    material_slot: instance
                        .material
                        .map_or(0, |id| scene.routes.material_slots[&id] as u32),
                    mesh: surface.geometry.mesh.clone(),
                    model: instance.model,
                    selected: surface.geometry.selected.clone(),
                    color,
                }
            })
            .collect::<Vec<_>>();
        let mut cache = self.0.lock().unwrap();
        if let Some(entry) = cache.as_mut().filter(|entry| {
            entry.packing == packing
                && entry.sources.len() == sources.len()
                && entry
                    .sources
                    .iter()
                    .zip(&sources)
                    .all(|(old, new)| old.geometry_matches(new))
        }) {
            if entry
                .sources
                .iter()
                .zip(&sources)
                .any(|(old, new)| old.color != new.color || old.material_slot != new.material_slot)
            {
                let triangles: Vec<_> = entry
                    .packed
                    .triangles
                    .chunks_exact(size_of::<Triangle>())
                    .zip(&entry.triangle_sources)
                    .map(|(bytes, &owner)| {
                        let mut triangle: Triangle = bytemuck::pod_read_unaligned(bytes);
                        triangle.color = sources[owner].color;
                        triangle.material[0] = sources[owner].material_slot;
                        triangle
                    })
                    .collect();
                entry.packed = Arc::new(Packed {
                    triangles: bytemuck::cast_slice(&triangles).into(),
                    acceleration: entry.packed.acceleration.clone(),
                    count: entry.packed.count,
                });
                entry.sources = sources;
            }
            return Ok(entry.packed.clone());
        }
        let mut triangles = vec![];
        for (owner, (surface, source)) in surfaces.iter().zip(&sources).enumerate() {
            for vertices in surface.geometry.triangles()? {
                triangles.push((
                    Triangle {
                        positions: vertices.map(|v| [v[0], v[1], v[2], v[3]]),
                        normals: vertices.map(|v| [v[4], v[5], v[6], v[7]]),
                        color: source.color,
                        material: [source.material_slot, 0, 0, 0],
                    },
                    owner,
                ));
            }
        }
        let acceleration = bytemuck::cast_slice(&tree(&mut triangles)).into();
        let (triangles, triangle_sources): (Vec<_>, Vec<_>) = triangles.into_iter().unzip();
        let packed = Arc::new(Packed {
            triangles: bytemuck::cast_slice(&triangles).into(),
            acceleration,
            count: triangles.len().try_into()?,
        });
        *cache = Some(Cached {
            sources,
            packing,
            triangle_sources,
            packed: packed.clone(),
        });
        Ok(packed)
    }
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct Triangle {
    pub positions: [[f32; 4]; 3],
    pub normals: [[f32; 4]; 3],
    pub color: [f32; 4],
    pub material: [u32; 4],
}

pub(in crate::experiments) fn shadow_casters<'a, 'b>(
    surfaces: &'a [PosedSurface<'b>],
) -> impl Iterator<Item = &'a PosedSurface<'b>> {
    surfaces.iter().filter(|surface| !surface.instance.emitter)
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct Node {
    low: [f32; 3],
    first: u32,
    high: [f32; 3],
    count: u32,
}

fn emit(triangles: &mut [(Triangle, usize)], nodes: &mut Vec<Node>, first: u32) {
    let (mut low, mut high) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for (triangle, _) in triangles.iter() {
        for p in triangle.positions {
            let p = Vec3::from_slice(&p);
            low = low.min(p);
            high = high.max(p);
        }
    }
    let node = nodes.len();
    nodes.push(Node {
        low: (low - 0.00005).to_array(),
        high: (high + 0.00005).to_array(),
        first,
        count: triangles.len() as u32,
    });
    if triangles.len() > 6 {
        let extent = high - low;
        let axis = if extent.x >= extent.y.max(extent.z) {
            0
        } else {
            usize::from(extent.z > extent.y) + 1
        };
        let centroid = |triangle: &Triangle| {
            triangle.positions[0][axis] + triangle.positions[1][axis] + triangle.positions[2][axis]
        };
        let mid = triangles.len() / 2;
        triangles.select_nth_unstable_by(mid, |a, b| centroid(&a.0).total_cmp(&centroid(&b.0)));
        let (left, right) = triangles.split_at_mut(mid);
        emit(left, nodes, first);
        emit(right, nodes, first + mid as u32);
        nodes[node].first = nodes.len() as u32;
        nodes[node].count = 0;
    }
}

fn tree(triangles: &mut [(Triangle, usize)]) -> Vec<Node> {
    let mut nodes = vec![];
    if !triangles.is_empty() {
        emit(triangles, &mut nodes, 0);
    }
    nodes
}
