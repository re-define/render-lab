//! Partition the physical clusters produced by one reduction level.
use super::*;
use meshoptimizer_rs::{Positions, Workspace, partition_clusters};

const MIN: usize = 8;
const MAX: usize = 32;

#[derive(Clone, Copy, Debug)]
pub(super) struct Member {
    pub slot: u32,
    pub bounds: [f32; 4],
    pub lod: [f32; 4],
    // One voxel member carries every physical sibling from its generating group.
    pub physical: usize,
    pub voxel: bool,
}

pub(super) fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let ca = Vec3::from_slice(&a);
    let cb = Vec3::from_slice(&b);
    let d = cb - ca;
    let dist = d.length();
    if a[3] >= dist + b[3] {
        return a;
    }
    if b[3] >= dist + a[3] {
        return b;
    }
    let radius = (dist + a[3] + b[3]) * 0.5;
    (ca + d * ((radius - a[3]) / dist)).extend(radius).into()
}

pub(super) fn bounds(group: &[Member]) -> [f32; 4] {
    group
        .iter()
        .map(|m| m.bounds)
        .reduce(union)
        .expect("nonempty group")
}

struct Bundle {
    members: Vec<Member>,
    sphere: [f32; 4],
    count: usize,
}
fn morton(p: [f32; 4], lo: Vec3, extent: f32) -> u32 {
    let v = ((Vec3::from_slice(&p) - lo) / extent * 1023.)
        .clamp(Vec3::ZERO, Vec3::splat(1023.))
        .as_uvec3();
    let mut code = 0;
    for bit in 0..10 {
        for axis in 0..3 {
            code |= ((v[axis] >> bit) & 1) << (bit * 3 + axis);
        }
    }
    code
}

fn spatial(members: Vec<Member>) -> Vec<Vec<Member>> {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = -lo;
    for m in &members {
        let p = Vec3::from_slice(&m.bounds);
        lo = lo.min(p);
        hi = hi.max(p);
    }
    let extent = (hi - lo).max_element().max(f32::EPSILON);
    let mut sets: Vec<_> = members
        .into_iter()
        .map(|m| Bundle {
            members: vec![m],
            sphere: m.bounds,
            count: m.physical,
        })
        .collect();
    sets.sort_by_key(|g| (morton(g.sphere, lo, extent), g.members[0].slot));
    loop {
        let nearest: Vec<_> = sets
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let start = i.saturating_sub(16);
                let end = (i + 17).min(sets.len());
                (start..end)
                    .filter(|&j| {
                        i != j
                            && (a.count < MIN || sets[j].count < MIN)
                            && a.count + sets[j].count <= MAX
                    })
                    .min_by(|&j, &k| {
                        union(a.sphere, sets[j].sphere)[3]
                            .total_cmp(&union(a.sphere, sets[k].sphere)[3])
                            .then(j.cmp(&k))
                    })
            })
            .collect();
        let mut merged = false;
        for (i, &other) in nearest.iter().enumerate() {
            if let Some(j) = other
                && i < j
                && nearest[j] == Some(i)
            {
                let (left, right) = sets.split_at_mut(j);
                let a = &mut left[i];
                let b = &mut right[0];
                a.sphere = union(a.sphere, b.sphere);
                a.count += b.count;
                a.members.append(&mut b.members);
                merged = true;
            }
        }
        if !merged {
            break;
        }
        sets.retain(|s| !s.members.is_empty());
    }
    sets.into_iter().map(|s| s.members).collect()
}

fn triangles(mesh: &Mesh, slots: &[Meshlet], members: Vec<Member>) -> Result<Vec<Vec<Member>>> {
    if members.len() <= MAX {
        return Ok(if members.is_empty() {
            vec![]
        } else {
            vec![members]
        });
    }
    let mut indices = Vec::new();
    let mut counts = Vec::new();
    let mut welded = HashMap::new();
    let mut xyz = Vec::new();
    for m in &members {
        let begin = indices.len();
        for &f in &slots[m.slot as usize].triangles {
            for v in triangle(mesh, f) {
                let point = mesh.positions[v];
                let key = levels::position_key(point);
                let id = *welded.entry(key).or_insert_with(|| {
                    let i = xyz.len() as u32;
                    xyz.push(point);
                    i
                });
                indices.push(id);
            }
        }
        counts.push((indices.len() - begin).try_into()?);
    }
    let p = partition_clusters(
        &indices,
        &counts,
        xyz.len(),
        Some(Positions::from_packed(&xyz)),
        16,
        &mut Workspace::default(),
    )?;
    let mut groups = vec![Vec::new(); p.count];
    for (m, k) in members.into_iter().zip(p.assignments) {
        groups[k as usize].push(m);
    }
    Ok(groups
        .into_iter()
        .flat_map(|g| g.chunks(MAX).map(<[_]>::to_vec).collect::<Vec<_>>())
        .collect())
}

pub(super) fn partition(
    mesh: &Mesh,
    slots: &[Meshlet],
    members: Vec<Member>,
) -> Result<Vec<Vec<Member>>> {
    if members.iter().map(|m| m.physical).sum::<usize>() <= MAX {
        return Ok(vec![members]);
    }
    let (mut vox, mut tri): (Vec<_>, Vec<_>) = members.into_iter().partition(|m| m.voxel);
    if tri.len() == 1 {
        vox.append(&mut tri);
        return Ok(spatial(vox));
    }
    let mut groups = triangles(mesh, slots, tri)?;
    if vox.len() == 1 && vox[0].physical == 1 {
        let m = vox[0];
        if let Some(g) = groups
            .iter_mut()
            .filter(|g| g.len() < MAX)
            .min_by(|a, b| union(bounds(a), m.bounds)[3].total_cmp(&union(bounds(b), m.bounds)[3]))
        {
            g.push(m);
        } else if let Some(g) = groups
            .iter_mut()
            .min_by(|a, b| union(bounds(a), m.bounds)[3].total_cmp(&union(bounds(b), m.bounds)[3]))
        {
            // All triangle groups are full. Keep the singleton with its nearest triangle.
            let i = (0..g.len())
                .min_by(|&i, &j| {
                    union(g[i].bounds, m.bounds)[3].total_cmp(&union(g[j].bounds, m.bounds)[3])
                })
                .unwrap();
            let neighbor = g.remove(i);
            groups.push(vec![m, neighbor]);
        }
        return Ok(groups);
    }
    if !vox.is_empty() {
        groups.extend(spatial(vox));
    }
    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voxel_families_stay_together_and_merge_with_nearby_families() {
        let members: Vec<_> = (0..18)
            .map(|id| {
                let sphere = [(id % 9) as f32 + 1000. * (id / 9) as f32, 0., 0., 0.5];
                Member {
                    slot: id,
                    bounds: sphere,
                    lod: sphere,
                    physical: 2,
                    voxel: true,
                }
            })
            .collect();
        let sets = spatial(members);
        let mut seen = Vec::new();
        for group in sets {
            let count = group.iter().map(|m| m.physical).sum::<usize>();
            assert!((MIN..=MAX).contains(&count));
            assert!(group.iter().all(|m| m.slot / 9 == group[0].slot / 9));
            seen.extend(group.iter().map(|m| m.slot));
        }
        seen.sort_unstable();
        assert_eq!(seen, (0..18).collect::<Vec<_>>());
    }
}
