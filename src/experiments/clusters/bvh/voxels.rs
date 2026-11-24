//! Build a voxel candidate for each complete DAG replacement.
use super::*;
mod bricks;
mod children;
mod coverage;
mod grouping;
mod levels;
pub(super) use levels::Hierarchy;
mod sampling;
use children::{ChildCut, SourceSurface};
use glam::Vec2;
use rayon::prelude::*;
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
    source_area: f32,
    // Occupancy discarded from this cut may still seed a coarser grid.
    // These coordinates are CPU build data and never enter the GPU brick stream.
    extra_cells: Vec<[i32; 3]>,
    layout: bricks::Layout,
    parts: Vec<Vec<usize>>,
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

    fn error(&self, reference: &Footprint, candidate: &Footprint) -> f32 {
        let mismatch: u32 = reference
            .iter()
            .flatten()
            .zip(candidate.iter().flatten())
            .map(|(a, b)| (a ^ b).count_ones())
            .sum();
        0.33 * self.pixel * (mismatch as f32 / 3.).sqrt()
    }
}

fn reduce(
    mesh: &Mesh,
    surface: &SourceSurface,
    cut: &ChildCut<'_>,
    coarse: &[u32],
    qem: Option<f32>,
    radius: f32,
    base_size: f32,
) -> (Option<Replacement>, f32) {
    let mut triangle_error = qem.unwrap_or(0.).max(cut.error_floor);
    if cut.voxels.is_empty() {
        let Some(probe) = Probe::new(mesh, &cut.triangles, coarse) else {
            return (None, triangle_error);
        };
        if !coarse.is_empty() {
            triangle_error = triangle_error.max(probe.error(
                &probe.triangles(mesh, &cut.triangles),
                &probe.triangles(mesh, coarse),
            ));
        }
        if !is_volume(mesh, &cut.triangles, probe.pixel) {
            return (None, triangle_error);
        }
    }
    let elements = cut.triangles.len()
        + cut
            .voxels
            .iter()
            .map(|r| r.layout.bricks.len())
            .sum::<usize>();
    let parent_budget = elements.div_ceil(2 * bricks::MAX_BRICKS).max(1);
    let target = (cut.vertices * 3 / 4).max(1);
    let mut size = (0.75 * (cut.area / target as f32).sqrt())
        .max(base_size * 0.5)
        .max(cut.error_floor);
    // A complete mixed group has no independent triangle simplification to beat.
    // Cap an empty or unencodable group by its support diameter.
    let limit = if qem.is_none() {
        2. * (2. * radius).max(size)
    } else {
        triangle_error
    };
    while size.is_finite() && size > 0. && size < limit {
        if let Some(samples) = sampling::sample(mesh, surface, cut, size) {
            let retained = coverage::reduce(&samples);
            let (cells, discarded): (Vec<_>, Vec<_>) = samples
                .into_iter()
                .zip(retained)
                .partition(|(_, keep)| *keep);
            if !cells.is_empty()
                && cells.len() <= target
                && (!cut.triangles.is_empty() || cells.len() < cut.vertices)
            {
                let layout =
                    bricks::Layout::new(cells.into_iter().map(|(sample, _)| sample.cell).collect());
                if let Some(parts) = layout.partition(parent_budget) {
                    return (
                        Some(Replacement {
                            size,
                            source_area: cut.area,
                            layout,
                            parts,
                            extra_cells: discarded
                                .into_iter()
                                .map(|(sample, _)| sample.cell.grid)
                                .collect(),
                        }),
                        size,
                    );
                }
            }
        }
        size *= 1.1;
    }
    (None, triangle_error)
}

struct SurfaceCell<'a> {
    cell: &'a Cell,
    origin: [i32; 3],
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
    for brick in cells.chunk_by(|a, b| a.origin == b.origin && a.cell.joint == b.cell.joint) {
        let coord = brick[0].origin;
        let joint = brick[0].cell.joint;
        let origin = Vec3::from_array(coord.map(|v| v as f32)) * size;
        let first = (attributes.len() / 6) as u32;
        let mut mask = 0u64;
        let mut lo = [3u32; 3];
        let mut hi = [0u32; 3];
        let brick_index = words[start];
        for record in brick {
            let cell = record.cell;
            let bit = bricks::bit(cell.grid, coord);
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
        meta: [u32; 4],
        bones: &[PlantBone],
    ) -> Result<Vec<Cluster>> {
        let mut parents = Vec::with_capacity(self.parts.len());
        for part in &self.parts {
            let mut meta = meta;
            let first_page = pages.len();
            let stream: u32 = vertices.len().try_into()?;
            let half = self.size * 0.5;
            let mut cells = Vec::new();
            let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
            let mut primitives = 0;
            for &id in part {
                let brick = &self.layout.bricks[id];
                for i in brick.cells.clone() {
                    let cell = &self.layout.cells[i];
                    let faces = u32::from(self.layout.faces[i]);
                    if faces == 0 {
                        continue;
                    }
                    let triangles = 2 * faces.count_ones();
                    if pages.len() == first_page
                        || pages
                            .last()
                            .is_some_and(|p| bricks::page_full(p.span[1], p.span[3], triangles))
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
                        origin: brick.origin,
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
            }
            if cells.is_empty() {
                continue;
            }
            let bricks = write_bricks(vertices, &cells, self.size);
            ensure!(
                (1..=bricks::MAX_BRICKS as u32).contains(&bricks)
                    && pages.len() - first_page <= bricks::MAX_PAGES,
                "voxel cluster exceeds brick or hit-page encoding"
            );
            let bounds = sphere(lo, hi);
            // Only the logical cluster is culled. Its CLAS pages inherit its
            // transition and conservative caster bounds.
            for page in &mut pages[first_page..] {
                page.sphere = bounds;
            }
            meta[3] = 1 | bricks << 1;
            parents.push(Cluster {
                span: [
                    stream,
                    (pages.len() - first_page).try_into()?,
                    first_page.try_into()?,
                    primitives,
                ],
                sphere: bounds,
                meta,
            });
        }
        Ok(parents)
    }
}
