//! Fine foliage geometry attached to terminal shoots.
use super::super::vertex;
use super::{Branch, Mesh, Random, Species, Vec3};

// A folded blade with a raised midrib. Successive pairs describe the outline along its length.
fn blade(
    mesh: &mut Mesh,
    root: Vec3,
    axis: Vec3,
    side: Vec3,
    profile: &[(f32, f32)],
    joint: u16,
    tint: f32,
) {
    let normal = side.cross(axis).normalize();
    let base = mesh.positions.len() as u32;
    let emit = |m: &mut Mesh, p, n, t| vertex(m, p, n, [tint, t], [joint; 2], 0.);
    emit(mesh, root, normal, 0.);
    for (i, &(t, w)) in profile.iter().enumerate() {
        let mid = root + axis * t;
        let k = base + 1 + i as u32 * 3;
        emit(mesh, mid - side * w, (normal - side * 0.20).normalize(), t);
        emit(mesh, mid + normal * w * 0.23, normal, t);
        emit(mesh, mid + side * w, (normal + side * 0.20).normalize(), t);
        if i == 0 {
            mesh.indices.extend([base, k, k + 1, base, k + 1, k + 2]);
        } else {
            mesh.indices.extend([
                k - 3,
                k,
                k - 2,
                k - 2,
                k,
                k + 1,
                k - 2,
                k + 1,
                k - 1,
                k - 1,
                k + 1,
                k + 2,
            ]);
        }
    }
    let end = mesh.positions.len() as u32;
    emit(mesh, root + axis, normal, 1.);
    mesh.indices
        .extend([end - 3, end, end - 2, end - 2, end, end - 1]);
}

// The branchlet is a continuous rib. The swept teeth on either side model groups of needles.
pub(super) fn feather(
    mesh: &mut Mesh,
    root: Vec3,
    axis: Vec3,
    spread: f32,
    roll: f32,
    joint: u16,
    tint: f32,
) {
    let dir = axis.normalize();
    let flat = dir.cross(Vec3::Y).normalize_or_zero();
    let side = flat * roll.cos() + dir.cross(flat) * roll.sin();
    let normal = side.cross(dir);
    let base = mesh.positions.len() as u32;
    for k in 0..=3 {
        let t = k as f32 / 3.;
        let p = root + axis * t - Vec3::Y * (0.10 * axis.length() * t * (1. - t));
        let w = spread * (1. - 0.77 * t);
        for sign in [-1., 1.] {
            vertex(
                mesh,
                p + side * (sign * w * 0.22),
                (normal + side * sign * 0.35).normalize(),
                [tint, 0.16 + 0.27 * t],
                [joint; 2],
                0.,
            );
        }
        if k == 3 {
            break;
        }
        for sign in [-1., 1.] {
            vertex(
                mesh,
                p + axis * (0.29 + 0.05 * sign) + side * (sign * w) - Vec3::Y * w * 0.10,
                (normal + side * sign * 1.25).normalize(),
                [tint, 0.80 + 0.17 * t],
                [joint; 2],
                0.,
            );
        }
        let i = base + 4 * k;
        mesh.indices.extend([
            i,
            i + 1,
            i + 4,
            i + 1,
            i + 5,
            i + 4,
            i,
            i + 4,
            i + 2,
            i + 1,
            i + 3,
            i + 5,
        ]);
    }
}

// An axial stem and tapered needle bundles. Gaps between bundles stay open in the fine mesh.
fn needle_shoot(mesh: &mut Mesh, root: Vec3, axis: Vec3, roll: f32, joint: u16, tint: f32) {
    let dir = axis.normalize();
    let flat = dir.cross(Vec3::Y).normalize();
    let side = flat * roll.cos() + dir.cross(flat) * roll.sin();
    let normal = side.cross(dir);
    let len = axis.length();
    let mut triangle = |points: [Vec3; 3], n: Vec3, tip: f32| {
        let first = mesh.positions.len() as u32;
        for (point, v) in points.into_iter().zip([0.12, 0.22, tip]) {
            vertex(mesh, point, n, [tint, v], [joint; 2], 0.);
        }
        mesh.indices.extend([first, first + 1, first + 2]);
    };
    triangle(
        [
            root - side * (len * 0.018),
            root + side * (len * 0.018),
            root + axis,
        ],
        normal,
        0.55,
    );
    for k in 0..6 {
        let t = k as f32 / 6.;
        for sign in [-1., 1.] {
            let at = t + 0.045 * (1. + sign);
            let base = root + axis * at;
            let reach = len * (0.24 - 0.12 * t);
            let end =
                base + axis * (0.24 - 0.09 * t) + side * (reach * sign) + normal * (0.24 * reach);
            triangle(
                [base, base + axis * 0.087, end],
                (normal + side * sign * 0.86).normalize(),
                1.,
            );
        }
    }
}

pub(super) fn needles(
    mesh: &mut Mesh,
    branch: Branch,
    heading: Vec3,
    species: Species,
    rng: &mut Random,
) {
    let side = heading.cross(Vec3::Y);
    let length = branch.tip.distance(branch.root);
    let spruce = species == Species::Spruce;
    let tint = 0.42 + rng.signed() * 0.28;
    let count = if spruce { 3 } else { 4 };
    for n in 0..count {
        let t = 0.13 + 0.83 * (n as f32 + rng.next() * 0.36) / count as f32;
        let sign = if n % 2 == 0 { -1. } else { 1. };
        let shoot_length = (0.30 + length * 0.59) * (1.22 - 0.55 * t) * (0.85 + rng.next() * 0.32);
        let axis = (heading * (0.65 + rng.next() * 0.4)
            + side * sign * (0.42 + rng.next() * 0.54)
            + Vec3::Y
                * (if spruce {
                    -0.47 + 0.36 * t
                } else {
                    0.25 + 0.58 * t
                } + rng.signed() * 0.25))
            .normalize();
        let root = branch.point(t);
        feather(
            mesh,
            root,
            axis * shoot_length,
            shoot_length * if spruce { 0.15 } else { 0.17 },
            rng.signed() * 0.69,
            branch.joint,
            tint,
        );
        if spruce {
            // Two shorter lateral shoots break up the broad face of the conifer spray.
            for sign in [-1., 1.] {
                let twig =
                    (axis * 0.64 + side * sign * 0.65 + Vec3::Y * (0.14 + rng.signed() * 0.28))
                        .normalize();
                needle_shoot(
                    mesh,
                    root + axis * shoot_length * (0.28 + 0.14 * sign),
                    twig * shoot_length * 0.64,
                    rng.signed() * 0.93,
                    branch.joint,
                    tint + 0.11,
                );
            }
        } else {
            let up = (axis * 0.60 + Vec3::Y * 0.8 + side * rng.signed() * 0.45).normalize();
            feather(
                mesh,
                root + axis * shoot_length * 0.34,
                up * shoot_length * 0.74,
                shoot_length * 0.14,
                rng.signed() * 1.3,
                branch.joint,
                tint + 0.10,
            );
        }
    }
}

pub(super) fn broad_leaves(
    mesh: &mut Mesh,
    twig: Branch,
    heading: Vec3,
    species: Species,
    rng: &mut Random,
) {
    let oak = species == Species::Oak;
    let span = heading.cross(Vec3::Y);
    let tint = 0.48 + rng.signed() * 0.31;
    for pair in 0..3 {
        let t = 0.23 + 0.31 * pair as f32;
        for sign in [-1., 1.] {
            let root = twig.point(t + rng.signed() * 0.035);
            let len = (if oak { 0.32 } else { 0.25 }) * (0.72 + 0.58 * rng.next());
            let axis = (heading * (0.25 + rng.next() * 0.66)
                + span * sign * (0.66 + rng.next() * 0.45)
                + Vec3::Y * (-0.48 + rng.next() * 0.93))
                .normalize();
            let flat = axis.cross(Vec3::Y).normalize();
            let roll = rng.signed() * 0.88;
            let side = flat * roll.cos() + axis.cross(flat) * roll.sin();
            let w = len * if oak { 0.30 } else { 0.34 };
            if oak {
                blade(
                    mesh,
                    root,
                    axis * len,
                    side,
                    &[
                        (0.28, w * 0.75),
                        (0.48, w * 0.62),
                        (0.65, w),
                        (0.81, w * 0.78),
                    ],
                    twig.joint,
                    tint,
                );
            } else {
                blade(
                    mesh,
                    root,
                    axis * len,
                    side,
                    &[(0.32, w), (0.65, w * 0.63)],
                    twig.joint,
                    tint,
                );
            }
        }
    }
}
