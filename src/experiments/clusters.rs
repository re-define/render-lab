//! Resident cluster LOD with GPU spatial traversal and two-phase visibility rasterization.
use crate::{rdg::*, render::*, scene::*};
use anyhow::{Result, ensure};
use glam::{Mat4, Vec3};
use meshoptimizer_rs::{Workspace, clusterlod as cl};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
mod bvh;
mod dragons;
mod foliage;
mod gpu;
pub use dragons::scene;
pub const VISUALIZE: &str = "lab.cluster_lod.visualize";
pub const ORDINARY: &str = "lab.cluster_lod.ordinary";
/// RGB colors of the meshlet generation shown in the viewport legend.
pub const LOD_COLORS: [u32; 10] = [
    0x39d49b, 0x4ea5ff, 0xffce4b, 0xfa7645, 0xcc6ff6, 0x20cfd9, 0xf46ea5, 0x8ad24a, 0xaa91fa,
    0xf9eed5,
];
pub const SETTING: &str = "lab.cluster_lod.pixel_error";
pub(super) const SNAPSHOT: crate::render::SnapshotComponent = crate::render::SnapshotComponent {
    related_settings: &[VISUALIZE],
    ..crate::render::SnapshotComponent::setting(SETTING)
};

pub fn prepare(world: &mut World) -> Result<()> {
    let mut staged = world.clone();
    for e in staged.entities.values_mut().filter(|e| {
        e.mesh.is_some()
            && e.hierarchy.is_none()
            && !e
                .components
                .get(ORDINARY)
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
    }) {
        let mesh = Arc::make_mut(e.mesh.as_mut().unwrap());
        if mesh.indices.is_empty() {
            continue;
        }
        let source_faces = mesh.indices.len() / 3;
        let config = cl::default_config(64)?;
        let groups = cl::build(
            config,
            cl::Mesh {
                indices: &mesh.indices,
                positions: &mut mesh.positions,
                attributes: None,
                vertex_lock: None,
                attribute_weights: &[],
                attribute_protect_mask: 0,
            },
            &mut Workspace::default(),
        )?;
        // Leaf clusters reuse source face IDs so adjacency components survive preprocessing.
        let mut faces = std::collections::HashMap::<[u32; 3], Vec<u32>>::new();
        let key = |a: u32, b: u32, c: u32| [a, b, c].min([b, c, a]).min([c, a, b]);
        for (i, t) in mesh.indices.chunks_exact(3).enumerate() {
            faces
                .entry(key(t[0], t[1], t[2]))
                .or_default()
                .push(i as u32);
        }
        let mut clusters = vec![];
        let mut nodes = vec![];
        for g in groups {
            let b = g.group.simplified;
            let mut node = ClusterNode {
                meshlets: vec![],
                center: b.center,
                radius: b.radius,
                error: b.error,
            };
            for c in g.clusters {
                let mut triangles = Vec::with_capacity(c.indices.len() / 3);
                for t in c.indices.chunks_exact(3) {
                    let source = (c.refined < 0)
                        .then(|| faces.get_mut(&key(t[0], t[1], t[2])).and_then(Vec::pop))
                        .flatten();
                    let face = match source {
                        Some(i) => i,
                        None => {
                            let i = (mesh.indices.len() / 3).try_into()?;
                            mesh.indices.extend_from_slice(t);
                            i
                        }
                    };
                    triangles.push(face);
                }
                node.meshlets.push(clusters.len() as u32);
                clusters.push(Meshlet {
                    triangles,
                    refined_group: c.refined.try_into().ok(),
                });
            }
            nodes.push(node);
        }
        if let Some(plant) = e
            .plant
            .as_mut()
            .filter(|plant| plant.voxel_cell_size.is_some())
        {
            let expansion = foliage::preserve_area(mesh, source_faces, &mut nodes, &clusters)?;
            // Any additional rest-space offset can move by at most twice its length when posed.
            Arc::make_mut(plant).displacement_bound += 2. * expansion;
        }
        let fixed = bvh::repaired(&nodes, &clusters, None)?;
        for (node, fixed) in nodes.iter_mut().zip(fixed) {
            node.center = fixed.center;
            node.radius = fixed.radius;
            node.error = fixed.error;
        }
        e.meshlets = Some(Arc::new(clusters));
        e.hierarchy = Some(Arc::new(nodes));
    }
    if staged.first_raw_component(SETTING).is_none() {
        let mut settings = Entity {
            name: "Cluster LOD".into(),
            ..Default::default()
        };
        settings.components.insert(SETTING.into(), 1.0.into());
        staged.spawn(settings);
    }
    staged.validate()?;
    *world = staged;
    Ok(())
}
pub fn terrain() -> Result<World> {
    let mut world = World::default();
    let n = 64u32;
    let mut positions = vec![];
    let mut indices = vec![];
    for y in 0..=n {
        for x in 0..=n {
            let u = x as f32 / n as f32;
            let v = y as f32 / n as f32;
            let h = 0.6 * (u * 11.).sin() * (v * 8.).cos() + 0.15 * (v * 37. + u * 25.).sin();
            positions.push([(u - 0.5) * 8., h, (v - 0.5) * 8.]);
        }
    }
    for y in 0..n {
        for x in 0..n {
            let a = y * (n + 1) + x;
            indices.extend([a, a + n + 1, a + 1, a + 1, a + n + 1, a + n + 2]);
        }
    }
    let mesh = world.spawn(Entity {
        name: "Ridged terrain".into(),
        mesh: Some(Arc::new(Mesh {
            positions,
            indices,
            normals: vec![],
            uv: vec![],
            joints: vec![],
            weights: vec![],
        })),
        ..Default::default()
    });
    let material = world.spawn(Entity {
        material: Some(Material {
            base_color: [0.2, 0.62, 0.51, 1.],
            ..Default::default()
        }),
        ..Default::default()
    });
    world.spawn(Entity {
        name: "Terrain".into(),
        instance: Some(Instance {
            outline: true,
            ..Instance::new(mesh, Some(material))
        }),
        ..Default::default()
    });
    prepare(&mut world)?;
    Ok(world)
}
pub struct ClusterLod {
    cache: Mutex<BTreeMap<MeshId, Arc<bvh::Packed>>>,
    history: u64,
    full_upload: bool,
}
impl Default for ClusterLod {
    fn default() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            cache: Mutex::default(),
            full_upload: false,
            history: NEXT.fetch_add(1, Ordering::Relaxed),
        }
    }
}
impl ClusterLod {
    /// Full CPU packing baseline for image and upload comparisons.
    pub fn full_upload_reference() -> Self {
        Self {
            full_upload: true,
            ..Default::default()
        }
    }
}
impl GeometrySelection for ClusterLod {
    fn triangles(&self, e: &MeshAsset, model: Mat4, s: &Snapshot) -> Result<Vec<u32>> {
        let (Some(groups), Some(clusters)) = (&e.hierarchy, &e.meshlets) else {
            return AllTriangles.triangles(e, model, s);
        };
        let threshold = s
            .scene
            .setting(SETTING)
            .and_then(|x| x.as_f64())
            .unwrap_or(1.) as f32;
        ensure!(
            threshold.is_finite() && threshold >= 0.,
            "pixel error must be finite and non-negative"
        );
        let scale = cl::cut::max_scale(glam::Mat3::from_mat4(model).transpose().to_cols_array_2d());
        let focal = s.extent[1] as f32 * 0.5 / 25f32.to_radians().tan();
        let mut refine = vec![true; groups.len()];
        for (index, g) in groups.iter().enumerate().rev() {
            let center = model.transform_point3(Vec3::from(g.center));
            let distance =
                (s.camera.eye().distance(center) - g.radius * scale).max(s.camera.distance * 0.001);
            refine[index] &= threshold == 0.
                || g.error == f32::MAX
                || g.error * scale * focal / distance > threshold;
            if !refine[index] {
                for &c in &g.meshlets {
                    if let Some(child) = clusters[c as usize].refined_group {
                        refine[child as usize] = false;
                    }
                }
            }
        }
        Ok(groups
            .iter()
            .enumerate()
            .filter(|(i, _)| refine[*i])
            .flat_map(|(_, g)| &g.meshlets)
            .map(|&c| &clusters[c as usize])
            .filter(|c| c.refined_group.is_none_or(|g| !refine[g as usize]))
            .flat_map(|c| c.triangles.iter().copied())
            .collect())
    }
}
