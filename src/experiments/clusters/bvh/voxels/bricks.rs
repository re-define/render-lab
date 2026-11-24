//! Pack a group in its common grid, then partition whole bricks between sibling parents.
use super::{BTreeMap, Cell, Vec3};
use std::{collections::HashMap, ops::Range};

pub(super) const MAX_BRICKS: usize = 128;
pub(super) const MAX_PAGES: usize = 256;

pub(super) fn page_full(vertices: u32, triangles: u32, extra: u32) -> bool {
    vertices == 64 || triangles + extra > 64
}

pub(super) struct Brick {
    pub origin: [i32; 3],
    pub cells: Range<usize>,
    surface_cells: usize,
    triangles: usize,
}

pub(super) struct Layout {
    pub cells: Vec<Cell>,
    pub bricks: Vec<Brick>,
    pub faces: Vec<u8>,
}

fn seed_key(cell: &Cell) -> (u32, i32, i32, i32) {
    let [x, y, z] = cell.grid.map(|v| v.div_euclid(4));
    (cell.joint, z, y, x)
}

pub(super) fn bit(grid: [i32; 3], origin: [i32; 3]) -> u32 {
    let [x, y, z] = std::array::from_fn(|i| (grid[i] - origin[i]) as u32);
    x | y << 2 | z << 4
}

impl Layout {
    pub fn new(cells: Vec<Cell>) -> Self {
        let mut seeds = BTreeMap::<_, Vec<usize>>::new();
        let mut source = HashMap::with_capacity(cells.len());
        for (i, cell) in cells.iter().enumerate() {
            seeds.entry(seed_key(cell)).or_default().push(i);
            source.insert(cell.grid, i);
        }
        let mut ordered = Vec::with_capacity(cells.len());
        let mut faces = Vec::with_capacity(cells.len());
        let mut claimed = vec![false; cells.len()];
        let mut bricks = Vec::with_capacity(seeds.len());
        for ((joint, _, _, _), seed) in seeds {
            let mut origin = [i32::MAX; 3];
            let mut available = false;
            for i in seed {
                let g = cells[i].grid;
                if !claimed[i] {
                    available = true;
                    origin = std::array::from_fn(|k| origin[k].min(g[k]));
                }
            }
            if !available {
                continue;
            }
            let start = ordered.len();
            for index in 0..64 {
                let grid = std::array::from_fn(|k| origin[k] + ((index >> (2 * k)) & 3));
                if let Some(&i) = source.get(&grid)
                    && cells[i].joint == joint
                    && !claimed[i]
                {
                    claimed[i] = true;
                    let mut exposed = 0u8;
                    for f in 0..6 {
                        let mut neighbor = grid;
                        let value = neighbor[f / 2].checked_add(if f % 2 == 0 { -1 } else { 1 });
                        let same_pose = value.is_some_and(|v| {
                            neighbor[f / 2] = v;
                            source
                                .get(&neighbor)
                                .is_some_and(|&n| cells[n].joint == joint)
                        });
                        if !same_pose {
                            exposed |= 1 << f;
                        }
                    }
                    ordered.push(cells[i]);
                    faces.push(exposed);
                }
            }
            let surface = &faces[start..];
            bricks.push(Brick {
                origin,
                cells: start..ordered.len(),
                surface_cells: surface.iter().filter(|&&f| f != 0).count(),
                triangles: surface.iter().map(|f| 2 * f.count_ones() as usize).sum(),
            });
        }
        debug_assert_eq!(ordered.len(), cells.len());
        Self {
            cells: ordered,
            bricks,
            faces,
        }
    }

    pub fn partition(&self, budget: usize) -> Option<Vec<Vec<usize>>> {
        let mut parts = Vec::new();
        self.split((0..self.bricks.len()).collect(), budget, &mut parts)?;
        Some(parts)
    }

    fn fits_pages(&self, ids: &[usize]) -> bool {
        let (mut pages, mut vertices, mut tris) = (1, 0, 0);
        for &id in ids {
            for &mask in &self.faces[self.bricks[id].cells.clone()] {
                if mask == 0 {
                    continue;
                }
                let add = 2 * mask.count_ones();
                if page_full(vertices, tris, add) {
                    pages += 1;
                    vertices = 0;
                    tris = 0;
                }
                if pages > MAX_PAGES {
                    return false;
                }
                vertices += 8;
                tris += add;
            }
        }
        true
    }

    fn split(&self, mut ids: Vec<usize>, budget: usize, parts: &mut Vec<Vec<usize>>) -> Option<()> {
        let count = ids.len();
        let required = |bricks: usize, cells: usize, tris: usize| {
            bricks
                .div_ceil(MAX_BRICKS)
                .max(cells.div_ceil(8 * MAX_PAGES))
                .max(tris.div_ceil(64 * MAX_PAGES))
        };
        let cells: usize = ids.iter().map(|&i| self.bricks[i].surface_cells).sum();
        let tris: usize = ids.iter().map(|&i| self.bricks[i].triangles).sum();
        if count == 0 || required(count, cells, tris) > budget {
            return None;
        }
        if count <= MAX_BRICKS && self.fits_pages(&ids) {
            parts.push(ids);
            return Some(());
        }
        if budget < 2 {
            return None;
        }
        // Sweep brick origins along each axis. Exclude splits that would spend
        // too many parent IDs, and prefer the minimum feasible parent count.
        let mut best = None;
        let mut right_cost = vec![0.; count];
        for axis in 0..3 {
            ids.sort_unstable_by_key(|&i| (self.bricks[i].origin[axis], i));
            let mut lo = Vec3::splat(f32::INFINITY);
            let mut hi = -lo;
            let pos = |i: usize| Vec3::from_array(self.bricks[i].origin.map(|v| v as f32));
            for k in (0..count).rev() {
                let p = pos(ids[k]);
                lo = lo.min(p);
                hi = hi.max(p);
                right_cost[k] = (count - k) as f32 * (hi - lo).length_squared();
            }
            lo = Vec3::splat(f32::INFINITY);
            hi = -lo;
            let mut left_cells = 0;
            let mut left_tris = 0;
            for k in 1..count {
                let i = ids[k - 1];
                let p = pos(i);
                lo = lo.min(p);
                hi = hi.max(p);
                left_cells += self.bricks[i].surface_cells;
                left_tris += self.bricks[i].triangles;
                let left = required(k, left_cells, left_tris);
                let right = required(count - k, cells - left_cells, tris - left_tris);
                let total = left + right;
                if total > budget {
                    continue;
                }
                let cost = k as f32 * (hi - lo).length_squared() + right_cost[k];
                let key = (total, cost, axis, k, left, right);
                if best.is_none_or(|old| key < old) {
                    best = Some(key);
                }
            }
        }
        let (_, _, axis, k, left, right) = best?;
        ids.sort_unstable_by_key(|&i| (self.bricks[i].origin[axis], i));
        let upper = ids.split_off(k);
        // Unused capacity of the first side is available to the second.
        let start = parts.len();
        self.split(ids, budget - right, parts)?;
        debug_assert!(parts.len() - start >= left);
        self.split(upper, budget - (parts.len() - start), parts)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{PlantBone, Replacement, Vec2};
    use super::*;

    fn cell(x: i32, joint: u32) -> Cell {
        Cell {
            grid: [x, 0, 0],
            joint,
            normals: [0; 3],
            uv: Vec2::ZERO,
        }
    }

    #[test]
    fn shifted_bricks_consume_across_aligned_grid_boundaries_once_per_pose() {
        let layout = Layout::new(vec![cell(1, 1), cell(0, 0), cell(-1, 0), cell(2, 0)]);
        assert_eq!(layout.bricks.len(), 2);
        let b = &layout.bricks[0];
        assert_eq!(b.origin, [-1, 0, 0]);
        let bits: Vec<_> = layout.cells[b.cells.clone()]
            .iter()
            .map(|c| bit(c.grid, b.origin))
            .collect();
        assert_eq!(bits, [0, 1, 3]);
        assert_eq!(layout.bricks[1].origin, [1, 0, 0]);
        assert_eq!(layout.cells.len(), 4);
    }

    #[test]
    fn interior_cells_do_not_consume_visibility_pages() {
        let mut cells = Vec::new();
        for z in 0..12 {
            for y in 0..12 {
                for x in 0..12 {
                    cells.push(Cell {
                        grid: [x, y, z],
                        ..cell(0, 0)
                    });
                }
            }
        }
        let layout = Layout::new(cells);
        assert_eq!(layout.cells.len(), 1728);
        assert_eq!(layout.faces.iter().filter(|&&m| m != 0).count(), 728);
        assert_eq!(layout.partition(1).unwrap().len(), 1);
    }

    #[test]
    fn group_budget_can_select_more_than_one_parent_during_reduction() {
        use super::super::{ChildCut, ClusterNode, Mesh, Meshlet, SourceSurface, reduce};
        let faces: u32 = 768;
        let mut mesh = Mesh {
            positions: Vec::new(),
            indices: (0..3 * faces).collect(),
            normals: vec![],
            uv: vec![[0.; 2]; 3 * faces as usize],
            joints: vec![[0; 4]; 3 * faces as usize],
            weights: vec![[1., 0., 0., 0.]; 3 * faces as usize],
        };
        for i in 0..faces {
            let x = (i % 16) as f32 * 4.;
            let y = (i / 16 % 16) as f32 * 4.;
            let z = (i / 256) as f32 * 5. + 0.23;
            mesh.positions
                .extend([[x, y, z], [x + 1., y, z], [x, y + 1., z]]);
        }
        let mut meshlets: Vec<_> = (0..faces)
            .collect::<Vec<_>>()
            .chunks(64)
            .map(|ids| Meshlet {
                triangles: ids.to_vec(),
                refined_group: None,
            })
            .collect();
        let coarse = meshlets.len() as u32;
        meshlets.push(Meshlet {
            triangles: vec![0],
            refined_group: Some(0),
        });
        let group = |meshlets| ClusterNode {
            meshlets,
            center: [30., 30., 5.],
            radius: 50.,
            error: 12.,
        };
        let dag = [group((0..coarse).collect()), group(vec![coarse])];
        let surface = SourceSurface::new(&mesh, &meshlets);
        let cut = ChildCut::new(&mesh, &dag[0], &meshlets, &[], &[]);
        let (choice, error) = reduce(&mesh, &surface, &cut, &[0], Some(12.), 50., 0.1);
        let r = choice.unwrap();
        assert!((2..=3).contains(&r.parts.len()));
        assert!(r.layout.bricks.len() > MAX_BRICKS);
        assert_eq!(error, r.size);
        let ids: std::collections::BTreeSet<_> = r.parts.iter().flatten().copied().collect();
        assert_eq!(ids.len(), r.layout.bricks.len());
        assert!(r.parts.iter().all(|p| p.len() <= MAX_BRICKS));
    }

    #[test]
    fn sibling_parents_share_lod_and_emit_only_the_outer_surface() {
        let count = 520;
        let layout = Layout::new((0..count).map(|x| cell(x, 0)).collect());
        assert_eq!(layout.bricks.len(), 130);
        assert!(layout.partition(1).is_none());
        let parts = layout.partition(2).unwrap();
        assert_eq!(parts.len(), 2);
        let replacement = Replacement {
            size: 1.,
            source_area: 1.,
            layout,
            parts,
            extra_cells: vec![],
        };
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut pages = Vec::new();
        let meta = [9, 3, 2, 1];
        let parents = replacement
            .emit(
                &mut vertices,
                &mut indices,
                &mut pages,
                meta,
                &[PlantBone {
                    parent: None,
                    pivot: [0.; 3],
                    bend: 0.,
                }],
            )
            .unwrap();
        assert_eq!(parents.len(), 2);
        let mut decoded = std::collections::BTreeSet::new();
        for p in &parents {
            assert_eq!(p.meta[..3], meta[..3]);
            let stream = p.span[0] as usize;
            let bricks = vertices[stream] as usize;
            assert!((1..=128).contains(&bricks));
            let attr = stream + 1 + 8 * bricks;
            for i in 0..bricks {
                let h = stream + 1 + 8 * i;
                let x = f32::from_bits(vertices[h]) as i32;
                let mask = u64::from(vertices[h + 4]) | (u64::from(vertices[h + 5]) << 32);
                let mut slot = (vertices[h + 6] & 65535) as usize;
                for b in 0..64 {
                    if mask & (1u64 << b) == 0 {
                        continue;
                    }
                    let tag = vertices[attr + 6 * slot + 5];
                    assert_eq!(tag & 63, b as u32);
                    assert_eq!((tag >> 6) & 127, i as u32);
                    let world = x + b;
                    assert!(decoded.insert(world));
                    let faces = (tag >> 13) & 63;
                    assert_eq!(
                        faces & 3,
                        u32::from(world == 0) | (u32::from(world == count - 1) << 1)
                    );
                    let page = &pages[(p.span[2] + (tag >> 24)) as usize];
                    assert_eq!(page.span[0], p.span[0]);
                    assert_eq!(page.meta[..3], meta[..3]);
                    slot += 1;
                }
            }
        }
        assert_eq!(decoded.len(), count as usize);
        assert_eq!(
            parents.iter().map(|p| p.span[3]).sum::<u32>(),
            (2 * (4 * count + 2)) as u32
        );
    }
}
