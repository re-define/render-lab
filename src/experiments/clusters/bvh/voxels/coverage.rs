//! Reduce partially covered samples to binary occupancy with directional neighbor support.
use super::sampling::{CoveredCell, RAYS};
use glam::Vec3;
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BinaryHeap},
};

pub(super) fn reduce(samples: &[CoveredCell]) -> Vec<bool> {
    const FULL: u32 = 65535;
    let index: BTreeMap<_, _> = samples
        .iter()
        .enumerate()
        .map(|(i, s)| (s.cell.grid, i))
        .collect();
    let mut scores: Vec<_> = samples
        .iter()
        .map(|s| u32::from(s.hits) * FULL / RAYS as u32)
        .collect();
    let mut weakest: BinaryHeap<_> = scores
        .iter()
        .copied()
        .enumerate()
        .map(|(i, w)| Reverse((w, i)))
        .collect();
    let target = (samples.iter().map(|s| usize::from(s.hits)).sum::<usize>() / RAYS).max(1);
    let mut remaining = samples.len();
    let mut keep = vec![true; remaining];
    while remaining > target {
        let Some(Reverse((score, i))) = weakest.pop() else {
            break;
        };
        if !keep[i] || scores[i] != score {
            continue;
        }
        keep[i] = false;
        remaining -= 1;
        let cell = samples[i].cell;
        let lobes = cell.normals.map(|p| {
            let [x, y, z, w] = p.to_le_bytes();
            Vec3::new(x as i8 as f32, y as i8 as f32, z as i8 as f32).normalize_or_zero()
                * f32::from(w)
        });
        let mut neighbors = [(0usize, 0f32); 26];
        let mut used = 0;
        let mut total = 0.;
        for z in -1..=1 {
            for y in -1..=1 {
                for x in -1..=1 {
                    let grid = [cell.grid[0] + x, cell.grid[1] + y, cell.grid[2] + z];
                    let Some(&j) = index.get(&grid) else { continue };
                    if !keep[j] {
                        continue;
                    }
                    let direction = Vec3::new(x as f32, y as f32, z as f32).normalize();
                    let weight: f32 = lobes.iter().map(|n| n.dot(direction).abs()).sum();
                    if weight > 0. {
                        neighbors[used] = (j, weight);
                        used += 1;
                        total += weight;
                    }
                }
            }
        }
        for &(j, weight) in &neighbors[..used] {
            let transfer = score as f32 * weight / total;
            // Union of the neighbor's covered fraction with the transferred fraction.
            let next =
                scores[j] + (transfer * (1. - scores[j] as f32 / FULL as f32)).round() as u32;
            if next > scores[j] {
                scores[j] = next.min(FULL);
                weakest.push(Reverse((scores[j], j)));
            }
        }
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::super::Cell;
    use super::*;
    use glam::Vec2;

    #[test]
    fn discarded_coverage_favors_neighbors_along_the_projected_normal() {
        let make = |grid, hits| CoveredCell {
            cell: Cell {
                grid,
                uv: Vec2::ZERO,
                joint: 0,
                normals: [u32::from_le_bytes([127, 0, 0, 255]), 0, 0],
            },
            hits,
        };
        // The weakest cell can transfer coverage along X, not perpendicular to it.
        let cells = [make([0, 0, 0], 8), make([1, 0, 0], 20), make([0, 1, 0], 21)];
        assert_eq!(reduce(&cells), [false, true, false]);
        assert!(reduce(&[]).is_empty());
        assert_eq!(reduce(&[make([-4, 0, 0], 1)]), [true]);
    }
}
