//! An eight-way spatial BVH whose leaves reference complete cluster groups.
//! Geometry bounds answer visibility queries; nested error spheres answer LOD queries.
use crate::{render::MeshAsset, scene::*};
use anyhow::{Context, Result, ensure};
use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use meshoptimizer_rs::clusterlod::{self as cl, cut};
use std::{collections::HashMap, sync::Arc};

const NONE: u32 = u32::MAX;
mod voxels;

#[derive(Copy, Clone, Pod, Zeroable)]
#[repr(C)]
struct Group {
    sphere: [f32; 4],
    error: f32,
    level: u32,
    first_cluster: u32,
    cluster_count: u32,
}
#[derive(Copy, Clone, Pod, Zeroable)]
#[repr(C)]
struct Cluster {
    span: [u32; 4],
    sphere: [f32; 4],
    meta: [u32; 4],
}
#[derive(Copy, Clone, Pod, Zeroable, Default)]
#[repr(C)]
struct Node {
    geometry: [f32; 4],
    projection: [f32; 4],
    children: u32,
    first: u32,
    group: u32,
    max_error: f32,
}

pub(super) struct Streams {
    pub tree: Arc<[u8]>,
    pub groups: Arc<[u8]>,
    pub clusters: Arc<[u8]>,
    pub vertices: Arc<[u8]>,
    pub indices: Arc<[u8]>,
    pub level_widths: Vec<u32>,
    pub shadow_elements: [u64; 2],
    pub count: u32,
    pub voxel_count: u32,
    pub brick_draws: u32,
    pub shadow_count: u32,
}
pub(super) struct Packed {
    pub proxy: std::sync::OnceLock<Arc<[u8]>>,
    pub shadow_detail: std::sync::OnceLock<super::gpu::proxy::Detail>,
    pub streams: Streams,
    pub voxel_replacements: u32,
    pub bones: Option<Arc<[u8]>>,
    // Holding the source Arcs makes cache identity safe across scene edits.
    mesh: Arc<Mesh>,
    meshlets: Arc<Vec<Meshlet>>,
    dag: Arc<Vec<ClusterNode>>,
    plant: Option<Arc<PlantGeometry>>,
    mixed: bool,
}
fn bytes<T: Pod>(v: &[T]) -> Arc<[u8]> {
    bytemuck::cast_slice(v).into()
}
fn sphere(lo: Vec3, hi: Vec3) -> [f32; 4] {
    let c = (lo + hi) * 0.5;
    c.extend(
        (hi - c)
            .length()
            .max((lo - c).length())
            .mul_add(1.000002, 1e-6),
    )
    .into()
}
fn extend(lo: &mut Vec3, hi: &mut Vec3, s: [f32; 4]) {
    let c = Vec3::new(s[0], s[1], s[2]);
    *lo = lo.min(c - s[3]);
    *hi = hi.max(c + s[3]);
}
/// Siblings occupy one consecutive range. Partition group leaves, keeping the
/// cluster ranges and group IDs from the LOD DAG stable.
fn spatial_node(
    leaves: &mut [Node],
    groups: &[Group],
    nodes: &mut Vec<Node>,
    id: usize,
    depth: usize,
    widths: &mut Vec<u32>,
) {
    // Only roots and internal children become candidate-node jobs. A visiting
    // lane emits a leaf's clusters immediately.
    if depth == 0 || leaves.len() > 1 {
        if widths.len() == depth {
            widths.push(0);
        }
        widths[depth] += 1;
    }
    if leaves.len() == 1 {
        nodes[id] = leaves[0];
        return;
    }

    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = -lo;
    let (mut elo, mut ehi) = (lo, hi);
    let mut max_error = 0f32;
    for leaf in leaves.iter() {
        extend(&mut lo, &mut hi, leaf.geometry);
        extend(&mut elo, &mut ehi, leaf.projection);
        max_error = max_error.max(leaf.max_error);
    }
    let size = hi - lo;
    let axis = if size.x >= size.y && size.x >= size.z {
        0
    } else if size.y >= size.z {
        1
    } else {
        2
    };
    leaves.sort_unstable_by(|a, b| {
        groups[a.group as usize]
            .level
            .cmp(&groups[b.group as usize].level)
            .then_with(|| a.geometry[axis].total_cmp(&b.geometry[axis]))
    });
    let chunk = leaves.len().div_ceil(8);
    let children = leaves.len().div_ceil(chunk);
    let base = nodes.len();
    nodes[id] = Node {
        geometry: sphere(lo, hi),
        projection: sphere(elo, ehi),
        max_error,
        first: base as u32,
        children: children as u32,
        group: NONE,
    };
    nodes.resize(base + children, Node::default());
    for (i, sub) in leaves.chunks_mut(chunk).enumerate() {
        spatial_node(sub, groups, nodes, base + i, depth + 1, widths);
    }
}

fn foliage_coverage_errors(mesh: &Mesh, dag: &[ClusterNode], clusters: &[Meshlet]) -> Vec<f32> {
    let area = clusters
        .iter()
        .map(|cluster| {
            cluster
                .triangles
                .iter()
                .map(|&face| {
                    let start = face as usize * 3;
                    let [a, b, c] = std::array::from_fn(|corner| {
                        let vertex = mesh.indices[start + corner] as usize;
                        Vec3::from(mesh.positions[vertex])
                    });
                    (b - a).cross(c - a).abs() * 0.5
                })
                .sum::<Vec3>()
        })
        .collect::<Vec<_>>();
    let mut parents = vec![None; dag.len()];
    for (cluster, projected_area) in clusters.iter().zip(&area) {
        if let Some(finer) = cluster.refined_group {
            *parents[finer as usize].get_or_insert(Vec3::ZERO) += *projected_area;
        }
    }
    parents
        .into_iter()
        .zip(dag)
        .map(|(coarse, fine)| {
            coarse.map_or(0., |coarse| {
                let fine = fine
                    .meshlets
                    .iter()
                    .map(|&id| area[id as usize])
                    .sum::<Vec3>();
                let missing = (fine - coarse).max(Vec3::ZERO);
                // The square root expresses significant projected area loss as a
                // length in the same units as the simplifier's positional error.
                let significant = Vec3::select(missing.cmpgt(fine * 0.15), missing, Vec3::ZERO);
                0.55 * significant.max_element().sqrt()
            })
        })
        .collect()
}

/// Shared by preprocessing and the lazy upgrade of previously saved DAGs.
pub(super) fn repaired(
    dag: &[ClusterNode],
    clusters: &[Meshlet],
    errors: Option<&[f32]>,
) -> Result<Vec<cl::FlatDagGroup>> {
    let mut flat: Vec<cl::FlatDagGroup> = vec![];
    let mut entries = vec![];
    for (id, node) in dag.iter().enumerate() {
        let mut g = cl::FlatDagGroup::default();
        g.center = node.center;
        g.radius = node.radius;
        g.error = errors.map_or(node.error, |errors| errors[id]);
        g.first_cluster = entries.len().try_into()?;
        g.cluster_count = node.meshlets.len().try_into()?;
        for &c in &node.meshlets {
            let child = clusters[c as usize].refined_group;
            let mut cluster = cl::FlatDagCluster::default();
            cluster.group = id.try_into()?;
            cluster.refined = child.map_or(-1, |v| v as i32);
            if let Some(i) = child {
                g.depth = g
                    .depth
                    .max(flat.get(i as usize).context("unordered cluster DAG")?.depth + 1);
            }
            entries.push(cluster);
        }
        flat.push(g);
    }
    cut::enforce_monotonic(&mut flat, &entries)?;
    Ok(flat)
}

impl Packed {
    pub fn matches(&self, e: &MeshAsset, mixed: bool) -> bool {
        self.mixed == mixed
            && Arc::ptr_eq(&self.mesh, &e.mesh)
            && e.meshlets
                .as_ref()
                .is_some_and(|m| Arc::ptr_eq(&self.meshlets, m))
            && e.hierarchy
                .as_ref()
                .is_some_and(|h| Arc::ptr_eq(&self.dag, h))
            && match (&self.plant, &e.plant) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
    pub fn build(e: &MeshAsset, mixed: bool) -> Result<Self> {
        let source_mesh = &e.mesh;
        let source_meshlets = e.meshlets.as_ref().context("missing meshlets")?;
        let source_dag = e.hierarchy.as_ref().context("missing DAG")?;
        let size = e.plant.as_ref().and_then(|plant| plant.voxel_cell_size);
        let generated = if let (true, Some(size)) = (mixed, size) {
            Some(voxels::Hierarchy::build(
                source_mesh,
                source_meshlets,
                source_dag,
                size,
            )?)
        } else {
            None
        };
        let (mesh, meshlets, dag) = match &generated {
            Some(tree) => (&tree.mesh, tree.meshlets.as_slice(), tree.nodes.as_slice()),
            None => (
                source_mesh.as_ref(),
                source_meshlets.as_slice(),
                source_dag.as_slice(),
            ),
        };
        let replacements = generated
            .as_ref()
            .map_or(&[][..], |tree| tree.choices.as_slice());
        let errors = if size.is_some() && !mixed {
            Some(
                foliage_coverage_errors(mesh, dag, meshlets)
                    .into_iter()
                    .zip(dag)
                    .map(|(coverage, group)| coverage.max(group.error))
                    .collect::<Vec<_>>(),
            )
        } else {
            None
        };
        let flat = repaired(dag, meshlets, errors.as_deref())?;
        let mut groups = flat
            .iter()
            .map(|g| Group {
                sphere: [g.center[0], g.center[1], g.center[2], g.radius],
                error: g.error,
                level: g.depth as u32,
                first_cluster: 0,
                cluster_count: 0,
            })
            .collect::<Vec<_>>();
        let mut vertices = Vec::<u32>::with_capacity(
            mesh.positions.len() * if e.plant.is_some() { 16 } else { 8 },
        );
        for (i, p) in mesh.positions.iter().enumerate() {
            let n = mesh.normals.get(i).copied().unwrap_or_default();
            let uv = mesh.uv.get(i).copied().unwrap_or_default();
            vertices.extend([p[0], p[1], p[2], uv[0], n[0], n[1], n[2], uv[1]].map(f32::to_bits));
            if e.plant.is_some() {
                vertices.extend(mesh.joints[i].map(u32::from));
                vertices.extend(mesh.weights[i].map(f32::to_bits));
            }
        }
        let padding = e
            .plant
            .as_ref()
            .map_or(0., |plant| plant.displacement_bound);
        let mut indices = vec![];
        let mut packed = vec![];
        let mut voxel_pages = vec![];
        let mut leaves = vec![];
        for (owner, g) in dag.iter().enumerate() {
            let first = packed.len();
            for &id in &g.meshlets {
                let c = &meshlets[id as usize];
                let level = c.refined_group.map_or(0, |i| groups[i as usize].level + 1);
                if let Some(child) = c.refined_group
                    && let Some(Some(voxel)) = replacements.get(child as usize)
                {
                    packed.extend(
                        voxel.emit(
                            &mut vertices,
                            &mut indices,
                            &mut voxel_pages,
                            [owner.try_into()?, child, level, 1],
                            &e.plant
                                .as_ref()
                                .expect("voxel cluster requires a plant rig")
                                .bones,
                        )?,
                    );
                    continue;
                }
                // Older/custom assets may exceed the shader's per-workgroup limits.
                let mut faces = c.triangles.iter().peekable();
                while faces.peek().is_some() {
                    let mut local = HashMap::<u32, u32>::new();
                    let mut verts = vec![];
                    let mut tris = vec![];
                    let mut lo = Vec3::splat(f32::INFINITY);
                    let mut hi = -lo;
                    while let Some(&&t) = faces.peek() {
                        let ids = &mesh.indices[t as usize * 3..][..3];
                        if tris.len() == 64
                            || local.len() + ids.iter().filter(|v| !local.contains_key(*v)).count()
                                > 64
                        {
                            break;
                        }
                        let mut tri = 0;
                        for (j, &v) in ids.iter().enumerate() {
                            let ix = *local.entry(v).or_insert_with(|| {
                                let p = Vec3::from(mesh.positions[v as usize]);
                                lo = lo.min(p);
                                hi = hi.max(p);
                                verts.push(v);
                                verts.len() as u32 - 1
                            });
                            tri |= ix << (8 * j);
                        }
                        tris.push(tri);
                        faces.next();
                    }
                    let start: u32 = indices.len().try_into()?;
                    let nv = verts.len() as u32;
                    let nt = tris.len() as u32;
                    indices.extend(verts);
                    indices.extend(tris);
                    let mut bounds = sphere(lo, hi);
                    bounds[3] += padding;
                    packed.push(Cluster {
                        span: [start, nv, start + nv, nt],
                        sphere: bounds,
                        meta: [owner as u32, c.refined_group.unwrap_or(NONE), level, 0],
                    });
                }
            }
            let members = &packed[first..];
            if !members.is_empty() {
                let group = &mut groups[owner];
                group.first_cluster = first.try_into()?;
                group.cluster_count = members.len().try_into()?;
                let mut lo = Vec3::splat(f32::INFINITY);
                let mut hi = -lo;
                for c in members {
                    extend(&mut lo, &mut hi, c.sphere);
                }
                leaves.push(Node {
                    geometry: sphere(lo, hi),
                    projection: group.sphere,
                    group: owner.try_into()?,
                    max_error: group.error,
                    ..Default::default()
                });
            }
        }
        ensure!(!packed.is_empty(), "empty cluster geometry");
        let count = packed.len().try_into()?;
        let voxel_count = packed
            .iter()
            .filter(|c| c.meta[3] & 1 != 0)
            .count()
            .try_into()?;
        let brick_draws = packed
            .iter()
            .filter(|c| c.meta[3] & 1 != 0)
            .map(|c| u64::from((c.meta[3] >> 1).div_ceil(8)))
            .sum::<u64>()
            .try_into()?;
        let shadow_count: u32 = packed
            .len()
            .checked_add(voxel_pages.len())
            .context("cluster and hit page count overflow")?
            .try_into()?;
        ensure!(
            shadow_count <= 1 << 26,
            "cluster ID exceeds visibility buffer encoding"
        );
        let shadow_elements = [1, 3].map(|i| {
            packed
                .iter()
                .filter(|c| c.meta[3] == 0)
                .chain(&voxel_pages)
                .map(|c| u64::from(c.span[i]))
                .sum()
        });
        for cluster in packed.iter_mut().filter(|c| c.meta[3] & 1 != 0) {
            cluster.span[2] += count;
        }
        // Only the prefix is referenced by BVH leaves. The suffix gives raster
        // hits and hardware triangles identical 26+6-bit visibility IDs.
        packed.extend(voxel_pages);
        let mut nodes = vec![Node::default()];
        let mut level_widths = vec![];
        spatial_node(&mut leaves, &groups, &mut nodes, 0, 0, &mut level_widths);
        let plant = e.plant.clone();
        let bones = plant.as_ref().map(|p| {
            let mut words = vec![p.bones.len() as u32, 0, 0, 0];
            for b in &p.bones {
                words.extend(b.pivot.map(f32::to_bits));
                words.push(b.parent.map_or(NONE, u32::from));
                words.extend([b.bend.to_bits(), 0, 0, 0]);
            }
            bytes(&words)
        });
        Ok(Self {
            proxy: Default::default(),
            shadow_detail: Default::default(),
            streams: Streams {
                tree: bytes(&nodes),
                level_widths,
                shadow_elements,
                count,
                voxel_count,
                brick_draws,
                shadow_count,
                groups: bytes(&groups),
                clusters: bytes(&packed),
                vertices: bytes(&vertices),
                indices: bytes(&indices),
            },
            voxel_replacements: replacements.iter().flatten().count().try_into()?,
            bones,
            mesh: source_mesh.clone(),
            meshlets: source_meshlets.clone(),
            dag: source_dag.clone(),
            plant,
            mixed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn canopy_stores_voxel_replacements_at_multiple_dag_levels() -> Result<()> {
        let world = crate::experiments::vegetation::scene()?;
        let canopy = world
            .entities
            .values()
            .find(|e| e.name == "Spruce canopy")
            .unwrap();
        let asset = MeshAsset::from_entity(canopy).unwrap();
        let packed = Packed::build(&asset, true)?;
        let groups: &[Group] = bytemuck::cast_slice(&packed.streams.groups);
        let clusters: &[Cluster] = bytemuck::cast_slice(&packed.streams.clusters);
        let mut voxel_levels = BTreeSet::new();
        let mut parent_counts = std::collections::BTreeMap::new();
        let mut replaced = BTreeSet::new();
        let mut triangles = BTreeSet::new();
        for cluster in &clusters[..packed.streams.count as usize] {
            let [owner, child, level, kind] = cluster.meta;
            if child != NONE {
                assert!(child < owner);
                assert!(groups[child as usize].error <= groups[owner as usize].error);
            }
            if kind & 1 != 0 {
                assert_ne!(child, NONE);
                assert!((1..=128).contains(&(kind >> 1)));
                let pages = &clusters[cluster.span[2] as usize..][..cluster.span[1] as usize];
                assert!(pages.iter().all(|p| p.span[1] <= 64
                    && p.span[3] <= 64
                    && p.meta[..3] == cluster.meta[..3]));
                assert_eq!(
                    pages.iter().map(|p| p.span[3]).sum::<u32>(),
                    cluster.span[3]
                );
                voxel_levels.insert(level);
                replaced.insert(child);
                *parent_counts.entry(child).or_insert(0usize) += 1;
            } else {
                triangles.insert(child);
            }
        }
        let mut mixed = 0;
        let mut voxel_children = 0;
        for &child in &replaced {
            let group = &groups[child as usize];
            let cut = &clusters[group.first_cluster as usize..][..group.cluster_count as usize];
            let count = cut.iter().filter(|c| c.meta[3] & 1 != 0).count();
            voxel_children += usize::from(count != 0);
            mixed += usize::from(count != 0 && count != cut.len());
        }
        eprintln!(
            "Spruce: {} voxel replacements, {} from voxel children, {} from mixed children, {} emitted parents, {} groups with multiple parents",
            replaced.len(),
            voxel_children,
            mixed,
            parent_counts.values().sum::<usize>(),
            parent_counts.values().filter(|&&n| n > 1).count()
        );
        assert!(mixed > 0 && voxel_children >= mixed);
        assert!(voxel_levels.len() >= 2);
        assert!(triangles.contains(&NONE));
        assert!(replaced.is_disjoint(&triangles));
        assert_eq!(packed.voxel_replacements as usize, replaced.len());
        let plain = Packed::build(&asset, false)?;
        assert_eq!(plain.voxel_replacements, 0);
        Ok(())
    }

    #[test]
    fn mixed_frontier_crosses_saved_boundaries_and_uses_each_leaf_once() -> Result<()> {
        let world = crate::experiments::vegetation::scene()?;
        let canopy = world
            .entities
            .values()
            .find(|e| e.name == "Spruce canopy")
            .unwrap();
        let mut asset = MeshAsset::from_entity(canopy).unwrap();
        let slots = asset.meshlets.as_ref().unwrap();
        let stored = asset.hierarchy.as_ref().unwrap();
        let size = asset.plant.as_ref().unwrap().voxel_cell_size.unwrap();
        let dag = voxels::Hierarchy::build(&asset.mesh, slots, stored, size)?;
        let mut source_owner = HashMap::new();
        let mut leaf_ids = vec![];
        for (owner, g) in stored.iter().enumerate() {
            for &id in &g.meshlets {
                let c = &slots[id as usize];
                if c.refined_group.is_none() {
                    leaf_ids.push(id);
                    for &f in &c.triangles {
                        assert!(source_owner.insert(f, owner).is_none());
                    }
                }
            }
        }
        let mut seen = vec![0; dag.meshlets.len()];
        let mut leaf_faces = BTreeSet::new();
        let mut crossed = 0;
        for group in &dag.nodes {
            let mut owners = BTreeSet::new();
            for &id in &group.meshlets {
                seen[id as usize] += 1;
                let c = &dag.meshlets[id as usize];
                if c.refined_group.is_none() {
                    for &f in &c.triangles {
                        owners.insert(source_owner[&f]);
                        assert!(leaf_faces.insert(f));
                    }
                }
            }
            crossed += usize::from(owners.len() > 1);
        }
        assert_eq!(leaf_faces.len(), source_owner.len());
        assert!(seen.iter().all(|&n| n == 1));
        // The input and runtime use the same triangle partitioner. Their first all-triangle level
        // may coincide. Compare descendant sets after actual parents have re-entered grouping.
        let ancestry = |nodes: &[ClusterNode], slots: &[Meshlet]| {
            let mut sets = Vec::<BTreeSet<u32>>::new();
            for n in nodes {
                let mut represented = BTreeSet::new();
                for &id in &n.meshlets {
                    let m = &slots[id as usize];
                    if let Some(child) = m.refined_group {
                        represented.extend(&sets[child as usize]);
                    } else {
                        represented.insert(m.triangles[0]);
                    }
                }
                sets.push(represented);
            }
            sets
        };
        let archived = ancestry(stored, slots).into_iter().collect::<BTreeSet<_>>();
        let regenerated = ancestry(&dag.nodes, &dag.meshlets);
        let changed = regenerated
            .iter()
            .filter(|set| !archived.contains(*set))
            .count();
        assert!(
            changed > 0,
            "regrouped ancestors should differ after the first reductions"
        );
        eprintln!(
            "Spruce regroup: {crossed} first-level boundary crossings, {changed} new ancestry sets, {} groups, {} roots, {} source leaf triangles",
            dag.nodes.len(),
            dag.nodes.iter().filter(|n| n.error == f32::MAX).count(),
            leaf_faces.len()
        );
        let a = Packed::build(&asset, true)?.streams;
        // A valid leaf-only archive has different group membership and traversal order.
        // An unreferenced meshlet in the archive must not enter the ray scene or the rendered cut.
        let orphan = slots[leaf_ids[0] as usize].clone();
        Arc::make_mut(asset.meshlets.as_mut().unwrap()).push(orphan);
        leaf_ids.reverse();
        let mut alternate = vec![
            ClusterNode {
                meshlets: vec![],
                center: [0.; 3],
                radius: 100.,
                error: f32::MAX
            };
            7
        ];
        for (i, id) in leaf_ids.into_iter().enumerate() {
            alternate[i % 7].meshlets.push(id);
        }
        asset.hierarchy = Some(Arc::new(alternate));
        let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
        let b = pool.install(|| Packed::build(&asset, true))?.streams;
        for (x, y) in [
            (&a.tree, &b.tree),
            (&a.groups, &b.groups),
            (&a.clusters, &b.clusters),
            (&a.indices, &b.indices),
            (&a.vertices, &b.vertices),
        ] {
            assert!(
                x == y,
                "mixed DAG changed with old partition or worker count"
            );
        }
        Ok(())
    }

    #[test]
    fn each_leaf_owns_one_complete_group() -> Result<()> {
        let world = crate::experiments::clusters::terrain()?;
        let entity = world
            .entities
            .values()
            .find(|e| e.hierarchy.is_some())
            .unwrap();
        let packed = Packed::build(&MeshAsset::from_entity(entity).unwrap(), true)?.streams;
        let nodes: &[Node] = bytemuck::cast_slice(&packed.tree);
        let groups: &[Group] = bytemuck::cast_slice(&packed.groups);
        let clusters: &[Cluster] = bytemuck::cast_slice(&packed.clusters);
        let mut group_hits = vec![0; groups.len()];
        let mut cluster_hits = vec![0; clusters.len()];
        let mut frontier = vec![0usize];
        let mut widths = vec![];
        while !frontier.is_empty() {
            let jobs = frontier
                .iter()
                .filter(|&&id| id == 0 || nodes[id].children != 0)
                .count();
            if jobs > 0 {
                widths.push(jobs as u32);
            }
            let mut next = Vec::new();
            for id in frontier {
                let node = nodes[id];
                if node.children > 0 {
                    assert!(node.children <= 8);
                    assert_eq!(node.group, NONE);
                    next.extend((node.first..node.first + node.children).map(|i| i as usize));
                    continue;
                }
                let group = groups[node.group as usize];
                group_hits[node.group as usize] += 1;
                assert_eq!(node.projection, group.sphere);
                assert_eq!(node.max_error, group.error);
                for i in group.first_cluster..group.first_cluster + group.cluster_count {
                    assert_eq!(clusters[i as usize].meta[0], node.group);
                    cluster_hits[i as usize] += 1;
                }
            }
            frontier = next;
        }
        assert_eq!(widths, packed.level_widths);
        assert!(group_hits.iter().all(|&n| n == 1));
        assert!(cluster_hits.iter().all(|&n| n == 1));
        Ok(())
    }
}
