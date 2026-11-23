//! Short woodland blades, seed-bearing meadow grasses and arching bank sedges.
use super::{Mesh, PlantBone, Random, TAU, Vec3, bone, empty, vertex};

fn direction(angle: f32) -> Vec3 {
    Vec3::new(angle.cos(), 0., angle.sin())
}

fn grass_vertex(mesh: &mut Mesh, p: Vec3, n: Vec3, tint: f32, t: f32, height: f32) {
    let level = (height.max(0.) * 3.4).min(3.);
    let joint = level.floor() as u16;
    vertex(
        mesh,
        p,
        n,
        [tint, t],
        [joint, (joint + 1).min(3)],
        level.fract(),
    );
}

fn blade(mesh: &mut Mesh, root: Vec3, dir: Vec3, height: f32, sweep: f32, width: f32, tint: f32) {
    let side = Vec3::Y.cross(dir);
    let base = mesh.positions.len() as u32;
    for i in 0..3 {
        let t = i as f32 / 3.;
        let drop = 0.46 * sweep;
        let p = root + Vec3::Y * (height * t - drop * t * t * t) + dir * (sweep * t * t);
        let tangent = Vec3::Y * (height - 3. * drop * t * t) + dir * (2. * sweep * t);
        let normal = tangent.cross(side).normalize();
        let w = width * ((0.55 + 0.45 * (std::f32::consts::PI * t.sqrt()).sin()) * (1. - 0.62 * t));
        for (lane, rib) in [(-1., 0.), (0., 0.38), (1., 0.)] {
            grass_vertex(
                mesh,
                p + side * lane * w + normal * rib * w,
                (normal + side * lane * 0.50).normalize(),
                tint,
                t,
                p.y,
            );
        }
        if i > 0 {
            let v = base + i * 3;
            mesh.indices.extend([
                v - 3,
                v,
                v - 2,
                v - 2,
                v,
                v + 1,
                v - 2,
                v + 1,
                v - 1,
                v - 1,
                v + 1,
                v + 2,
            ]);
        }
    }
    let tip = root + dir * sweep + Vec3::Y * (height - 0.46 * sweep);
    let end = mesh.positions.len() as u32;
    grass_vertex(mesh, tip, (dir + Vec3::Y).normalize(), tint, 1., tip.y);
    mesh.indices
        .extend([end - 3, end, end - 2, end - 2, end, end - 1]);
}

fn seedhead(mesh: &mut Mesh, stem: Vec3, height: f32, rng: &mut Random) {
    let heading = direction(rng.next() * TAU);
    let base = mesh.positions.len() as u32;
    let tip = stem + Vec3::Y * height + heading * 0.09;
    let side = Vec3::Y.cross(heading) * 0.0034;
    for (p, t) in [(stem - side, 0.), (stem + side, 0.), (tip, 1.)] {
        grass_vertex(mesh, p, heading, -1.86, t, p.y);
    }
    mesh.indices.extend([base, base + 1, base + 2]);
    for i in 0..7 {
        let t = 0.61 + i as f32 * 0.051;
        let root = stem.lerp(tip, t);
        let dir = direction(i as f32 * 2.40 + rng.signed() * 0.24);
        let twig = root + dir * ((1. - t) * 0.48) + Vec3::Y * (0.047 + rng.next() * 0.058);
        let n = (dir + Vec3::Y).normalize();
        let side = Vec3::Y.cross(dir);
        let b = mesh.positions.len() as u32;
        for p in [root - side * 0.0018, root + side * 0.0018, twig] {
            grass_vertex(mesh, p, n, -1.93, 0.84, p.y);
        }
        mesh.indices.extend([b, b + 1, b + 2]);
        for grain in 0..3 {
            let r = root.lerp(twig, 0.58 + 0.19 * grain as f32);
            let sign = if grain % 2 == 0 { -1. } else { 1. };
            let axis = (Vec3::Y * 0.67 + dir * 0.24 + side * sign * 0.49).normalize();
            let end = r + axis * (0.023 + rng.next() * 0.026);
            let w = (dir + side * 0.31).normalize() * (0.0028 + 0.0015 * rng.next());
            let b = mesh.positions.len() as u32;
            for p in [r, r.lerp(end, 0.49) - w, r.lerp(end, 0.49) + w, end] {
                grass_vertex(mesh, p, n, -3.42, 0.87, p.y);
            }
            mesh.indices.extend([b, b + 1, b + 2, b + 1, b + 3, b + 2]);
        }
    }
}

pub(in crate::experiments::vegetation) fn river_grass(
    seed: u32,
    kind: usize,
) -> (Mesh, Vec<PlantBone>) {
    let mut rng = Random::new(seed);
    let mut mesh = empty();
    let mut bones = Vec::new();
    bone(&mut bones, None, Vec3::ZERO, 0.);
    for j in 1..4 {
        bone(
            &mut bones,
            Some(j - 1),
            Vec3::Y * (j as f32 - 1.) * 0.29,
            0.11,
        );
    }
    let (height, width, reach) = match kind {
        0 => (0.30, 0.012, 0.30), // shady groundcover
        1 => (0.58, 0.011, 0.40), // meadow tussocks
        _ => (0.70, 0.014, 0.54), // riverbank sedges
    };
    for rosette in 0..5 {
        let axis = direction(rosette as f32 * 2.40 + rng.signed() * 0.42);
        let root = axis
            * if rosette == 0 {
                0.
            } else {
                0.16 + 0.29 * rng.next()
            };
        let vigor = 0.75 + rng.next() * 0.45;
        for leaf in 0..9 {
            let dir = (direction(leaf as f32 * 2.40 + rng.signed() * 0.64)
                + axis * 0.31
                + Vec3::X * 0.27)
                .normalize();
            let h = height * vigor * (0.47 + 0.85 * rng.next());
            let s = reach * (0.48 + 0.83 * rng.next());
            let tint = -1. - rng.next() * 0.85;
            blade(
                &mut mesh,
                root + dir * rng.next() * 0.045,
                dir,
                h,
                s,
                width * (0.75 + 0.46 * rng.next()),
                tint,
            );
        }
        if kind == 1 && rosette < 2 {
            seedhead(&mut mesh, root, 0.81 + 0.23 * rng.next(), &mut rng);
        }
    }
    (mesh, bones)
}
