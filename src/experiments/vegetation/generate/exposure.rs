//! Bake local sky visibility from leaf area. The UV carries it through simplification and voxelization.
use super::{Mesh, Vec3};
use glam::IVec3;

const GRID: i32 = 16;
const CELLS: usize = (GRID * GRID * GRID) as usize;

fn index(p: IVec3) -> usize {
    (p.x + GRID * (p.y + GRID * p.z)) as usize
}
fn inside(p: IVec3) -> bool {
    p.min_element() >= 0 && p.max_element() < GRID
}

pub(super) fn bake(mesh: &mut Mesh) {
    let (min, max) = mesh.positions.iter().copied().map(Vec3::from).fold(
        (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
        |(lo, hi), p| (lo.min(p), hi.max(p)),
    );
    let cell = (max - min).max_element() / (GRID - 2) as f32;
    let origin = (max + min) * 0.5 - Vec3::splat(cell * GRID as f32 * 0.5);
    let grid = |p: Vec3| (p - origin) / cell;
    let mut area = vec![0.; CELLS];
    for tri in mesh.indices.chunks_exact(3) {
        let [a, b, c] = [0, 1, 2].map(|k| Vec3::from(mesh.positions[tri[k] as usize]));
        let p = grid((a + b + c) / 3.).floor().as_ivec3();
        area[index(p)] += 0.5 * (b - a).cross(c - a).length() / (cell * cell);
    }
    let mut light = vec![1.; CELLS];
    for z in 0..GRID {
        for y in 0..GRID {
            for x in 0..GRID {
                let start = IVec3::new(x, y, z);
                let mut sky = 0.;
                for step in [
                    IVec3::Y,
                    IVec3::new(1, 1, 0),
                    IVec3::new(-1, 1, 0),
                    IVec3::new(0, 1, 1),
                    IVec3::new(0, 1, -1),
                ] {
                    let mut depth = 0.26 * area[index(start)];
                    let mut p = start + step;
                    while inside(p) {
                        depth += area[index(p)];
                        p += step;
                    }
                    sky += (-0.52 * depth).exp();
                }
                light[index(start)] = sky * 0.20;
            }
        }
    }
    let center = (min + max) * 0.5;
    let radius = ((max - min) * 0.5).max(Vec3::splat(cell));
    for ((uv, normal), &p) in mesh
        .uv
        .iter_mut()
        .zip(&mut mesh.normals)
        .zip(&mesh.positions)
    {
        let g = grid(Vec3::from(p)) - Vec3::splat(0.5);
        let lo = g.floor().as_ivec3();
        let f = g - lo.as_vec3();
        let mut exposure = 0.;
        let mut gradient = Vec3::ZERO;
        for z in 0..2 {
            for y in 0..2 {
                for x in 0..2 {
                    let off = IVec3::new(x, y, z);
                    let weights = Vec3::ONE - (off.as_vec3() - f).abs();
                    let sample =
                        light[index((lo + off).clamp(IVec3::ZERO, IVec3::splat(GRID - 1)))];
                    exposure += weights.element_product() * sample;
                    // Analytic derivative of the same trilinear sky field, in cell units.
                    gradient += (2. * off.as_vec3() - Vec3::ONE)
                        * sample
                        * Vec3::new(
                            weights.y * weights.z,
                            weights.x * weights.z,
                            weights.x * weights.y,
                        );
                }
            }
        }
        // Grass uses [0,1]. Tree UV.x above two carries local sky exposure and a per-shoot tint.
        uv[0] = 2. + 0.86 * exposure + 0.14 * uv[0];
        // Orient the two-sided leaf frame toward its local canopy opening. The radial
        // term gives a continuous fallback in uniformly exposed or occluded cells.
        let radial = ((Vec3::from(p) - center) / radius / radius).normalize_or_zero();
        let crown = (gradient + 0.12 * radial + 0.055 * Vec3::Y).normalize_or(Vec3::Y);
        let leaf = Vec3::from(*normal);
        let leaf = if leaf.dot(crown) < 0. { -leaf } else { leaf };
        *normal = leaf.lerp(crown, 0.58).normalize_or(crown).into();
    }
}
