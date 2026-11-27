//! Continuous upright capsule against an affine box, including its rounded edge/corner contacts.
//! Separation is the segment-to-convex-hull distance minus the radius.
use super::Capsule;
use glam::{Mat4, Vec3};

fn closest_triangle(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0. && d2 <= 0. {
        return a;
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0. && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0. && d1 >= 0. && d3 <= 0. {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0. && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0. && d2 >= 0. && d6 <= 0. {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0. && d4 >= d3 && d5 >= d6 {
        return b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let inv = 1. / (va + vb + vc);
    a + ab * (vb * inv) + ac * (vc * inv)
}

fn closest_edge(p: Vec3, q: Vec3, a: Vec3, b: Vec3) -> Vec3 {
    let (u, v, r) = (q - p, b - a, p - a);
    let uu = u.length_squared();
    let vv = v.length_squared();
    let uv = u.dot(v);
    if uu < 1e-14 {
        return p - (a + v * (v.dot(p - a) / vv).clamp(0., 1.));
    }
    let denom = uu * vv - uv * uv;
    let mut s = if denom > 1e-12 * uu * vv {
        ((uv * v.dot(r) - vv * u.dot(r)) / denom).clamp(0., 1.)
    } else {
        0.
    };
    let t = (uv * s + v.dot(r)) / vv;
    if t < 0. {
        s = (-u.dot(r) / uu).clamp(0., 1.)
    }
    if t > 1. {
        s = ((uv - u.dot(r)) / uu).clamp(0., 1.)
    }
    p + u * s - a - v * t.clamp(0., 1.)
}

fn separation(p: Vec3, q: Vec3, corners: &[Vec3; 8]) -> Vec3 {
    let mut best = Vec3::splat(f32::INFINITY);
    let mut take = |v: Vec3| {
        if v.length_squared() < best.length_squared() {
            best = v
        }
    };
    for [i, j, k, l] in [
        [0, 1, 3, 2],
        [4, 6, 7, 5],
        [0, 4, 5, 1],
        [2, 3, 7, 6],
        [0, 2, 6, 4],
        [1, 5, 7, 3],
    ] {
        let [a, b, c, d] = [i, j, k, l].map(|n| corners[n]);
        for (a, b, c) in [(a, b, c), (a, c, d)] {
            take(p - closest_triangle(p, a, b, c));
            take(q - closest_triangle(q, a, b, c));
            let n = (b - a).cross(c - a);
            let denom = n.dot(q - p);
            if denom.abs() > 1e-12 {
                let t = n.dot(a - p) / denom;
                if (0.0..=1.).contains(&t) {
                    let at = p + (q - p) * t;
                    take(at - closest_triangle(at, a, b, c));
                }
            }
        }
        for (a, b) in [(a, b), (b, c), (c, d), (d, a)] {
            take(closest_edge(p, q, a, b));
        }
    }
    best
}
/// The signed vector from the closest point on an axis-aligned box to a vertical segment.
fn axis_separation(p: Vec3, q: Vec3, lo: Vec3, hi: Vec3) -> Vec3 {
    let mut gap = p - p.clamp(lo, hi);
    gap.y = (p.y - hi.y).max(0.) + (q.y - lo.y).min(0.);
    gap
}

/// Cast the capsule sole from start to end. The box can translate over the substep.
/// The returned normal points out of the box in the caller's coordinate system.
pub(crate) fn sweep_capsule(
    shape: Capsule,
    start: Vec3,
    end: Vec3,
    box_end: Mat4,
    box_delta: Vec3,
) -> Option<(f32, Vec3)> {
    let motion = end - start - box_delta;
    if motion.length_squared() < 1e-16 {
        return None;
    }
    let [x, y, z, _] = box_end.to_cols_array_2d();
    let axis_aligned =
        x[1] == 0. && x[2] == 0. && y[0] == 0. && y[2] == 0. && z[0] == 0. && z[1] == 0.;
    let center = box_end.w_axis.truncate();
    let half = Vec3::new(x[0], y[1], z[2]).abs() * 0.5;
    let corners = (!axis_aligned).then(|| {
        std::array::from_fn(|i| {
            box_end.transform_point3(Vec3::new(
                (i & 1) as f32 - 0.5,
                ((i >> 1) & 1) as f32 - 0.5,
                ((i >> 2) & 1) as f32 - 0.5,
            ))
        })
    });
    let mut time = 0.;
    for _ in 0..24 {
        let at = start + box_delta + time * motion;
        let p = at + Vec3::Y * shape.radius;
        let q = at + Vec3::Y * (shape.height - shape.radius);
        let delta = match &corners {
            Some(corners) => separation(p, q, corners),
            None => axis_separation(p, q, center - half, center + half),
        };
        let distance = delta.length();
        let normal = if distance > 1e-7 {
            delta / distance
        } else {
            -motion.normalize()
        };
        let closing = -normal.dot(motion);
        if closing <= 1e-7 {
            return None;
        }
        let gap = distance - shape.radius;
        if gap <= 1e-5 {
            return Some((time, normal));
        }
        time += gap / closing;
        if time > 1. {
            return None;
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    use glam::Quat;
    #[test]
    fn swept_capsule_hits_affine_wall_and_ceiling_and_clears_rounded_corner() {
        let c = Capsule {
            radius: 0.2,
            height: 1.6,
            ..Default::default()
        };
        let wall = Mat4::from_scale_rotation_translation(
            Vec3::new(0.1, 4., 4.),
            Quat::IDENTITY,
            Vec3::new(2., 1., 0.),
        );
        let hit = sweep_capsule(c, Vec3::ZERO, Vec3::X * 100., wall, Vec3::ZERO).unwrap();
        assert!((hit.0 - 0.0175).abs() < 1e-5 && hit.1.dot(-Vec3::X) > 0.999);
        let rotation = Quat::from_rotation_y(0.73);
        let toward = rotation * Vec3::X;
        let tilted = Mat4::from_scale_rotation_translation(
            Vec3::new(0.1, 4., 4.),
            rotation,
            toward * 2. + Vec3::Y,
        );
        let hit = sweep_capsule(c, Vec3::ZERO, toward * 100., tilted, Vec3::ZERO).unwrap();
        assert!((hit.0 - 0.0175).abs() < 1e-5 && hit.1.dot(-toward) > 0.999);
        let roof = Mat4::from_scale_rotation_translation(
            Vec3::new(4., 0.2, 4.),
            Quat::IDENTITY,
            Vec3::Y * 2.,
        );
        let hit = sweep_capsule(c, Vec3::ZERO, Vec3::Y, roof, Vec3::ZERO).unwrap();
        assert!((hit.0 - 0.3).abs() < 1e-5 && hit.1.y < -0.999);
        let at = Vec3::new(0.66, 2., 0.66);
        assert!(sweep_capsule(c, at, at - Vec3::Y * 4., Mat4::IDENTITY, Vec3::ZERO).is_none());
        let hit = sweep_capsule(
            c,
            Vec3::ZERO,
            Vec3::ZERO,
            wall * Mat4::from_translation(Vec3::X * (-20.)),
            Vec3::X * (-2.),
        )
        .unwrap();
        assert!(hit.1.x < -0.999);
    }
}
