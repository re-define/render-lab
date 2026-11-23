//! Distant shadow proxies and near source-triangle batches. A crown is several separated volumes; each
//! follows a representative joint from the same prototype and wind palette as raster.
use super::*;
use crate::render::{MeshAsset, SurfaceBuffers, rays};
pub(super) fn prototype(asset: &MeshAsset) -> Arc<[u8]> {
    let mesh = &asset.mesh;
    if asset.plant.is_none() {
        // Preserve the shape of rigid clustered meshes with the coarsest complete DAG cut.
        let groups = asset.hierarchy.as_ref().expect("cluster hierarchy");
        let clusters = asset.meshlets.as_ref().expect("meshlets");
        let children: std::collections::BTreeSet<_> =
            clusters.iter().filter_map(|c| c.refined_group).collect();
        let mut points = Vec::new();
        for (_, group) in groups
            .iter()
            .enumerate()
            .filter(|(i, _)| !children.contains(&(*i as u32)))
        {
            for &slot in &group.meshlets {
                for &face in &clusters[slot as usize].triangles {
                    for &v in &mesh.indices[face as usize * 3..face as usize * 3 + 3] {
                        points.push(Vec3::from(mesh.positions[v as usize]).extend(0.).to_array());
                        let [u, v] = mesh.uv.get(v as usize).copied().unwrap_or_default();
                        points.push([u, v, 0., 0.]);
                    }
                }
            }
        }
        return bytemuck::cast_slice(&points).into();
    }
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = -lo;
    for &p in &mesh.positions {
        let p = Vec3::from(p);
        lo = lo.min(p);
        hi = hi.max(p);
    }
    let size = (hi - lo).max(Vec3::splat(1e-4));
    let mut cells = BTreeMap::<u32, (Vec3, Vec3, BTreeMap<u16, u32>)>::new();
    for (i, &pos) in mesh.positions.iter().enumerate() {
        let p = Vec3::from(pos);
        let joint = mesh.joints.get(i).map_or(0, |j| j[0]);
        let uv = (p - lo) / size;
        let key = match &asset.plant {
            Some(plant) if plant.voxel_cell_size.is_some() && size.y > 1.2 => {
                let c = (uv * Vec3::new(3., 2., 3.))
                    .as_uvec3()
                    .min(glam::UVec3::new(2, 1, 2));
                c.x + 3 * c.y + 6 * c.z
            }
            Some(_) if size.y > 1.2 => u32::from(joint),
            _ => 0,
        };
        let (low, high, hist) = cells.entry(key).or_insert_with(|| (p, p, BTreeMap::new()));
        *low = low.min(p);
        *high = high.max(p);
        *hist.entry(joint).or_default() += 1;
    }
    let mut vertices = vec![];
    for (_, (lo, hi, hist)) in cells {
        let joint = hist.into_iter().max_by_key(|v| v.1).unwrap().0;
        let c = (lo + hi) * 0.5;
        let r = ((hi - lo) * 0.49).max(Vec3::splat(0.018));
        let ring = [Vec3::X, Vec3::Z, Vec3::NEG_X, Vec3::NEG_Z];
        for i in 0..4 {
            let (a, b) = (ring[i], ring[(i + 1) % 4]);
            for d in [Vec3::Y, b, a, Vec3::NEG_Y, a, b] {
                vertices.push(
                    (c + d * r)
                        .extend(f32::from_bits(u32::from(joint)))
                        .to_array(),
                );
                let pos = c + d * r;
                // The volume proxy has its own cylindrical UV chart in rest space.
                vertices.push([
                    d.z.atan2(d.x) / std::f32::consts::TAU + 0.5,
                    (pos.y - lo.y) / (2. * r.y),
                    0.,
                    0.,
                ]);
            }
        }
    }
    bytemuck::cast_slice(&vertices).into()
}
mod detail;
pub(in crate::experiments::clusters) use detail::Detail;

pub(super) struct Batch {
    data: Arc<[u8]>,
    first: u32,
    n: u32,
    indices: Option<(Resource, u32)>,
    plant_vertices: Option<Resource>,
}
impl Batch {
    pub(super) fn coarse(data: Arc<[u8]>, first: u32, n: u32) -> Self {
        Self {
            data,
            first,
            n,
            indices: None,
            plant_vertices: None,
        }
    }
    fn face_counts(&self) -> Result<(u32, u32)> {
        let stride = if self.plant_vertices.is_some() {
            12
        } else {
            96
        };
        let per = u32::try_from(self.data.len() / stride)?;
        let total = per
            .checked_mul(self.n)
            .context("ray proxy face count overflow")?;
        Ok((per, total))
    }

    pub(super) fn stats(&self) -> Result<(usize, u32, u32)> {
        let (_, total) = self.face_counts()?;
        Ok((
            if self.plant_vertices.is_some() { 0 } else { 2 },
            self.n,
            total,
        ))
    }
    pub(super) fn publish(
        self,
        g: &mut FrameGraph,
        mesh: MeshId,
        instances: Resource,
        layout: Resource,
        palette: Option<Resource>,
    ) -> Result<()> {
        let (per, count) = self.face_counts()?;
        let Self {
            data,
            first,
            n,
            indices,
            plant_vertices,
        } = self;
        if count == 0 {
            return Ok(());
        }
        let src = g.resident_input("Plant shadow proxy prototype", data);
        let palette = palette.unwrap_or_else(|| {
            g.input(
                "Ray proxy rest pose",
                bytemuck::bytes_of(&Mat4::IDENTITY.to_cols_array()).to_vec(),
            )
        });
        let (vertex_name, face_name) = if plant_vertices.is_some() {
            ("Detailed shadow vertices", "Detailed shadow faces")
        } else {
            ("Coarse shadow vertices", "Coarse shadow faces")
        };
        let vertices = g.scratch_buffer(vertex_name, mesh.0, count as usize * 96);
        let faces = g.scratch_buffer(face_name, mesh.0, count as usize * 16);
        let mut bindings = vec![
            (instances, R),
            (src, R),
            (palette, R),
            (vertices, W),
            (faces, W),
            (layout, R),
        ];
        let mut push = vec![first, n, per];
        let mut shader: &'static [u8] = spirv!("plant_proxy.comp");
        if let Some((order, start)) = indices {
            bindings.push((order, R));
            push.push(start);
            shader = spirv!("plant_proxy_indexed.comp");
        }
        let label = if let Some(source) = plant_vertices {
            bindings.push((source, R));
            shader = spirv!("plant_shadow_detail.comp");
            "Deform detailed vegetation shadow triangles"
        } else {
            "Deform offscreen and visible ray proxies"
        };
        rays::dispatch(
            g,
            label,
            shader,
            bindings,
            bytemuck::cast_slice(&push).to_vec(),
            count,
        );
        let mut surface = rays::Surface::new(
            SurfaceBuffers {
                vertices,
                faces,
                count,
                model: Mat4::IDENTITY,
            },
            [0.16, 0.28, 0.09, 1.],
            0.,
            true,
            u32::MAX, // generated faces carry the per-instance material slot
        );
        // The GPU expands every proxy face to vertices 3i, 3i+1, 3i+2.
        // Its indices stay fixed for this prototype and count while positions change in wind.
        surface.topology = src;
        g.products.ray_surfaces.push(surface);
        Ok(())
    }
}
