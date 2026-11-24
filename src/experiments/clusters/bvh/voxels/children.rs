//! The current DAG cut supplies coverage candidates; original leaf surfaces supply ray hits.
use super::*;

pub(super) struct ChildCut<'a> {
    pub triangles: Vec<u32>,
    pub voxels: Vec<&'a Replacement>,
    pub vertices: usize,
    pub area: f32,
    pub error_floor: f32,
}

impl<'a> ChildCut<'a> {
    pub fn new(
        mesh: &Mesh,
        group: &ClusterNode,
        meshlets: &[Meshlet],
        choices: &'a [Option<Replacement>],
        errors: &[f32],
    ) -> Self {
        let mut cut = Self {
            triangles: Vec::new(),
            voxels: Vec::new(),
            vertices: 0,
            area: 0.,
            error_floor: 0.,
        };
        let mut seen = BTreeSet::new();
        for &id in &group.meshlets {
            let cluster = &meshlets[id as usize];
            if let Some(child) = cluster.refined_group {
                let index = child as usize;
                cut.error_floor = cut.error_floor.max(errors[index]);
                if let Some(voxel) = &choices[index] {
                    // Multiple meshlets can refer to a single whole-group replacement.
                    if seen.insert(child) {
                        cut.vertices += voxel.layout.cells.len();
                        cut.area += voxel.source_area;
                        cut.voxels.push(voxel);
                    }
                    continue;
                }
            }
            cut.vertices += cluster
                .triangles
                .iter()
                .flat_map(|&f| triangle(mesh, f))
                .collect::<BTreeSet<_>>()
                .len();
            for &f in &cluster.triangles {
                let [a, b, c] = positions(mesh, f);
                cut.area += (b - a).cross(c - a).length() * 0.5;
                cut.triangles.push(f);
            }
        }
        cut
    }
}

struct Patch<'a> {
    lo: Vec3,
    hi: Vec3,
    faces: &'a [u32],
}

pub(super) struct SourceSurface<'a> {
    patches: Vec<Patch<'a>>,
}

impl<'a> SourceSurface<'a> {
    pub fn new(mesh: &Mesh, meshlets: impl IntoIterator<Item = &'a Meshlet>) -> Self {
        let patches = meshlets
            .into_iter()
            .filter(|c| c.refined_group.is_none())
            .map(|c| {
                let (lo, hi) = c.triangles.iter().flat_map(|&f| positions(mesh, f)).fold(
                    (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
                    |(lo, hi), p| (lo.min(p), hi.max(p)),
                );
                Patch {
                    lo,
                    hi,
                    faces: &c.triangles,
                }
            })
            .collect();
        Self { patches }
    }

    pub fn faces_in(&self, lo: Vec3, hi: Vec3) -> impl Iterator<Item = u32> + '_ {
        self.patches
            .iter()
            .filter(move |p| p.hi.cmpge(lo).all() && p.lo.cmple(hi).all())
            .flat_map(|p| p.faces.iter().copied())
    }
}
