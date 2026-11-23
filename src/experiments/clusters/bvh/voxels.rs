//! Build a voxel candidate for each complete DAG replacement.
use super::*;
use glam::Vec2;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy)]
struct Cell {
    grid: [i32; 3],
    normals: [u32; 3],
    joint: u32,
    uv: Vec2,
}

pub(super) struct Replacement {
    size: f32,
    cells: Vec<Cell>,
}

#[derive(Default)]
struct Sample {
    area: Vec3,
    // Each bit is a distinct orthographic ray through a cell face. Stacked
    // leaves increase area, but the same ray only contributes coverage once.
    rays: [u16; 3],
    // Two-sided leaf normals are axes. Keep an area-weighted directional lobe
    // for each dominant axis, including its mass for the shading distribution.
    lobes: [Vec3; 3],
    mass: [f32; 3],
    uv: Vec2,
    joint: u32,
    strongest: f32,
}

fn projected_area(mesh: &Mesh, faces: &[u32]) -> Vec3 {
    faces
        .iter()
        .map(|&f| {
            let [a, b, c] = positions(mesh, f);
            (b - a).cross(c - a).abs() * 0.5
        })
        .sum()
}

fn is_volume(mesh: &Mesh, faces: &[u32], tolerance: f32) -> bool {
    let largest = faces
        .iter()
        .map(|&f| {
            let [a, b, c] = positions(mesh, f);
            (a, (b - a).cross(c - a))
        })
        .max_by(|a, b| a.1.length_squared().total_cmp(&b.1.length_squared()));
    let Some((point, n)) = largest else {
        return false;
    };
    let Some(n) = n.try_normalize() else {
        return false;
    };
    faces.iter().any(|&f| {
        positions(mesh, f)
            .iter()
            .any(|p| (*p - point).dot(n).abs() > tolerance)
    })
}

// A brick ray contributes one surface sample for each occupied column,
// regardless of how many cells it crosses along that axis.
fn brick_projected_area(cells: &[Cell], size: f32) -> Vec3 {
    let mut area = Vec3::ZERO;
    for brick in cells.chunk_by(|a, b| brick_key(a) == brick_key(b)) {
        let mut columns = [0u16; 3];
        for cell in brick {
            let [x, y, z] = cell.grid.map(|v| v.rem_euclid(4) as u32);
            for (mask, bit) in columns.iter_mut().zip([y + 4 * z, x + 4 * z, x + 4 * y]) {
                *mask |= 1 << bit;
            }
        }
        area += Vec3::from_array(columns.map(|m| m.count_ones() as f32)) * size * size;
    }
    area
}

// Trace a 4x4 lattice of parallel rays per cell face. Barycentric intersection
// gives the cell along the ray. The three views share a model-space lattice.
fn sample_projected_rays(cells: &mut BTreeMap<[i32; 3], Sample>, vertices: [Vec3; 3]) {
    for axis in 0..3 {
        let u = (axis + 1) % 3;
        let v = (axis + 2) % 3;
        let [a, b, c] = vertices.map(|p| Vec2::new(p[u], p[v]) * 4.);
        let ab = b - a;
        let ac = c - a;
        let det = ab.perp_dot(ac);
        if det.abs() < 1e-7 {
            continue;
        }
        let lo = (a.min(b).min(c) - 0.5).ceil().as_ivec2();
        let hi = (a.max(b).max(c) - 0.5).floor().as_ivec2();
        // Very large triangles remain eligible for the triangle path. Bound
        // preprocessing for assets with an impractically small voxel grid.
        if (hi - lo).max_element() > 96 {
            continue;
        }
        for y in lo.y..=hi.y {
            for x in lo.x..=hi.x {
                let ap = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) - a;
                let s = ap.perp_dot(ac) / det;
                let t = ab.perp_dot(ap) / det;
                if s < 0. || t < 0. || s + t > 1. {
                    continue;
                }
                let h = vertices[0][axis]
                    + s * (vertices[1][axis] - vertices[0][axis])
                    + t * (vertices[2][axis] - vertices[0][axis]);
                let mut grid = [0; 3];
                grid[axis] = h.floor() as i32;
                grid[u] = x.div_euclid(4);
                grid[v] = y.div_euclid(4);
                if let Some(cell) = cells.get_mut(&grid) {
                    cell.rays[axis] |= 1 << (x.rem_euclid(4) + 4 * y.rem_euclid(4));
                }
            }
        }
    }
}

impl Sample {
    fn normals(&self) -> [u32; 3] {
        let total = self.mass.iter().sum::<f32>();
        std::array::from_fn(|i| {
            let n = self.lobes[i].normalize_or_zero();
            let [x, y, z] = n
                .to_array()
                .map(|v| (v.clamp(-1., 1.) * 127.).round() as i8 as u8);
            u32::from_le_bytes([x, y, z, (self.mass[i] / total * 255.).round() as u8])
        })
    }
}

fn triangle(mesh: &Mesh, face: u32) -> [usize; 3] {
    let i = face as usize * 3;
    [i, i + 1, i + 2].map(|i| mesh.indices[i] as usize)
}

fn positions(mesh: &Mesh, face: u32) -> [Vec3; 3] {
    triangle(mesh, face).map(|i| Vec3::from(mesh.positions[i]))
}

// Compare silhouettes at this region's scale. Expanded solid foliage can
// conceal gaps just as easily as over-simplified triangles can drop leaves.
const RES: usize = 40;
type Footprint = [[u64; RES]; 3];

struct Probe {
    origin: Vec3,
    pixel: f32,
}

impl Probe {
    fn new(mesh: &Mesh, fine: &[u32], coarse: &[u32]) -> Option<Self> {
        let points = fine.iter().chain(coarse).flat_map(|&f| positions(mesh, f));
        let (lo, hi) = points.fold(
            (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
            |(lo, hi), p| (lo.min(p), hi.max(p)),
        );
        let width = (hi - lo).max_element() * 1.35;
        (width.is_finite() && width > 1e-5).then_some(Self {
            origin: (lo + hi - width) * 0.5,
            pixel: width / RES as f32,
        })
    }

    fn triangles(&self, mesh: &Mesh, faces: &[u32]) -> Footprint {
        let mut masks = [[0; RES]; 3];
        for &face in faces {
            let p = positions(mesh, face).map(|v| (v - self.origin) / self.pixel);
            for (axis, rows) in masks.iter_mut().enumerate() {
                let project = |p: Vec3| glam::Vec2::new(p[(axis + 1) % 3], p[(axis + 2) % 3]);
                let [a, b, c] = p.map(project);
                let cross = (b - a).perp_dot(c - a);
                if cross.abs() < 1e-5 {
                    continue;
                }
                let lo = a.min(b).min(c).floor().max(glam::Vec2::ZERO);
                let hi = a
                    .max(b)
                    .max(c)
                    .floor()
                    .min(glam::Vec2::splat((RES - 1) as f32));
                let mut hit = false;
                for (y, row) in rows
                    .iter_mut()
                    .enumerate()
                    .take(hi.y as usize + 1)
                    .skip(lo.y as usize)
                {
                    for x in lo.x as usize..=hi.x as usize {
                        let p = glam::Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                        let u = (b - a).perp_dot(p - a) / cross;
                        let v = (c - b).perp_dot(p - b) / cross;
                        let w = (a - c).perp_dot(p - c) / cross;
                        if u >= 0. && v >= 0. && w >= 0. {
                            *row |= 1 << x;
                            hit = true;
                        }
                    }
                }
                // Very fine leaves still contribute at least their centroid sample.
                if !hit {
                    let p = ((a + b + c) / 3.).floor();
                    if p.min_element() >= 0. && p.max_element() < RES as f32 {
                        rows[p.y as usize] |= 1 << p.x as usize;
                    }
                }
            }
        }
        masks
    }

    fn voxels(&self, size: f32, cells: &[Cell]) -> Footprint {
        let mut masks = [[0; RES]; 3];
        for cell in cells {
            let lo =
                (Vec3::from_array(cell.grid.map(|p| p as f32)) * size - self.origin) / self.pixel;
            let hi = lo + size / self.pixel;
            for (axis, rows) in masks.iter_mut().enumerate() {
                let u = (axis + 1) % 3;
                let v = (axis + 2) % 3;
                // Center sampling, matching the triangle probe.
                let x0 = ((lo[u] - 0.5).ceil() as i32).clamp(0, RES as i32) as usize;
                let x1 = ((hi[u] - 0.5).ceil() as i32).clamp(0, RES as i32) as usize;
                let y0 = ((lo[v] - 0.5).ceil() as i32).clamp(0, RES as i32) as usize;
                let y1 = ((hi[v] - 0.5).ceil() as i32).clamp(0, RES as i32) as usize;
                if x0 < x1 && y0 < y1 {
                    let bits = ((1u64 << x1) - 1) ^ ((1u64 << x0) - 1);
                    for row in &mut rows[y0..y1] {
                        *row |= bits;
                    }
                }
            }
        }
        masks
    }

    fn error(&self, reference: &Footprint, candidate: &Footprint) -> f32 {
        let disagreement: f32 = reference
            .iter()
            .flatten()
            .zip(candidate.iter().flatten())
            .map(|(a, b)| (a ^ b).count_ones() as f32)
            .sum();
        0.33 * self.pixel * (disagreement / 3.).sqrt()
    }
}

fn sample_cells(mesh: &Mesh, faces: &[u32], size: f32) -> BTreeMap<[i32; 3], Sample> {
    let mut cells = BTreeMap::<_, Sample>::new();
    for &face in faces {
        let vertex = triangle(mesh, face);
        let [a, b, c] = vertex.map(|i| Vec3::from(mesh.positions[i]));
        let cross = (b - a).cross(c - a);
        if cross.length_squared() <= 1e-14 {
            continue;
        }
        let steps = ((a.distance(b).max(b.distance(c)).max(c.distance(a)) / (0.5 * size)).ceil()
            as u32)
            .clamp(1, 22);
        let count = (steps + 1) * (steps + 2) / 2;
        let area = cross.abs() * (0.5 / count as f32);
        let strength = area.element_sum();
        let geometric = cross.normalize();
        let normals = vertex.map(|i| mesh.normals.get(i).copied().map_or(geometric, Vec3::from));
        let uvs = vertex.map(|i| mesh.uv.get(i).copied().map_or(Vec2::splat(0.5), Vec2::from));
        for row in 0..=steps {
            for col in 0..=steps - row {
                let u = row as f32 / steps as f32;
                let v = col as f32 / steps as f32;
                let p = (a + (b - a) * u + (c - a) * v) / size;
                let grid = p.floor();
                let normal = (normals[0] * (1. - u - v) + normals[1] * u + normals[2] * v)
                    .normalize_or(geometric);
                let sample = cells.entry(grid.as_ivec3().to_array()).or_default();
                sample.area += area;
                let a = normal.abs();
                let axis = if a.x >= a.y && a.x >= a.z {
                    0
                } else if a.y >= a.z {
                    1
                } else {
                    2
                };
                sample.lobes[axis] += normal * (normal[axis].signum() * strength);
                sample.mass[axis] += strength;
                sample.uv += (uvs[0] * (1. - u - v) + uvs[1] * u + uvs[2] * v) * strength;
                if strength > sample.strongest {
                    let dominant = mesh.weights[vertex[0]]
                        .iter()
                        .enumerate()
                        .max_by(|a, b| a.1.total_cmp(b.1))
                        .unwrap()
                        .0;
                    sample.joint = u32::from(mesh.joints[vertex[0]][dominant]);
                    sample.strongest = strength;
                }
            }
        }
    }
    for &face in faces {
        sample_projected_rays(&mut cells, positions(mesh, face).map(|p| p / size));
    }
    cells
}

pub(super) fn choose(
    mesh: &Mesh,
    nodes: &[ClusterNode],
    meshlets: &[Meshlet],
    base_size: f32,
) -> (Vec<Option<Replacement>>, Vec<f32>) {
    let mut incoming = vec![BTreeMap::<usize, Vec<u32>>::new(); nodes.len()];
    for (owner, group) in nodes.iter().enumerate() {
        for &id in &group.meshlets {
            let cluster = &meshlets[id as usize];
            if let Some(fine) = cluster.refined_group {
                incoming[fine as usize]
                    .entry(owner)
                    .or_default()
                    .extend(&cluster.triangles);
            }
        }
    }
    let mut reference = Vec::<Vec<u32>>::with_capacity(nodes.len());
    let mut choices = Vec::with_capacity(nodes.len());
    let mut errors = Vec::with_capacity(nodes.len());
    for (id, group) in nodes.iter().enumerate() {
        let mut fine = BTreeSet::new();
        for &c in &group.meshlets {
            let meshlet = &meshlets[c as usize];
            match meshlet.refined_group {
                Some(child) if incoming[child as usize].len() == 1 => {
                    fine.extend(reference[child as usize].iter().copied());
                }
                _ => fine.extend(meshlet.triangles.iter().copied()),
            }
        }
        let fine: Vec<u32> = fine.into_iter().collect();
        let coarse: Vec<u32> = incoming[id].values().flatten().copied().collect();
        let mut error = group.error;
        let mut best = None;
        if let Some(probe) = (!coarse.is_empty())
            .then(|| Probe::new(mesh, &fine, &coarse))
            .flatten()
        {
            let reference_mask = probe.triangles(mesh, &fine);
            error = error.max(probe.error(&reference_mask, &probe.triangles(mesh, &coarse)));
            // A complete replacement must have a single owner. Replacing a
            // fraction of a shared group would duplicate or drop its coverage.
            if incoming[id].len() == 1 && is_volume(mesh, &fine, probe.pixel) {
                let triangle_area = projected_area(mesh, &coarse).element_sum().max(1e-12);
                let mut best_score = error;
                // Compact samples use 24 bytes and share their brick header.
                let limit = coarse.len().div_ceil(3).clamp(8, 128);
                for size in [0.5, 0.75, 1., 1.5, 2., 3., 4.].map(|scale| base_size * scale) {
                    let mut samples: Vec<_> = sample_cells(mesh, &fine, size).into_iter().collect();
                    samples
                        .sort_by(|a, b| b.1.area.max_element().total_cmp(&a.1.area.max_element()));
                    for (budget, density) in [limit.div_ceil(2), limit]
                        .into_iter()
                        .flat_map(|budget| [0.14, 0.30, 0.55].map(|density| (budget, density)))
                    {
                        let mut cells: Vec<_> = samples
                            .iter()
                            .take(budget)
                            .take_while(|(_, s)| s.area.max_element() >= density * size * size)
                            .filter(|(_, s)| {
                                s.rays
                                    .iter()
                                    .any(|mask| mask.count_ones() as f32 >= density * 16.)
                            })
                            .map(|&(grid, ref s)| Cell {
                                grid,
                                normals: s.normals(),
                                joint: s.joint,
                                uv: s.uv / s.area.element_sum(),
                            })
                            .collect();
                        if cells.is_empty() {
                            continue;
                        }
                        cells.sort_by_key(|c| (brick_key(c), brick_bit(c)));
                        // Bound the projected half-diagonal, including isolated canopy cells on the skyline.
                        let voxel_error = (0.5 * 3.0_f32.sqrt() * size)
                            .max(probe.error(&reference_mask, &probe.voxels(size, &cells)));
                        // A coarse triangle cut can drop an entire spray of leaves. Charge
                        // additional layers when ranking candidates, but let a better
                        // silhouette win. Only geometric error drives runtime LOD.
                        let layers =
                            brick_projected_area(&cells, size).element_sum() / triangle_area;
                        let score = voxel_error + probe.pixel * 0.25 * layers.max(1.).log2();
                        if score < best_score {
                            best_score = score;
                            error = voxel_error;
                            best = Some(Replacement { size, cells });
                        }
                    }
                }
            }
        }
        choices.push(best);
        errors.push(error);
        reference.push(fine);
    }
    (choices, errors)
}

fn brick_key(cell: &Cell) -> ([i32; 3], u32) {
    (cell.grid.map(|v| v.div_euclid(4)), cell.joint)
}

fn brick_bit(cell: &Cell) -> u32 {
    let [x, y, z] = cell.grid.map(|v| v.rem_euclid(4) as u32);
    x | y << 2 | z << 4
}

struct SurfaceCell<'a> {
    cell: &'a Cell,
    faces: u32,
    page: u32,
    first_quad: u32,
}

/// Header words hold the grid transform, occupancy bits, compact cell range,
/// occupied bounds and bone. Six words per occupied cell carry shading and
/// its mapping to a 64-triangle visibility/CLAS page.
fn write_bricks(words: &mut Vec<u32>, cells: &[SurfaceCell], size: f32) -> u32 {
    let start = words.len();
    words.push(0);
    let mut attributes = Vec::with_capacity(6 * cells.len());
    for brick in cells.chunk_by(|a, b| brick_key(a.cell) == brick_key(b.cell)) {
        let (coord, joint) = brick_key(brick[0].cell);
        let origin = Vec3::from_array(coord.map(|v| v as f32)) * (4. * size);
        let first = (attributes.len() / 6) as u32;
        let mut mask = 0u64;
        let mut lo = [3u32; 3];
        let mut hi = [0u32; 3];
        let brick_index = words[start];
        for record in brick {
            let cell = record.cell;
            let bit = brick_bit(cell);
            mask |= 1 << bit;
            for axis in 0..3 {
                let coord = (bit >> (2 * axis)) & 3;
                lo[axis] = lo[axis].min(coord);
                hi[axis] = hi[axis].max(coord);
            }
            attributes.extend(cell.uv.to_array().map(f32::to_bits));
            attributes.extend(cell.normals);
            attributes.push(
                bit | brick_index << 6
                    | record.faces << 13
                    | record.first_quad << 19
                    | record.page << 24,
            );
        }
        let pack_grid = |[x, y, z]: [u32; 3]| x | y << 2 | z << 4;
        words.extend(origin.extend(size).to_array().map(f32::to_bits));
        words.extend([
            mask as u32,
            (mask >> 32) as u32,
            first | pack_grid(lo) << 16 | pack_grid(hi) << 22,
            joint,
        ]);
        words[start] += 1;
    }
    words.extend(attributes);
    words[start]
}

impl Replacement {
    pub(super) fn emit(
        &self,
        vertices: &mut Vec<u32>,
        indices: &mut Vec<u32>,
        pages: &mut Vec<Cluster>,
        mut meta: [u32; 4],
        bones: &[PlantBone],
    ) -> Result<Cluster> {
        let occupancy: BTreeMap<_, _> = self.cells.iter().map(|c| (c.grid, c.joint)).collect();
        let first_page = pages.len();
        let stream: u32 = vertices.len().try_into()?;
        let half = self.size * 0.5;
        let mut cells = Vec::with_capacity(self.cells.len());
        let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
        let mut primitives = 0;
        for cell in &self.cells {
            let faces = (0..6)
                .filter(|&f| {
                    let mut neighbor = cell.grid;
                    neighbor[f / 2] += if f % 2 == 0 { -1 } else { 1 };
                    occupancy.get(&neighbor) != Some(&cell.joint)
                })
                .fold(0u32, |bits, f| bits | 1 << f);
            if faces == 0 {
                continue;
            }
            let triangles = 2 * faces.count_ones();
            if pages.len() == first_page
                || pages
                    .last()
                    .is_some_and(|p| p.span[1] == 64 || p.span[3] + triangles > 64)
            {
                let mut page_meta = meta;
                page_meta[3] = 1 | (cells.len() as u32) << 1;
                pages.push(Cluster {
                    span: [stream, 0, indices.len().try_into()?, 0],
                    sphere: [0.; 4],
                    meta: page_meta,
                });
            }
            let relative_page = (pages.len() - first_page - 1) as u32;
            let page = pages.last_mut().unwrap();
            let slot = cells.len() as u32;
            cells.push(SurfaceCell {
                cell,
                faces,
                page: relative_page,
                first_quad: page.span[3] / 2,
            });
            indices.extend(
                (0..6)
                    .filter(|&f| faces & (1 << f) != 0)
                    .map(|f| (slot << 3) | f),
            );
            page.span[1] += 8;
            page.span[3] += triangles;
            primitives += triangles;
            let center = Vec3::from_array(cell.grid.map(|p| p as f32)) * self.size + half;
            let mut joint = Some(cell.joint as usize);
            let mut displacement = 0.;
            while let Some(i) = joint {
                let bone = bones[i];
                let corner = (center - Vec3::from(bone.pivot)).abs() + half;
                displacement += 2. * bone.bend.sin() * corner.length();
                joint = bone.parent.map(usize::from);
            }
            let bound = half + displacement;
            lo = lo.min(center - bound);
            hi = hi.max(center + bound);
        }
        let bricks = write_bricks(vertices, &cells, self.size);
        ensure!(
            (1..=128).contains(&bricks) && pages.len() - first_page <= 256,
            "voxel cluster exceeds brick or hit-page encoding"
        );
        let bounds = sphere(lo, hi);
        // Only the logical cluster is culled. Its CLAS pages inherit its
        // transition and conservative caster bounds.
        for page in &mut pages[first_page..] {
            page.sphere = bounds;
        }
        meta[3] = 1 | bricks << 1;
        Ok(Cluster {
            span: [
                stream,
                (pages.len() - first_page).try_into()?,
                first_page.try_into()?,
                primitives,
            ],
            sphere: bounds,
            meta,
        })
    }
}
