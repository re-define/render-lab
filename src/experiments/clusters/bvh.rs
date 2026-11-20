//! An eight-way spatial BVH whose leaves reference complete cluster groups.
//! Geometry bounds answer visibility queries; nested error spheres answer LOD queries.
use crate::scene::*;
use anyhow::{Context, Result, ensure};
use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use meshoptimizer_rs::clusterlod::{self as cl, cut};
use std::{collections::HashMap, sync::Arc};

const NONE: u32 = u32::MAX;

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

pub(super) struct Packed {
    pub tree: Arc<[u8]>,
    pub groups: Arc<[u8]>,
    pub clusters: Arc<[u8]>,
    pub vertices: Arc<[u8]>,
    pub indices: Arc<[u8]>,
    pub level_widths: Vec<u32>,
    pub count: u32,
    // Holding the source Arcs makes cache identity safe across scene edits.
    mesh: Arc<Mesh>,
    meshlets: Arc<Vec<Meshlet>>,
    dag: Arc<Vec<ClusterNode>>,
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

/// Shared by preprocessing and the lazy upgrade of previously saved DAGs.
pub(super) fn repaired(dag: &[ClusterNode], clusters: &[Meshlet]) -> Result<Vec<cl::FlatDagGroup>> {
    let mut flat: Vec<cl::FlatDagGroup> = vec![];
    let mut entries = vec![];
    for (id, node) in dag.iter().enumerate() {
        let mut g = cl::FlatDagGroup::default();
        g.center = node.center;
        g.radius = node.radius;
        g.error = node.error;
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
    pub fn matches(&self, e: &Entity) -> bool {
        Arc::ptr_eq(&self.mesh, e.mesh.as_ref().unwrap())
            && Arc::ptr_eq(&self.meshlets, e.meshlets.as_ref().unwrap())
            && Arc::ptr_eq(&self.dag, e.hierarchy.as_ref().unwrap())
    }
    pub fn build(e: &Entity) -> Result<Self> {
        let mesh = e.mesh.as_ref().context("missing geometry")?;
        let meshlets = e.meshlets.as_ref().context("missing meshlets")?;
        let dag = e.hierarchy.as_ref().context("missing DAG")?;
        let mut groups = repaired(dag, meshlets)?
            .iter()
            .map(|g| Group {
                sphere: [g.center[0], g.center[1], g.center[2], g.radius],
                error: g.error,
                level: g.depth as u32,
                first_cluster: 0,
                cluster_count: 0,
            })
            .collect::<Vec<_>>();
        let vertices = mesh
            .positions
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let n = mesh.normals.get(i).copied().unwrap_or_default();
                let uv = mesh.uv.get(i).copied().unwrap_or_default();
                [p[0], p[1], p[2], uv[0], n[0], n[1], n[2], uv[1]]
            })
            .collect::<Vec<_>>();
        let mut indices = vec![];
        let mut packed = vec![];
        let mut leaves = vec![];
        for (owner, g) in dag.iter().enumerate() {
            let first = packed.len();
            for &id in &g.meshlets {
                let c = &meshlets[id as usize];
                let level = c.refined_group.map_or(0, |i| groups[i as usize].level + 1);
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
                    packed.push(Cluster {
                        span: [start, nv, start + nv, nt],
                        sphere: sphere(lo, hi),
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
        ensure!(
            count <= 1 << 26,
            "cluster ID exceeds visibility buffer encoding"
        );
        let mut nodes = vec![Node::default()];
        let mut level_widths = vec![];
        spatial_node(&mut leaves, &groups, &mut nodes, 0, 0, &mut level_widths);
        Ok(Self {
            tree: bytes(&nodes),
            level_widths,
            count,
            groups: bytes(&groups),
            clusters: bytes(&packed),
            vertices: bytes(&vertices),
            indices: bytes(&indices),
            mesh: mesh.clone(),
            meshlets: meshlets.clone(),
            dag: dag.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_leaf_owns_one_complete_group() -> Result<()> {
        let world = crate::experiments::clusters::terrain()?;
        let entity = world
            .entities
            .values()
            .find(|e| e.hierarchy.is_some())
            .unwrap();
        let packed = Packed::build(entity)?;
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
