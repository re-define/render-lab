use super::*;
use crate::rdg::rows::Rows;
use std::sync::Mutex;

/// A camera-independent cut through real triangle clusters, retaining disconnected leaves.
/// The GPU reads the exact source vertex skinning attributes alongside these indices.
pub(in crate::experiments::clusters) struct Detail {
    triangles: Arc<[u8]>,
    center: Vec3,
    radius: f32,
    near: f32,
    selection: Mutex<Option<Selection>>,
}
struct Block {
    low: Vec3,
    high: Vec3,
    spheres: Vec<(Vec3, f32)>,
}
struct Selection {
    models: Rows,
    layout: Arc<[u8]>,
    blocks: Vec<Block>,
    spatial_ids: Vec<u32>,
    cameras: [CameraSelection; 2],
}
#[derive(Default)]
struct CameraSelection {
    eye: Option<Vec3>,
    near: Vec<u32>,
    order: Arc<[u8]>,
}
// The software BVH stores successive sets of eight faces per leaf.
// Spatial median partitions keep its internal bounds tight in a dense canopy.
fn spatial_order<T>(items: &mut [(T, Vec3)], leaf_size: usize) {
    if items.len() <= leaf_size {
        return;
    }
    let (lo, hi) = items.iter().fold(
        (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
        |(lo, hi), (_, p)| (lo.min(*p), hi.max(*p)),
    );
    let size = hi - lo;
    let axis = if size.x >= size.y && size.x >= size.z {
        0
    } else if size.y >= size.z {
        1
    } else {
        2
    };
    let mid = items.len() / 2;
    items.select_nth_unstable_by(mid, |a, b| a.1[axis].total_cmp(&b.1[axis]));
    let (a, b) = items.split_at_mut(mid);
    spatial_order(a, leaf_size);
    spatial_order(b, leaf_size);
}

impl Detail {
    pub(in crate::experiments::clusters) fn new(asset: &MeshAsset) -> Self {
        let mesh = &asset.mesh;
        let dag = asset.hierarchy.as_ref().unwrap();
        let clusters = asset.meshlets.as_ref().unwrap();
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = -lo;
        for &p in &mesh.positions {
            let p = Vec3::from(p);
            lo = lo.min(p);
            hi = hi.max(p);
        }
        let tree = hi.y - lo.y > 1.2;
        let threshold = if tree { 0.045 } else { 0.0 };
        let mut triangles = Vec::new();
        // Use the same monotonic triangle errors as raster, before any voxel substitutions.
        let groups = bvh::repaired(dag, clusters, None).expect("validated cluster DAG");
        let mut root = vec![true; dag.len()];
        for child in clusters.iter().filter_map(|c| c.refined_group) {
            root[child as usize] = false;
        }
        for (id, group) in dag.iter().enumerate() {
            if !root[id] && groups[id].error < threshold {
                continue;
            }
            for &idx in &group.meshlets {
                let cluster = &clusters[idx as usize];
                if cluster
                    .refined_group
                    .is_some_and(|c| groups[c as usize].error >= threshold)
                {
                    continue;
                }
                for &face in &cluster.triangles {
                    let t = face as usize * 3;
                    let ids = [mesh.indices[t], mesh.indices[t + 1], mesh.indices[t + 2]];
                    let [a, b, c] = ids.map(|v| Vec3::from(mesh.positions[v as usize]));
                    triangles.push((ids, (a + b + c) / 3.));
                }
            }
        }
        spatial_order(&mut triangles, 8);
        let faces: Vec<_> = triangles.into_iter().map(|(tri, _)| tri).collect();
        let wind = asset.plant.as_ref().unwrap().displacement_bound;
        Self {
            triangles: bytemuck::cast_slice(&faces).into(),
            center: (lo + hi) * 0.5,
            radius: (hi - lo).length() * 0.5 + wind,
            near: if tree { 32. } else { 22. },
            selection: Mutex::default(),
        }
    }
    pub(in crate::experiments::clusters::gpu) fn batches(
        &self,
        g: &mut FrameGraph,
        s: &Snapshot,
        slots: &[usize],
        first: u32,
        vertices: Resource,
        coarse: Arc<[u8]>,
    ) -> Vec<Batch> {
        let mut cache = self.selection.lock().unwrap();
        let stale = cache.as_ref().is_none_or(|c| {
            !c.models.identical(&s.scene.models)
                || !Arc::ptr_eq(&c.layout, &s.scene.routes.bindings)
        });
        if stale {
            let blocks: Vec<_> = slots
                .chunks(32)
                .map(|slice| {
                    let mut low = Vec3::splat(f32::INFINITY);
                    let mut high = -low;
                    let spheres = slice
                        .iter()
                        .map(|&i| {
                            let model = s.scene.instances[i].model;
                            let center = model.transform_point3(self.center);
                            let scale = cl::cut::max_scale(
                                glam::Mat3::from_mat4(model).transpose().to_cols_array_2d(),
                            );
                            let radius = self.radius * scale;
                            low = low.min(center - radius);
                            high = high.max(center + radius);
                            (center, radius)
                        })
                        .collect();
                    Block { low, high, spheres }
                })
                .collect();
            let mut centers: Vec<_> = blocks
                .iter()
                .flat_map(|b| b.spheres.iter())
                .enumerate()
                .map(|(id, &(center, _))| (id as u32, center))
                .collect();
            spatial_order(&mut centers, 1);
            let spatial_ids = centers.into_iter().map(|(id, _)| id).collect();
            *cache = Some(Selection {
                spatial_ids,
                models: s.scene.models.clone(),
                layout: s.scene.routes.bindings.clone(),
                blocks,
                cameras: Default::default(),
            });
        }
        let c = cache
            .as_mut()
            .unwrap()
            .for_eye(s.camera.eye(), self.near, s.secondary_view);
        let n = slots.len() as u32;
        let k = c.near.len() as u32;
        if k == 0 || self.triangles.is_empty() {
            return vec![Batch::coarse(coarse, first, n)];
        }
        let order = g.frame_input("Vegetation shadow instance order", c.order.clone());
        let mut batches = vec![Batch {
            data: self.triangles.clone(),
            first,
            n: k,
            indices: Some((order, 0)),
            plant_vertices: Some(vertices),
        }];
        if k < n {
            batches.push(Batch {
                indices: Some((order, k)),
                ..Batch::coarse(coarse, first, n - k)
            });
        }
        batches
    }
}

impl Selection {
    fn for_eye(&mut self, eye: Vec3, threshold: f32, secondary: bool) -> &CameraSelection {
        let camera = &mut self.cameras[usize::from(secondary)];
        if camera.eye != Some(eye) {
            let mut near = Vec::new();
            for (block, b) in self.blocks.iter().enumerate() {
                if eye.clamp(b.low, b.high).distance_squared(eye) > (threshold + 2.).powi(2) {
                    continue;
                }
                for (i, &(pos, radius)) in b.spheres.iter().enumerate() {
                    let id = (block * 32 + i) as u32;
                    let margin = if camera.near.binary_search(&id).is_ok() {
                        2.
                    } else {
                        0.
                    };
                    if eye.distance_squared(pos) <= (threshold + margin + radius).powi(2) {
                        near.push(id);
                    }
                }
            }
            if camera.near != near {
                let (mut order, far): (Vec<_>, Vec<_>) = self
                    .spatial_ids
                    .iter()
                    .copied()
                    .partition(|id| near.binary_search(id).is_ok());
                order.extend(far);
                camera.order = bytemuck::cast_slice(&order).into();
                camera.near = near;
            }
            camera.eye = Some(eye);
        }
        camera
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn two_cameras_keep_independent_shadow_hysteresis_around_the_same_tree() {
        let create = || Selection {
            models: Rows::words(),
            layout: Arc::default(),
            blocks: vec![Block {
                low: -Vec3::ONE,
                high: Vec3::ONE,
                spheres: vec![(Vec3::ZERO, 1.)],
            }],
            spatial_ids: vec![0],
            cameras: Default::default(),
        };
        let (mut primary, mut secondary, mut paired) = (create(), create(), create());
        // A tree enters fine at 33 m and exits past 35 m. Both cameras revisit the band
        // from opposite sides, with a different history at each visit.
        for (main, second, expected) in [
            (34., 32., [false, true]),
            (34., 34., [false, true]),
            (32., 36., [true, false]),
            (34., 34., [true, false]),
        ] {
            let eye = |z| Vec3::Z * z;
            let selected = |scene: &mut Selection, z, second| {
                !scene.for_eye(eye(z), 32., second).near.is_empty()
            };
            assert_eq!(
                [
                    selected(&mut primary, main, false),
                    selected(&mut secondary, second, false)
                ],
                expected
            );
            assert_eq!(
                [
                    selected(&mut paired, main, false),
                    selected(&mut paired, second, true)
                ],
                expected
            );
        }
    }
}
