use crate::scene::{ClusterNode, Mesh, Meshlet};
use anyhow::Result;
use glam::Vec3;
use std::collections::{BTreeMap, BTreeSet};

fn face(indices: &[u32], id: u32) -> [u32; 3] {
    let i = id as usize * 3;
    [indices[i], indices[i + 1], indices[i + 2]]
}

// An edge used by more than one triangle has no open boundary.
fn edges(triangles: impl Iterator<Item = [u32; 3]>) -> BTreeMap<[u32; 2], Option<u32>> {
    let mut edges = BTreeMap::new();
    for [a, b, c] in triangles {
        for (a, b, opposite) in [(a, b, c), (b, c, a), (c, a, b)] {
            edges
                .entry([a.min(b), a.max(b)])
                .and_modify(|value| *value = None)
                .or_insert(Some(opposite));
        }
    }
    edges
}

fn area(mesh: &Mesh, faces: &[u32]) -> f32 {
    faces
        .iter()
        .map(|&id| {
            let [a, b, c] = face(&mesh.indices, id).map(|i| Vec3::from(mesh.positions[i as usize]));
            (b - a).cross(c - a).length() * 0.5
        })
        .sum()
}

/// Restore some of the area lost when a simplification drops disconnected sheets.
/// Only coarse faces receive new vertices. Vertices at a simplification seam stay pinned.
pub(super) fn preserve_area(
    mesh: &mut Mesh,
    source_faces: usize,
    groups: &mut [ClusterNode],
    clusters: &[Meshlet],
) -> Result<f32> {
    let original = mesh.indices.clone();
    let source = edges(original[..source_faces * 3].as_chunks().0.iter().copied());
    let mut replacements = vec![Vec::new(); groups.len()];
    for cluster in clusters {
        if let Some(finer) = cluster.refined_group {
            replacements[finer as usize].extend_from_slice(&cluster.triangles);
        }
    }

    let mut max_expansion = 0.0f32;
    for (group, coarse) in groups
        .iter_mut()
        .zip(replacements)
        .filter(|(_, faces)| !faces.is_empty())
    {
        let fine = group
            .meshlets
            .iter()
            .flat_map(|&i| clusters[i as usize].triangles.iter().copied())
            .collect::<Vec<_>>();
        let fine_area = area(mesh, &fine);
        let missing = fine_area - area(mesh, &coarse);
        if missing <= 0.1 * fine_area {
            continue;
        }
        let open_in_source = |edge| source.get(&edge).is_some_and(Option::is_some);
        let pinned = edges(fine.iter().map(|&id| face(&original, id)))
            .into_iter()
            .filter(|&(edge, opposite)| opposite.is_some() && !open_in_source(edge))
            .flat_map(|(edge, _)| edge)
            .collect::<BTreeSet<_>>();
        let mut boundary = BTreeMap::<u32, (Vec3, f32, f32)>::new();
        let mut perimeter = 0.0;
        for ([a, b], opposite) in edges(coarse.iter().map(|&id| face(&original, id))) {
            let Some(c) = opposite else { continue };
            if !open_in_source([a, b]) || (pinned.contains(&a) && pinned.contains(&b)) {
                continue;
            }
            let [p, q, r] = [a, b, c].map(|i| Vec3::from(mesh.positions[i as usize]));
            let length = p.distance(q);
            let tangent = (q - p).normalize_or_zero();
            let outside = (p - r - tangent * (p - r).dot(tangent)).normalize_or_zero();
            if outside != Vec3::ZERO {
                for i in [a, b].into_iter().filter(|i| !pinned.contains(i)) {
                    let entry = boundary.entry(i).or_insert((Vec3::ZERO, 0., f32::INFINITY));
                    entry.0 += outside * length;
                    entry.1 += length;
                    entry.2 = entry.2.min(length * 0.35);
                    perimeter += length * 0.5;
                }
            }
        }
        if perimeter == 0. {
            continue;
        }
        let mut expanded = BTreeMap::new();
        for (old, (direction, weight, limit)) in boundary {
            let average = direction / weight;
            let offset = (average * (missing / (perimeter * average.length_squared().max(0.25))))
                .clamp_length_max(limit);
            if offset == Vec3::ZERO {
                continue;
            }
            let index = mesh.positions.len().try_into()?;
            let old = old as usize;
            let position = Vec3::from(mesh.positions[old]) + offset;
            mesh.positions.push(position.into());
            if !mesh.normals.is_empty() {
                mesh.normals.push(mesh.normals[old]);
            }
            if !mesh.uv.is_empty() {
                mesh.uv.push(mesh.uv[old]);
            }
            mesh.joints.push(mesh.joints[old]);
            mesh.weights.push(mesh.weights[old]);
            expanded.insert(old as u32, index);
            let length = offset.length();
            max_expansion = max_expansion.max(length);
            group.error = group.error.max(length);
            group.radius = group.radius.max(position.distance(group.center.into()));
        }
        for id in coarse {
            let start = id as usize * 3;
            for vertex in &mut mesh.indices[start..start + 3] {
                if let Some(&index) = expanded.get(vertex) {
                    *vertex = index;
                }
            }
        }
    }
    Ok(max_expansion)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_coarse_leaf_area_without_moving_source_or_opening_a_seam() -> Result<()> {
        let mut mesh = Mesh {
            positions: vec![
                [0., 0., 0.],
                [1., 0., 0.],
                [0., 1., 0.],
                [0., -1., 0.],
                [2., 0., 0.],
                [3., 0., 0.],
                [2., 1., 0.],
            ],
            indices: vec![0, 1, 2, 1, 0, 3, 4, 5, 6, 0, 1, 2],
            normals: vec![[0., 0., 1.]; 7],
            uv: vec![[0.4, 0.5]; 7],
            joints: vec![[2, 0, 0, 0]; 7],
            weights: vec![[1., 0., 0., 0.]; 7],
        };
        let source = mesh.positions.clone();
        let mut groups = [ClusterNode {
            meshlets: vec![0],
            center: [0.; 3],
            radius: 4.,
            error: 0.05,
        }];
        let clusters = [
            Meshlet {
                triangles: vec![0, 2],
                refined_group: None,
            },
            Meshlet {
                triangles: vec![3],
                refined_group: Some(0),
            },
        ];
        let before = area(&mesh, &[3]);
        assert!(preserve_area(&mut mesh, 3, &mut groups, &clusters)? > 0.);
        assert!(area(&mesh, &[3]) > before);
        assert_eq!(mesh.positions[..7], source);
        assert_eq!(mesh.indices[..11], [0, 1, 2, 1, 0, 3, 4, 5, 6, 0, 1]);
        let grown = mesh.indices[11] as usize;
        assert!(grown >= source.len());
        assert_eq!(mesh.joints[grown], mesh.joints[2]);
        assert_eq!(mesh.weights[grown], mesh.weights[2]);
        Ok(())
    }
}
