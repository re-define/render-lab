//! Guide positions and velocities live in the experiment's metre-scale frame.
use glam::{Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::f32::consts::{PI, TAU};

pub(super) const GUIDES: usize = 64;
pub(super) const POINTS: usize = 14;
pub(super) const CENTER: Vec3 = Vec3::new(0., 1.43, 0.);
pub(super) const HEAD: f32 = 0.285;
pub(super) const SHOULDER_Y: f32 = 0.81;
pub(super) const SHOULDER_X: f32 = 0.255;
pub(super) const SHOULDER_RADIUS: f32 = 0.16;
pub(super) const MARGIN: f32 = 0.008;
#[cfg(test)]
const ITERATIONS: usize = 18;
pub(super) type Curve = [Vec3; POINTS];

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(super) struct Controls {
    pub wind: f32,
    pub stiffness: f32,
    pub motion: bool,
}
impl Default for Controls {
    fn default() -> Self {
        Self {
            wind: 4.,
            stiffness: 0.55,
            motion: true,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Strand {
    pub points: Curve,
    pub previous: Curve,
}
pub(super) struct Guide {
    pub rest: Curve,
    pub(super) length: [f32; POINTS - 1],
    pub(super) chord: [f32; POINTS - 2],
}

pub(super) fn groom(i: usize) -> Guide {
    let angle = TAU * i as f32 / GUIDES as f32;
    let ring = |a: f32| Vec3::new(a.sin(), 0., a.cos());
    let side = (1. - angle.cos()) * 0.5;
    let fringe = side.powf(3.);
    let length = 0.59 - 0.53 * fringe + 0.045 * (5. * angle).sin();
    let start = 0.10 + 0.19 * (angle * 3.0).sin().powi(2);
    let end = 1.79 - 0.34 * fringe;
    let loft = 0.020 + 0.008 * (angle * 7.).sin();
    let rest = std::array::from_fn(|j| {
        let u = j as f32 / (POINTS - 1) as f32;
        let split = 0.47;
        let arc = (u / split).min(1.);
        let radius = HEAD + 0.001 + loft * (arc * 3.).min(1.);
        let theta = start + (end - start) * arc;
        let tail = ((u - split) / (1. - split)).max(0.);
        let az = angle + 0.43 * (1. - arc).powi(2) + 0.055 * (8. * u + 3. * angle).sin() * u;
        let radial = radius * theta.sin() + 0.055 * (tail * PI).sin() + 0.050 * tail;
        CENTER + ring(az) * radial + Vec3::Y * (radius * theta.cos() - length * tail)
    });
    Guide {
        length: std::array::from_fn(|j| rest[j].distance(rest[j + 1])),
        chord: std::array::from_fn(|j| rest[j].distance(rest[j + 2])),
        rest,
    }
}

pub(super) fn head_frame(phase: f32) -> Mat4 {
    let q = Quat::from_rotation_y(0.66 * phase.sin())
        * Quat::from_rotation_z(0.115 * (1.5 * phase).sin());
    let drift = Vec3::new(0.105 * phase.sin(), 0.022 * (2. * phase).sin(), 0.);
    Mat4::from_rotation_translation(q, CENTER + drift - q * CENTER)
}
#[cfg(test)]
pub(super) fn shoulder_axis(p: Vec3) -> Vec3 {
    Vec3::new(p.x.clamp(-SHOULDER_X, SHOULDER_X), SHOULDER_Y, 0.)
}
#[cfg(test)]
fn project(p: &mut Vec3, center: Vec3, r: f32) {
    let d = *p - center;
    if d.length_squared() < r * r {
        *p = center + d.try_normalize().unwrap_or(Vec3::Y) * r;
    }
}

// XPBD scalar distance constraint. Both pinned scalp samples have zero inverse mass.
#[cfg(test)]
fn constrain(points: &mut Curve, i: usize, j: usize, rest: f32, alpha: f32, lambda: &mut f32) {
    let wi = f32::from(i >= 2);
    let wj = f32::from(j >= 2);
    if wi + wj == 0. {
        return;
    }
    let d = points[j] - points[i];
    let len = d.length();
    if len < 1e-8 {
        return;
    }
    let dl = (-(len - rest) - alpha * *lambda) / (wi + wj + alpha);
    *lambda += dl;
    let offset = d * (dl / len);
    points[i] -= wi * offset;
    points[j] += wj * offset;
}

#[cfg(test)]
impl Guide {
    pub fn initial(&self) -> Strand {
        Strand {
            points: self.rest,
            previous: self.rest,
        }
    }
    pub fn advance(&self, s: &mut Strand, frame: Mat4, controls: Controls, time: f32, dt: f32) {
        let h2 = dt * dt;
        let head = frame.transform_point3(CENTER);
        let mut bend = [0.; POINTS - 2];
        let mut stretch = [0.; POINTS - 1];
        let alpha = 0.0000006 / h2;
        let bend_alpha = (0.000015 + 0.004 * (1. - controls.stiffness).powi(3)) / h2;
        let damping = (-2.8 * dt).exp();
        for j in 0..POINTS {
            let p = s.points[j];
            let goal = frame.transform_point3(self.rest[j]);
            let t = j as f32 / (POINTS - 1) as f32;
            s.points[j] = if j < 2 {
                goal
            } else {
                let gust = 0.80
                    + 0.35 * (2.7 * time + 3. * p.y).sin()
                    + 0.16 * (4.4 * time + 4. * p.z).cos();
                let wind =
                    controls.wind * Vec3::new(gust, 0.10 * gust, 0.23 * (time + p.x * 2.).sin());
                let shape = (goal - p) * controls.stiffness * (20. + 120. * (1. - t).powi(3));
                p + (p - s.previous[j]) * damping + h2 * (Vec3::NEG_Y * 9.81 + wind + shape)
            };
            s.previous[j] = p;
        }
        for _ in 0..ITERATIONS {
            for (i, l) in bend.iter_mut().enumerate() {
                constrain(&mut s.points, i, i + 2, self.chord[i], bend_alpha, l);
            }
            for (i, l) in stretch.iter_mut().enumerate() {
                constrain(&mut s.points, i, i + 1, self.length[i], alpha, l);
            }
            for p in &mut s.points[2..] {
                project(p, head, HEAD + MARGIN);
                project(p, shoulder_axis(*p), SHOULDER_RADIUS + MARGIN);
            }
        }
    }
    pub fn max_stretch(&self, s: &Strand) -> f32 {
        self.length
            .iter()
            .zip(s.points.windows(2))
            .map(|(l, p)| (p[0].distance(p[1]) / l - 1.).max(0.))
            .fold(0., f32::max)
    }
}
pub(super) fn tangent(curve: &Curve, j: usize) -> Vec3 {
    (curve[(j + 1).min(POINTS - 1)] - curve[j.saturating_sub(1)]).normalize_or_zero()
}
