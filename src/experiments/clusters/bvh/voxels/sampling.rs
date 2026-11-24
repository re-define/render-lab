//! Bake binary occupancy and representative attributes from the same bounded ray hits.
use super::{Cell, ChildCut, Mesh, SourceSurface, Vec2, Vec3, positions, triangle};
use std::collections::BTreeMap;

pub(super) const RAYS: usize = 48;
const MAX_CANDIDATE_CELLS: usize = 32_768;

struct Face {
    index: [usize; 3],
    a: Vec3,
    normal: Vec3,
    bary_u: Vec3,
    bary_v: Vec3,
}

impl Face {
    fn hit(&self, origin: Vec3, direction: Vec3, length: f32) -> Option<(f32, Vec3)> {
        let denom = self.normal.dot(direction);
        if denom.abs() < 1e-8 {
            return None;
        }
        let t = self.normal.dot(self.a - origin) / denom;
        if !(0.0..length).contains(&t) {
            return None;
        }
        let offset = origin + t * direction - self.a;
        let u = offset.dot(self.bary_u);
        let v = offset.dot(self.bary_v);
        let bary = Vec3::new(1. - u - v, u, v);
        (bary.min_element() >= -1e-6).then_some((t, bary))
    }
}

#[derive(Clone, Copy)]
struct Ray {
    origin: Vec3,
    direction: Vec3,
    length: f32,
}

fn hash(mut i: u32) -> u32 {
    i ^= i >> 16;
    i = i.wrapping_mul(0x7feb352d);
    i ^= i >> 15;
    i = i.wrapping_mul(0x846ca68b);
    i ^ (i >> 16)
}

fn rays(seed: u32) -> [Ray; RAYS] {
    // Equal-area cube faces with cosine-weighted incident directions sample isotropic flux.
    // Stratify the entry positions and keep the pattern fixed in asset space.
    std::array::from_fn(|r| {
        let axis = r / 16;
        let sign = if r % 16 < 8 { 1. } else { -1. };
        let u = (axis + 1) % 3;
        let v = (axis + 2) % 3;
        let random = |n| ((hash(seed ^ ((r as u32) * 4 + n)) >> 8) as f32 + 0.5) / 16777216.;
        let mut origin = Vec3::ZERO;
        origin[axis] = if sign > 0. { 0. } else { 1. };
        origin[u] = ((r % 4) as f32 + random(0)) / 4.;
        origin[v] = ((r / 4 % 2) as f32 + random(1)) / 2.;
        let radius2 = random(2);
        let (sin, cos) = (std::f32::consts::TAU * random(3)).sin_cos();
        let mut direction = Vec3::ZERO;
        direction[axis] = sign * (1. - radius2).sqrt();
        direction[u] = radius2.sqrt() * cos;
        direction[v] = radius2.sqrt() * sin;
        let mut length = f32::INFINITY;
        for a in 0..3 {
            if direction[a] != 0. {
                let end = if direction[a] > 0. { 1. } else { 0. };
                length = length.min((end - origin[a]) / direction[a]);
            }
        }
        Ray {
            origin,
            direction,
            length,
        }
    })
}

fn dominant_joint(mesh: &Mesh, index: [usize; 3], bary: Vec3) -> u32 {
    let mut weights = [(0u16, 0f32); 12];
    let mut count = 0;
    for (corner, i) in index.into_iter().enumerate() {
        for (&joint, &weight) in mesh.joints[i].iter().zip(&mesh.weights[i]) {
            if weight <= 0. {
                continue;
            }
            let slot = weights[..count]
                .iter()
                .position(|w| w.0 == joint)
                .unwrap_or(count);
            if slot == count {
                weights[slot].0 = joint;
                count += 1;
            }
            weights[slot].1 += bary[corner] * weight;
        }
    }
    weights[..count]
        .iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map_or(0, |w| u32::from(w.0))
}

pub(super) struct CoveredCell {
    pub cell: Cell,
    pub hits: u8,
}
fn walk_grid(lo: Vec3, hi: Vec3, mut visit: impl FnMut([i32; 3])) -> Option<()> {
    if !lo.is_finite()
        || !hi.is_finite()
        || lo.abs().max(hi.abs()).max_element() >= (1u32 << 23) as f32
    {
        return None;
    }
    if lo.cmpgt(hi).any() {
        return Some(());
    }
    let lo = lo.floor().as_ivec3();
    let hi = hi.floor().as_ivec3();
    let span = hi.as_i64vec3() - lo.as_i64vec3() + glam::I64Vec3::ONE;
    if span.x.checked_mul(span.y)?.checked_mul(span.z)? > MAX_CANDIDATE_CELLS as i64 {
        return None;
    }
    for z in lo.z..=hi.z {
        for y in lo.y..=hi.y {
            for x in lo.x..=hi.x {
                visit([x, y, z]);
            }
        }
    }
    Some(())
}

fn triangle_cells(points: [Vec3; 3], mut visit: impl FnMut([i32; 3])) -> Option<()> {
    let [a, b, c] = points;
    let n = (b - a).cross(c - a).try_normalize()?;
    // The bounding box covers the three grid axes. Test the triangle plane
    // and nine edge-cross-axis projections for conservative surface coverage.
    let axes = [b - a, c - b, a - c].map(|edge| {
        [Vec3::X, Vec3::Y, Vec3::Z].map(|basis| {
            let axis = edge.cross(basis);
            let dots = points.map(|p| p.dot(axis));
            let radius = 0.5 * axis.abs().element_sum() + 1e-5;
            (
                axis,
                dots[0].min(dots[1]).min(dots[2]) - radius,
                dots[0].max(dots[1]).max(dots[2]) + radius,
            )
        })
    });
    let plane_radius = 0.5 * n.abs().element_sum() + 1e-5;
    walk_grid(a.min(b).min(c), a.max(b).max(c), |grid| {
        let center = Vec3::from_array(grid.map(|v| v as f32 + 0.5));
        if n.dot(center - a).abs() <= plane_radius
            && axes.iter().flatten().all(|&(axis, lo, hi)| {
                let t = center.dot(axis);
                t >= lo && t <= hi
            })
        {
            visit(grid);
        }
    })
}

pub(super) fn sample(
    mesh: &Mesh,
    surface: &SourceSurface,
    cut: &ChildCut,
    size: f32,
) -> Option<Vec<CoveredCell>> {
    let mut candidates = BTreeMap::<[i32; 3], Vec<usize>>::new();
    for &f in &cut.triangles {
        let points = positions(mesh, f).map(|p| p / size);
        if (points[1] - points[0])
            .cross(points[2] - points[0])
            .length_squared()
            == 0.
        {
            continue;
        }
        triangle_cells(points, |grid| {
            candidates.entry(grid).or_default();
        })?;
        if candidates.len() > MAX_CANDIDATE_CELLS {
            return None;
        }
    }
    for child in &cut.voxels {
        let ratio = child.size / size;
        for grid in child
            .layout
            .cells
            .iter()
            .map(|c| &c.grid)
            .chain(&child.extra_cells)
        {
            let lo = Vec3::from_array(grid.map(|v| v as f32)) * ratio;
            walk_grid(lo, lo + ratio, |grid| {
                candidates.entry(grid).or_default();
            })?;
            if candidates.len() > MAX_CANDIDATE_CELLS {
                return None;
            }
        }
    }
    if candidates.is_empty() {
        return Some(Vec::new());
    }
    let (lo, hi) = candidates
        .keys()
        .map(|g| Vec3::from_array(g.map(|v| v as f32)))
        .fold(
            (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
            |(lo, hi), v| (lo.min(v), hi.max(v)),
        );
    // Leaf patches of the original prototype are the ray scene, even when a
    // candidate was contributed exclusively by occupied cells of a child.
    let mut geometry = Vec::new();
    for id in surface.faces_in(lo * size, (hi + 1.) * size) {
        let index = triangle(mesh, id);
        let [a, b, c] = positions(mesh, id).map(|p| p / size);
        let ab = b - a;
        let ac = c - a;
        let cross = ab.cross(ac);
        let Some(normal) = cross.try_normalize() else {
            continue;
        };
        let plane_radius = normal.abs().element_sum() * 0.5 + 1e-5;
        let bounds_lo = a.min(b).min(c).floor().max(lo);
        let bounds_hi = a.max(b).max(c).floor().min(hi);
        walk_grid(bounds_lo, bounds_hi, |grid| {
            let center = Vec3::from_array(grid.map(|v| v as f32 + 0.5));
            if normal.dot(center - a).abs() <= plane_radius
                && let Some(list) = candidates.get_mut(&grid)
            {
                list.push(geometry.len());
            }
        })?;
        let inv = 1. / cross.length_squared();
        geometry.push(Face {
            index,
            a,
            normal,
            bary_u: ac.cross(cross) * inv,
            bary_v: cross.cross(ab) * inv,
        });
    }
    static PATTERNS: std::sync::LazyLock<[[Ray; RAYS]; 16]> =
        std::sync::LazyLock::new(|| std::array::from_fn(|i| rays(hash(i as u32 + 37))));
    let mut cells = Vec::new();
    for (grid, list) in candidates {
        let cell_seed =
            hash(grid[0] as u32 ^ hash(grid[1] as u32) ^ hash((grid[2] as u32).wrapping_add(97)));
        let offset = Vec3::from_array(grid.map(|n| n as f32));
        let mut lobes = [Vec3::ZERO; 3];
        let mut mass = [0u8; 3];
        let mut count = 0u8;
        let mut representative = None;
        for (r, ray) in PATTERNS[cell_seed as usize % PATTERNS.len()]
            .iter()
            .enumerate()
        {
            let mut nearest = ray.length;
            let mut hit = None;
            for &f in &list {
                let face = &geometry[f];
                if let Some((t, bary)) = face.hit(offset + ray.origin, ray.direction, nearest) {
                    nearest = t;
                    hit = Some((face, bary));
                }
            }
            if let Some((face, bary)) = hit {
                let mut normal = Vec3::ZERO;
                for (c, &i) in face.index.iter().enumerate() {
                    normal +=
                        mesh.normals.get(i).copied().map_or(face.normal, Vec3::from) * bary[c];
                }
                let n = normal.normalize_or(face.normal);
                let a = n.abs();
                let axis = if a.x >= a.y && a.x >= a.z {
                    0
                } else if a.y >= a.z {
                    1
                } else {
                    2
                };
                lobes[axis] += n * n[axis].signum();
                mass[axis] += 1;
                count += 1;
                let priority = hash(cell_seed ^ r as u32);
                if representative
                    .as_ref()
                    .is_none_or(|&(old, _, _)| priority < old)
                {
                    representative = Some((priority, face.index, bary));
                }
            }
        }
        if let Some((_, index, bary)) = representative {
            let uv = index
                .iter()
                .enumerate()
                .map(|(c, &i)| {
                    mesh.uv.get(i).copied().map_or(Vec2::splat(0.5), Vec2::from) * bary[c]
                })
                .sum();
            let normals = std::array::from_fn(|i| {
                let [x, y, z] = lobes[i]
                    .normalize_or_zero()
                    .to_array()
                    .map(|v| (v.clamp(-1., 1.) * 127.).round() as i8 as u8);
                u32::from_le_bytes([
                    x,
                    y,
                    z,
                    (255. * f32::from(mass[i]) / f32::from(count)).round() as u8,
                ])
            });
            cells.push(CoveredCell {
                cell: Cell {
                    grid,
                    uv,
                    joint: dominant_joint(mesh, index, bary),
                    normals,
                },
                hits: count,
            });
        }
    }
    Some(cells)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_triangles(mesh: &Mesh, faces: &[u32]) -> Vec<CoveredCell> {
        let leaves = [super::super::Meshlet {
            triangles: faces.to_vec(),
            refined_group: None,
        }];
        let cut = ChildCut {
            triangles: faces.to_vec(),
            voxels: vec![],
            vertices: 0,
            area: 0.,
            error_floor: 0.,
        };
        sample(mesh, &SourceSurface::new(mesh, &leaves), &cut, 1.).unwrap()
    }

    #[test]
    fn mixed_children_seed_parent_cells_once_and_resample_original_leaf_attributes() {
        use super::super::{ClusterNode, Meshlet, Replacement};
        // The original leaf scene contains three islands. The current group
        // seeds them by a triangle, an occupied voxel, and a build-only rejected cell.
        let mut mesh = Mesh {
            positions: vec![],
            indices: vec![],
            normals: vec![],
            uv: vec![],
            joints: vec![],
            weights: vec![],
        };
        for (island, x) in [0., 2., 5.].into_iter().enumerate() {
            let base = mesh.positions.len() as u32;
            mesh.positions
                .extend([[x, 0., 0.5], [x + 0.9, 0., 0.5], [x, 0.9, 0.5]]);
            mesh.indices.extend([base, base + 1, base + 2]);
            mesh.uv.extend([[island as f32 / 3., 0.25]; 3]);
            mesh.normals.extend([[0., 0., 1.]; 3]);
            mesh.joints.extend([[island as u16 + 1, 0, 0, 0]; 3]);
            mesh.weights.extend([[1., 0., 0., 0.]; 3]);
        }
        let meshlets = vec![
            Meshlet {
                triangles: vec![0],
                refined_group: None,
            },
            Meshlet {
                triangles: vec![1, 2],
                refined_group: None,
            },
            Meshlet {
                triangles: vec![2],
                refined_group: Some(0),
            },
        ];
        let group = ClusterNode {
            meshlets: vec![0, 2, 2],
            center: [0.; 3],
            radius: 8.,
            error: 1.,
        };
        let child = Replacement {
            size: 0.5,
            source_area: 0.5,
            extra_cells: vec![[10, 0, 1]],
            parts: vec![vec![0]],
            layout: super::super::bricks::Layout::new(vec![Cell {
                grid: [4, 0, 1],
                joint: 99,
                uv: Vec2::splat(9.),
                normals: [0; 3],
            }]),
        };
        let mut choices = [Some(child)];
        let cut = ChildCut::new(&mesh, &group, &meshlets, &choices, &[0.7]);
        assert_eq!(cut.triangles, [0]);
        assert_eq!(cut.voxels.len(), 1);
        assert_eq!(cut.vertices, 4);
        assert_eq!(cut.error_floor, 0.7);
        let samples = sample(&mesh, &SourceSurface::new(&mesh, &meshlets), &cut, 1.).unwrap();
        assert!(
            samples
                .iter()
                .any(|s| s.cell.grid == [0, 0, 0] && s.cell.joint == 1)
        );
        let inherited = samples.iter().find(|s| s.cell.grid == [2, 0, 0]).unwrap();
        assert_eq!(inherited.cell.joint, 2);
        assert!((inherited.cell.uv.x - 1. / 3.).abs() < 1e-5);
        let recovered = samples.iter().find(|s| s.cell.grid == [5, 0, 0]).unwrap();
        assert_eq!(recovered.cell.joint, 3);
        assert_eq!(cut.voxels[0].layout.cells.len(), 1);
        choices[0].as_mut().unwrap().extra_cells.clear();
        let without_extra = ChildCut::new(&mesh, &group, &meshlets, &choices, &[0.7]);
        let samples = sample(
            &mesh,
            &SourceSurface::new(&mesh, &meshlets),
            &without_extra,
            1.,
        )
        .unwrap();
        assert!(samples.iter().all(|s| s.cell.grid[0] < 5));
    }

    #[test]
    fn duplicated_surfaces_do_not_gain_opacity_or_mix_attributes() {
        // One square on the midplane of a negative-coordinate cell, with one UV
        // island. Duplicating the faces must keep the same hit masks and samples.
        let mesh = Mesh {
            positions: vec![[-1., 0., 0.5], [0., 0., 0.5], [0., 1., 0.5], [-1., 1., 0.5]],
            indices: vec![0, 1, 2, 0, 2, 3],
            normals: vec![[0., 0., 1.]; 4],
            uv: vec![[0., 0.], [1., 0.], [1., 1.], [0., 1.]],
            joints: vec![[3, 0, 0, 0]; 4],
            weights: vec![[1., 0., 0., 0.]; 4],
        };
        let bake = |faces: &[u32]| {
            let samples = sample_triangles(&mesh, faces);
            let cell = samples.iter().find(|s| s.cell.grid == [-1, 0, 0]).unwrap();
            assert!(cell.hits > 0 && usize::from(cell.hits) < RAYS);
            assert_eq!(cell.cell.normals[2] >> 24, 255);
            assert_eq!(cell.cell.joint, 3);
            assert!((0.0..=1.).contains(&cell.cell.uv.x) && (0.0..=1.).contains(&cell.cell.uv.y));
            (cell.hits, cell.cell.uv, cell.cell.normals)
        };
        assert_eq!(bake(&[0, 1]), bake(&[0, 1, 0, 1]));
        // Every UV comes from one surface, including across atlas islands.
        let mut islands = mesh.clone();
        let mut second = mesh.positions.clone();
        for p in &mut second {
            p[2] += 0.15;
        }
        islands.positions.extend(second);
        islands.indices.extend([4, 5, 6, 4, 6, 7]);
        islands.normals = vec![[0., 0., 1.]; 8];
        islands.uv = [[0.1, 0.1]; 4].into_iter().chain([[0.9, 0.9]; 4]).collect();
        islands.joints = vec![[3, 0, 0, 0]; 8];
        islands.weights = vec![[1., 0., 0., 0.]; 8];
        for s in sample_triangles(&islands, &[0, 1, 2, 3]) {
            assert!((s.cell.uv.x - 0.1).abs() < 1e-5 || (s.cell.uv.x - 0.9).abs() < 1e-5);
        }
        let mut weighted = mesh;
        weighted.joints[0][0] = 9;
        assert_eq!(
            dominant_joint(&weighted, [0, 1, 2], Vec3::new(0.4, 0.3, 0.3)),
            3
        );
    }
}
