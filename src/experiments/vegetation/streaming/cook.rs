//! Cook the same seeded oak and triangle cluster hierarchy as the forest preset.
//! Each non-root group publishes as a unit. Multiple coarser meshlets may reference that unit.
use super::*;
use crate::scene::Mesh;
use glam::{Vec2, Vec3};
use std::{collections::BTreeSet, io::Write};

const NONE: u32 = u32::MAX;
pub(super) const TILES: usize = 16;
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Cooked {
    // Serialized words preserve the simplifier's sentinel floats. The shader struct is in forest_stream.glsl.
    pub groups: Vec<[u32; 12]>,
    pub chunks: Vec<[u32; 4]>,
    pub parents: Vec<u32>,
    pub order: Vec<u32>,
    pub layers: Vec<(u32, u32)>,
    pub roots: Vec<u32>,
}
impl Cooked {
    /// Check every range referenced by the GPU and the topological ordering before publishing a frame.
    fn validate(&self, package: &Package) -> Result<()> {
        let n = self.groups.len();
        ensure!(
            n > 0 && self.order.len() == n && package.parts.len() > TILES,
            "incomplete forest stream index"
        );
        for part in &package.parts[..TILES] {
            ensure!(
                part.root.len() >= 8 * 8 * 4
                    && part.levels.len() == 1
                    && part.levels[0].bytes == 128 * 128 * 4,
                "invalid bark tile"
            );
        }
        let mut depth = vec![None; n];
        let mut next = 0usize;
        for (layer, &(start, count)) in self.layers.iter().enumerate() {
            ensure!(start as usize == next && count > 0, "invalid canopy layer");
            next = next
                .checked_add(count as usize)
                .context("canopy layer overflow")?;
            for &g in self
                .order
                .get(start as usize..next)
                .context("invalid layer range")?
            {
                let value = depth.get_mut(g as usize).context("invalid layer group")?;
                ensure!(value.replace(layer).is_none(), "duplicate canopy group");
            }
        }
        ensure!(next == n, "missing canopy groups");
        let mut incoming = vec![BTreeSet::new(); n];
        for &[owner, child, first, faces] in &self.chunks {
            ensure!((1..=32).contains(&faces), "invalid oak mesh dispatch");
            let end = first as u64 + u64::from(faces) * 12;
            if owner == NONE {
                ensure!(
                    child == NONE && end <= self.roots.len() as u64,
                    "invalid trunk payload"
                );
                continue;
            }
            let group = self
                .groups
                .get(owner as usize)
                .context("invalid chunk owner")?;
            if group[5] == NONE {
                ensure!(
                    u64::from(group[6]) + end <= self.roots.len() as u64,
                    "invalid pinned canopy chunk"
                );
            } else {
                let part = package
                    .parts
                    .get(group[5] as usize)
                    .context("unknown streamed canopy group")?;
                ensure!(
                    group[5] as usize >= TILES
                        && part.levels.len() == 1
                        && end * 4 <= part.levels[0].bytes as u64,
                    "invalid paged canopy chunk"
                );
            }
            if child != NONE {
                ensure!(
                    (child as usize) < n && depth[child as usize] > depth[owner as usize],
                    "unordered canopy edge"
                );
                incoming[child as usize].insert(owner);
            }
        }
        for (g, parents) in self.groups.iter().zip(incoming) {
            let start = g[7] as usize;
            let stored = self
                .parents
                .get(start..start + g[8] as usize)
                .context("invalid parent range")?;
            ensure!(
                stored.iter().copied().eq(parents.iter().copied())
                    && (g[5] == NONE) == parents.is_empty(),
                "incomplete canopy replacement group"
            );
        }
        Ok(())
    }
}
fn half2(x: f32, y: f32) -> u32 {
    u32::from(half::f16::from_f32(x).to_bits())
        | (u32::from(half::f16::from_f32(y).to_bits()) << 16)
}
/// Four words per unindexed vertex keep groups independently decodable.
fn vertices(out: &mut Vec<u32>, mesh: &Mesh, face: u32) {
    for &idx in &mesh.indices[face as usize * 3..][..3] {
        let i = idx as usize;
        let [x, y, z] = mesh.positions[i];
        let [u, v] = mesh.uv[i];
        let n = mesh.normals[i].map(|v| ((v * 0.5 + 0.5).clamp(0., 1.) * 255.).round() as u32);
        out.extend([
            half2(x, y),
            half2(z, 0.),
            half2(u, v),
            n[0] | (n[1] << 8) | (n[2] << 16),
        ]);
    }
}
fn fract(x: f32) -> f32 {
    x - x.floor()
}
fn noise(p: Vec2) -> f32 {
    let cell = p.floor();
    let f = p - cell;
    let f = f * f * (3. - 2. * f);
    let hash = |v: Vec2| {
        let mut q = Vec3::new(v.x, v.y, v.x).map(|k| fract(k * 0.1031));
        q += Vec3::splat(q.dot(Vec3::new(q.y, q.z, q.x) + Vec3::splat(33.33)));
        fract((q.x + q.y) * q.z)
    };
    let a = hash(cell);
    let b = hash(cell + Vec2::X);
    let c = hash(cell + Vec2::Y);
    let d = hash(cell + Vec2::ONE);
    (a + (b - a) * f.x) * (1. - f.y) + (c + (d - c) * f.x) * f.y
}
// Oak branch UVs measure height in metres. Bake the non-birch branch of vegetation_material.comp.
fn bark_tiles() -> Vec<Vec<Vec<u8>>> {
    (0..TILES)
        .map(|tile| {
            let mut fine = Vec::with_capacity(65536);
            let mut sums = [[0u32; 3]; 64];
            for y in 0..128 {
                for x in 0..128 {
                    let uv = Vec2::new(
                        ((tile % 4 * 128 + x) as f32 + 0.5) / 512.,
                        ((tile / 4 * 128 + y) as f32 + 0.5) / 512. * 8.,
                    );
                    let grain = uv * Vec2::new(22., 1.48);
                    let ridges = noise(grain + Vec2::X * noise(grain * 0.23) * 3.2);
                    let t = ((ridges - 0.24) / 0.41).clamp(0., 1.);
                    let detail = noise(uv * Vec2::new(15., 9.));
                    let rgb = Vec3::new(0.33, 0.38, 0.42)
                        .lerp(Vec3::new(1.06, 0.98, 0.85), t * t * (3. - 2. * t))
                        * Vec3::new(0.183, 0.139, 0.097)
                        * (0.77 + 0.34 * detail);
                    let v = rgb.to_array().map(|f| (f * 255.).round() as u8);
                    for (sum, c) in sums[(y / 16) * 8 + x / 16].iter_mut().zip(v) {
                        *sum += u32::from(c)
                    }
                    fine.extend([v[0], v[1], v[2], 255]);
                }
            }
            let root = sums
                .into_iter()
                .flat_map(|s| {
                    [
                        (s[0] / 256) as u8,
                        (s[1] / 256) as u8,
                        (s[2] / 256) as u8,
                        255,
                    ]
                })
                .collect();
            vec![root, fine]
        })
        .collect()
}
fn emit_chunks(chunks: &mut Vec<[u32; 4]>, owner: u32, child: u32, first: u32, n: usize) {
    for start in (0..n).step_by(32) {
        chunks.push([
            owner,
            child,
            first + 12 * start as u32,
            (n - start).min(32) as u32,
        ]);
    }
}
fn build(path: &std::path::Path) -> Result<Cooked> {
    use super::super::generate::{Species, rig, tree};
    let (wood, leaves, bones) = tree(0x71b9 + 3 * 157, Species::Oak);
    let plant = rig(&leaves, &bones, Some(0.62));
    let mut w = World::default();
    let id = super::super::mesh(&mut w, "Oak canopy", leaves, Some(plant));
    crate::experiments::clusters::prepare(&mut w)?;
    let e = &w.entities[&id];
    let mesh = e.mesh.as_deref().unwrap();
    let dag = e.hierarchy.as_deref().unwrap();
    let clusters = e.meshlets.as_deref().unwrap();
    let mut parents = vec![BTreeSet::new(); dag.len()];
    for (id, node) in dag.iter().enumerate() {
        for &m in &node.meshlets {
            if let Some(child) = clusters[m as usize].refined_group {
                anyhow::ensure!((child as usize) < id, "forest DAG is not bottom-up");
                parents[child as usize].insert(id as u32);
            }
        }
    }
    let mut depth = vec![0; dag.len()];
    for id in (0..dag.len()).rev() {
        for &p in &parents[id] {
            depth[id] = depth[id].max(depth[p as usize] + 1);
        }
    }
    let mut result = Cooked {
        groups: vec![],
        chunks: vec![],
        parents: vec![],
        order: vec![],
        layers: vec![],
        roots: vec![],
    };
    let mut parts = bark_tiles();
    // The trunk has a fixed representation. All canopy groups and bark tiles share one package.
    let trunk_faces = wood.indices.len() / 3;
    for i in 0..trunk_faces {
        vertices(&mut result.roots, &wood, i as u32);
    }
    emit_chunks(&mut result.chunks, NONE, NONE, 0, trunk_faces);
    for (id, node) in dag.iter().enumerate() {
        let pinned = parents[id].is_empty();
        let mut words = Vec::new();
        let unit = if pinned { NONE } else { parts.len() as u32 };
        for &c in &node.meshlets {
            let meshlet = &clusters[c as usize];
            emit_chunks(
                &mut result.chunks,
                id as u32,
                meshlet.refined_group.unwrap_or(NONE),
                words.len() as u32,
                meshlet.triangles.len(),
            );
            for &face in &meshlet.triangles {
                vertices(&mut words, mesh, face);
            }
        }
        let root = result.roots.len() as u32;
        if pinned {
            result.roots.extend(words);
        } else {
            anyhow::ensure!(!words.is_empty(), "empty canopy refinement group");
            parts.push(vec![vec![0; 4], bytemuck::cast_slice(&words).to_vec()]);
        }
        result.groups.push([
            node.center[0].to_bits(),
            node.center[1].to_bits(),
            node.center[2].to_bits(),
            node.radius.to_bits(),
            node.error.to_bits(),
            unit,
            root,
            result.parents.len() as u32,
            parents[id].len() as u32,
            0,
            0,
            0,
        ]);
        result.parents.extend(parents[id].iter().copied());
    }
    for level in 0..=depth.iter().copied().max().unwrap_or(0) {
        let start = result.order.len() as u32;
        result.order.extend(
            (0..dag.len())
                .filter(|&i| depth[i] == level)
                .map(|i| i as u32),
        );
        result
            .layers
            .push((start, result.order.len() as u32 - start));
    }
    anyhow::ensure!(
        result.groups.iter().any(|g| g[5] != NONE),
        "canopy needs child groups"
    );
    Package::cook(path, parts)?;
    Ok(result)
}
pub(super) fn open() -> Result<(Cooked, Package)> {
    let dir = crate::assets::cache_dir().join("forest-stream-oak-v1");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("oak.pages");
    let meta = dir.join("oak.json");
    if let (Ok(file), Ok(p)) = (std::fs::File::open(&meta), Package::open(&path)) {
        let cooked: Cooked = serde_json::from_reader(std::io::BufReader::new(file))?;
        cooked.validate(&p)?;
        return Ok((cooked, p));
    }
    let cooked = build(&path)?;
    let mut f = tempfile::NamedTempFile::new_in(&dir)?;
    let mut writer = std::io::BufWriter::new(f.as_file_mut());
    serde_json::to_writer(&mut writer, &cooked)?;
    writer.flush()?;
    drop(writer);
    f.persist(&meta)?;
    let package = Package::open(&path)?;
    cooked.validate(&package)?;
    Ok((cooked, package))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oak_directory_keeps_all_parents_and_rejects_an_incomplete_cut() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("test.pages");
        let mut data = build(&path)?;
        let package = Package::open(&path)?;
        data.validate(&package)?;
        ensure!(package.parts.len() > TILES && data.layers.len() > 1);
        let child = data
            .groups
            .iter_mut()
            .find(|g| g[8] > 0)
            .context("missing canopy parents")?;
        child[8] -= 1;
        ensure!(
            data.validate(&package).is_err(),
            "directory accepted an incomplete set of incoming edges"
        );
        Ok(())
    }
}
