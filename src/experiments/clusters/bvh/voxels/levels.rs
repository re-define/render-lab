//! Build a transient mixed DAG from the saved leaf meshlets. Its group IDs are local to the packed prototype.
use super::{
    grouping::{self, Member},
    *,
};
use meshoptimizer_rs::{
    Attributes, MeshletSettings, Positions, SimplifyOptions, SimplifySettings, VertexFlags,
    Workspace, build_meshlets, simplify_with_attributes,
};
use std::collections::{HashMap, HashSet};

pub(in super::super) struct Hierarchy {
    pub mesh: Mesh,
    pub nodes: Vec<ClusterNode>,
    pub meshlets: Vec<Meshlet>,
    pub choices: Vec<Option<Replacement>>,
}

fn triangle_bounds(mesh: &Mesh, faces: &[u32]) -> [f32; 4] {
    let (lo, hi) = faces.iter().flat_map(|&f| positions(mesh, f)).fold(
        (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
        |(lo, hi), p| (lo.min(p), hi.max(p)),
    );
    sphere(lo, hi)
}

type PositionKey = [u32; 3];
pub(super) fn position_key(p: [f32; 3]) -> PositionKey {
    p.map(|x| if x == 0. { 0 } else { x.to_bits() })
}
fn keys<'a>(
    mesh: &'a Mesh,
    meshlets: &'a [Meshlet],
    members: &'a [Member],
) -> impl Iterator<Item = PositionKey> + 'a {
    members
        .iter()
        .filter(|m| !m.voxel)
        .flat_map(move |m| meshlets[m.slot as usize].triangles.iter())
        .flat_map(move |&f| triangle(mesh, f))
        .map(move |v| position_key(mesh.positions[v]))
}

fn shared_positions(mesh: &Mesh, slots: &[Meshlet], sets: &[Vec<Member>]) -> HashSet<PositionKey> {
    let mut owners = HashMap::new();
    let mut locks = HashSet::new();
    for (owner, members) in sets.iter().enumerate() {
        for pos in keys(mesh, slots, members) {
            if *owners.entry(pos).or_insert(owner) != owner {
                locks.insert(pos);
            }
        }
    }
    locks
}

struct Candidate {
    meshes: Vec<Vec<[u32; 3]>>,
    error: f32,
}

fn simplify(
    mesh: &Mesh,
    slots: &[Meshlet],
    members: &[Member],
    locks: &HashSet<PositionKey>,
) -> Result<Option<Candidate>> {
    if members.iter().any(|m| m.voxel) {
        return Ok(None);
    }
    let mut map = HashMap::new();
    let mut original = Vec::new();
    let mut indices = Vec::new();
    for m in members {
        for &f in &slots[m.slot as usize].triangles {
            for i in triangle(mesh, f) {
                let id = *map.entry(i).or_insert_with(|| {
                    let id = original.len() as u32;
                    original.push(i);
                    id
                });
                indices.push(id);
            }
        }
    }
    let xyz = original
        .iter()
        .map(|&i| mesh.positions[i])
        .collect::<Vec<_>>();
    let flags = xyz
        .iter()
        .map(|&p| {
            if locks.contains(&position_key(p)) {
                VertexFlags::LOCK
            } else {
                VertexFlags::default()
            }
        })
        .collect::<Vec<_>>();
    let mut attrs = Vec::with_capacity(5 * original.len());
    for &i in &original {
        attrs.extend(mesh.normals.get(i).copied().unwrap_or_default());
        attrs.extend(mesh.uv.get(i).copied().unwrap_or_default());
    }
    let mut ws = Workspace::default();
    let pos = Positions::from_packed(&xyz);
    let reduced = simplify_with_attributes(
        &indices,
        pos,
        Attributes::from_interleaved(&attrs, xyz.len(), 5, 5, 0)?,
        &[0.05, 0.05, 0.05, 0.1, 0.1],
        Some(&flags),
        SimplifySettings {
            target_index_count: (indices.len() / 6).max(1) * 3,
            target_error: 1e12,
            options: SimplifyOptions::ERROR_ABSOLUTE | SimplifyOptions::PRUNE,
        },
        &mut ws,
    )?;
    if reduced.indices.is_empty() || reduced.indices.len() >= indices.len() {
        return Ok(None);
    }
    let built = build_meshlets(
        &reduced.indices,
        pos,
        MeshletSettings {
            max_vertices: 64,
            max_triangles: 64,
        },
        0.,
        &mut ws,
    )?;
    let meshes = built
        .meshlets
        .iter()
        .map(|m| {
            let start = m.triangle_offset as usize;
            built.triangles[start..start + 3 * m.triangle_count as usize]
                .chunks_exact(3)
                .map(|t| {
                    std::array::from_fn(|k| {
                        original[built.vertices[m.vertex_offset as usize + t[k] as usize] as usize]
                            as u32
                    })
                })
                .collect()
        })
        .collect();
    Ok(Some(Candidate {
        meshes,
        error: reduced.error,
    }))
}

impl Hierarchy {
    pub(in super::super) fn build(
        source: &Mesh,
        slots: &[Meshlet],
        stored: &[ClusterNode],
        base_size: f32,
    ) -> Result<Self> {
        let ids: BTreeSet<_> = stored
            .iter()
            .flat_map(|g| g.meshlets.iter().copied())
            .collect();
        let mut leaves = Vec::new();
        for id in ids {
            let c = slots
                .get(id as usize)
                .context("invalid meshlet in saved plant")?;
            if c.refined_group.is_none() && !c.triangles.is_empty() {
                leaves.push(c);
            }
        }
        let surface = SourceSurface::new(source, leaves.iter().copied());
        let mut tree = Self {
            mesh: source.clone(),
            nodes: vec![],
            meshlets: vec![],
            choices: vec![],
        };
        let mut frontier = Vec::new();
        let mut errors = Vec::new();
        for c in leaves {
            let b = triangle_bounds(source, &c.triangles);
            frontier.push(Member {
                slot: tree.meshlets.len().try_into()?,
                bounds: b,
                lod: b,
                physical: 1,
                voxel: false,
            });
            tree.meshlets.push(c.clone());
        }
        ensure!(!frontier.is_empty(), "plant has no leaf meshlets");
        while !frontier.is_empty() {
            let total = frontier.iter().map(|m| m.physical).sum::<usize>();
            let sets = grouping::partition(&tree.mesh, &tree.meshlets, frontier)?;
            let locks = shared_positions(&tree.mesh, &tree.meshlets, &sets);
            let proposals: Result<Vec<_>> = sets
                .par_iter()
                .map(|set| {
                    if total == 1 {
                        Ok(None)
                    } else {
                        simplify(&tree.mesh, &tree.meshlets, set, &locks)
                    }
                })
                .collect();
            // Reserve face IDs in stable group order before workers read the new triangle candidates.
            let mut inputs = Vec::with_capacity(sets.len());
            for (set, candidate) in sets.iter().zip(proposals?) {
                let b = set
                    .iter()
                    .map(|m| m.lod)
                    .reduce(grouping::union)
                    .expect("nonempty group");
                let node = ClusterNode {
                    meshlets: set.iter().map(|m| m.slot).collect(),
                    center: [b[0], b[1], b[2]],
                    radius: b[3],
                    error: f32::MAX,
                };
                let mut parents = Vec::new();
                let mut qem = None;
                if let Some(candidate) = candidate {
                    qem = Some(candidate.error);
                    for faces in candidate.meshes {
                        let start = u32::try_from(tree.mesh.indices.len() / 3)?;
                        tree.mesh.indices.extend(faces.into_iter().flatten());
                        parents.push(
                            (start..u32::try_from(tree.mesh.indices.len() / 3)?)
                                .collect::<Vec<_>>(),
                        );
                    }
                }
                inputs.push((node, parents, qem));
            }
            let reductions: Vec<_> = inputs
                .par_iter()
                .zip(&sets)
                .map(|((node, parents, qem), set)| {
                    if total == 1 {
                        return (None, f32::MAX);
                    }
                    let cut =
                        ChildCut::new(&tree.mesh, node, &tree.meshlets, &tree.choices, &errors);
                    let coarse: Vec<_> = parents.iter().flatten().copied().collect();
                    reduce(
                        &tree.mesh,
                        &surface,
                        &cut,
                        &coarse,
                        *qem,
                        grouping::bounds(set)[3],
                        base_size,
                    )
                })
                .collect();
            // Keep an unreduced region available to other neighbors at the next level.
            // Every successful triangle reduction removes faces; a pure voxel reduction removes cells.
            let progressed = inputs
                .iter()
                .zip(&reductions)
                .any(|((_, _, qem), (vox, _))| qem.is_some() || vox.is_some());
            frontier = Vec::new();
            for ((members, (mut node, parents, qem)), (voxel, error)) in
                sets.into_iter().zip(inputs).zip(reductions)
            {
                if progressed && voxel.is_none() && qem.is_none() {
                    frontier.extend(members);
                    continue;
                }
                let id = u32::try_from(tree.nodes.len())?;
                let lod = [node.center[0], node.center[1], node.center[2], node.radius];
                if let Some(r) = &voxel {
                    // A single slot represents all siblings for the next build operation. GPU emission expands it.
                    let b = grouping::bounds(&members);
                    frontier.push(Member {
                        slot: tree.meshlets.len().try_into()?,
                        bounds: b,
                        lod,
                        physical: r.parts.len(),
                        voxel: true,
                    });
                    tree.meshlets.push(Meshlet {
                        triangles: vec![],
                        refined_group: Some(id),
                    });
                    node.error = error;
                } else if qem.is_some() {
                    for triangles in parents {
                        frontier.push(Member {
                            slot: tree.meshlets.len().try_into()?,
                            bounds: triangle_bounds(&tree.mesh, &triangles),
                            lod,
                            physical: 1,
                            voxel: false,
                        });
                        tree.meshlets.push(Meshlet {
                            triangles,
                            refined_group: Some(id),
                        });
                    }
                    node.error = error;
                }
                errors.push(node.error);
                tree.choices.push(voxel);
                tree.nodes.push(node);
            }
        }
        Ok(tree)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_indices_along_a_shared_seam_survive_both_triangle_reductions() -> Result<()> {
        let mut mesh = Mesh {
            positions: vec![],
            normals: vec![],
            uv: vec![],
            indices: vec![],
            joints: vec![],
            weights: vec![],
        };
        let mut slots = Vec::new();
        let mut sets = Vec::new();
        for side in 0..2 {
            let base = mesh.positions.len() as u32;
            for y in 0..5 {
                for x in 0..5 {
                    mesh.positions.push([(side * 4 + x) as f32, y as f32, 0.]);
                }
            }
            let start = mesh.indices.len() as u32 / 3;
            for y in 0..4 {
                for x in 0..4 {
                    let v = base + 5 * y + x;
                    mesh.indices.extend([v, v + 1, v + 5, v + 1, v + 6, v + 5]);
                }
            }
            slots.push(Meshlet {
                triangles: (start..mesh.indices.len() as u32 / 3).collect(),
                refined_group: None,
            });
            sets.push(vec![Member {
                slot: side,
                bounds: [0.; 4],
                lod: [0.; 4],
                voxel: false,
                physical: 1,
            }]);
        }
        let locked = shared_positions(&mesh, &slots, &sets);
        assert_eq!(locked.len(), 5);
        for set in &sets {
            let reduced = simplify(&mesh, &slots, set, &locked)?.expect("patch should simplify");
            let mut edges = BTreeSet::new();
            for tri in reduced.meshes.iter().flatten() {
                for i in 0..3 {
                    let a = mesh.positions[tri[i] as usize];
                    let b = mesh.positions[tri[(i + 1) % 3] as usize];
                    if a[0] == 4. && b[0] == 4. {
                        edges.insert((a[1].min(b[1]) as u32, a[1].max(b[1]) as u32));
                    }
                }
            }
            assert_eq!(edges, (0..4).map(|y| (y, y + 1)).collect());
        }
        Ok(())
    }
}
