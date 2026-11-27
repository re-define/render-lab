//! Finite support queries shared by character dynamics and animation contact IK.
use crate::scene::Id;
use glam::{Mat4, Quat, Vec3};

#[derive(Clone, Copy)]
pub struct SupportFace {
    pub id: Id,
    pub world: Mat4,
    inverse: Mat4,
    pub normal: Vec3,
    pub center: Vec3,
    pivot: Vec3,
    /// Linear velocity at the twist pivot in metres per second (top-face center by default).
    pub velocity: Vec3,
    pub angular_velocity: Vec3,
    pub(crate) rotation: Quat,
    min_normal_y: f32,
}
impl SupportFace {
    /// The local box spans -0.5 to 0.5 on each axis. Only its positive Y face is walkable.
    pub fn from_box(id: Id, world: Mat4, min_normal_y: f32) -> Option<Self> {
        if !world.is_finite()
            || world.determinant().abs() <= 1e-12
            || !(0.0..=1.).contains(&min_normal_y)
        {
            return None;
        }
        let inverse = world.inverse();
        let normal = inverse.transpose().transform_vector3(Vec3::Y).normalize();
        let center = world.transform_point3(0.5 * Vec3::Y);
        Some(Self {
            id,
            min_normal_y,
            world,
            inverse,
            normal,
            velocity: Vec3::ZERO,
            angular_velocity: Vec3::ZERO,
            rotation: world.to_scale_rotation_translation().1.normalize(),
            center,
            pivot: center,
        })
    }
    /// Estimate the rigid twist from two simulation samples. Legacy saves contain only a center.
    pub(crate) fn set_previous(&mut self, center: Vec3, rotation: Option<Quat>, dt: f32) {
        self.pivot = self.center;
        self.velocity = (self.center - center) / dt;
        self.angular_velocity = Vec3::ZERO;
        if let Some(old) = rotation
            && old != self.rotation
        {
            let delta = self.rotation * old.conjugate();
            let shortest = if delta.w < 0. { -delta } else { delta };
            self.angular_velocity = shortest.to_scaled_axis() / dt;
        }
    }

    /// Signed parent-Y angular drive about a stationary box origin (radians per second).
    /// Retain the sign and speed when sampling aliased poses, including on the first tick.
    pub(crate) fn set_driven_rotation(&mut self, radians_per_second: f32) {
        self.angular_velocity = Vec3::Y * radians_per_second;
        self.pivot = self.world.w_axis.truncate();
        self.velocity = Vec3::ZERO;
    }

    /// Velocity of a material point expressed in the current world frame.
    pub fn velocity_at(self, point: Vec3) -> Vec3 {
        self.velocity + self.angular_velocity.cross(point - self.pivot)
    }

    /// Carry a point from the start pose of this rigid support to the end pose.
    pub fn carry(self, point: Vec3, dt: f32) -> Vec3 {
        let translation = self.velocity * dt;
        if self.angular_velocity == Vec3::ZERO {
            return point + translation;
        }
        let previous_pivot = self.pivot - translation;
        self.pivot + Quat::from_scaled_axis(self.angular_velocity * dt) * (point - previous_pivot)
    }

    fn clearance(self, p: Vec3) -> f32 {
        (p - self.center).dot(self.normal)
    }
    pub fn below(self, p: Vec3) -> Option<Vec3> {
        if self.normal.y <= 0. || self.normal.y < self.min_normal_y {
            return None;
        }
        let at = p - Vec3::Y * self.clearance(p) / self.normal.y;
        let q = self.inverse.transform_point3(at);
        (q.x.abs() <= 0.50001 && q.z.abs() <= 0.50001).then_some(at)
    }
    /// Predict a landing on a finite support assuming its current linear and angular velocities persist.
    /// Rotating faces refine the first descending crossing in a bounded look-ahead window.
    pub fn landing_time(self, at: Vec3, velocity: Vec3, gravity: f32) -> Option<f32> {
        if self.normal.y <= 0. || self.normal.y < self.min_normal_y {
            return None;
        }
        if self.angular_velocity.length_squared() > 1e-8 {
            let horizon = 3.;
            let slices = (self.angular_velocity.length() * horizon * 12.)
                .ceil()
                .clamp(48., 192.) as usize;
            let relative_at = |time: f32| {
                let turn = Quat::from_scaled_axis(-self.angular_velocity * time);
                let ballistic = at + (velocity - self.velocity) * time
                    - Vec3::Y * (0.5 * gravity * time * time);
                self.pivot + turn * (ballistic - self.pivot)
            };
            let mut start = 0.;
            let mut above = self.clearance(at);
            for i in 1..=slices {
                let end = horizon * i as f32 / slices as f32;
                let clearance = self.clearance(relative_at(end));
                if above >= 0. && clearance < 0. {
                    let (mut a, mut b) = (start, end);
                    for _ in 0..12 {
                        let mid = 0.5 * (a + b);
                        if self.clearance(relative_at(mid)) > 0. {
                            a = mid;
                        } else {
                            b = mid;
                        }
                    }
                    let hit = (a + b) * 0.5;
                    let normal = Quat::from_scaled_axis(self.angular_velocity * hit) * self.normal;
                    if normal.y >= self.min_normal_y && self.below(relative_at(hit)).is_some() {
                        return Some(hit);
                    }
                }
                start = end;
                above = clearance;
            }
            return None;
        }
        let relative = velocity - self.velocity;
        let gn = gravity * self.normal.y;
        let vn = relative.dot(self.normal);
        let disc = vn * vn + 2. * gn * self.clearance(at);
        if disc < 0. {
            return None;
        }
        let time = (vn + disc.sqrt()) / gn;
        if !time.is_finite() || time <= 0. {
            return None;
        }
        let point = at + relative * time - 0.5 * Vec3::Y * gravity * time * time;
        self.below(point).map(|_| time)
    }
    // Intersect the swept sole point against the finite top face in the translating support frame.
    pub fn sweep(self, start: Vec3, end: Vec3, support_delta: Vec3) -> Option<(f32, Vec3)> {
        let old = start + support_delta;
        let (a, b) = (self.clearance(old), self.clearance(end));
        if a < -1e-4 || b > 0. || a - b < 1e-7 {
            return None;
        }
        let t = (a / (a - b)).clamp(0., 1.);
        self.below(old.lerp(end, t)).map(|p| (t, p))
    }
    pub fn highest_below(p: Vec3, surfaces: &[Self], max_y: f32) -> Option<(Self, Vec3)> {
        surfaces
            .iter()
            .filter_map(|&s| s.below(p).filter(|v| v.y <= max_y).map(|v| (s, v)))
            .max_by(|a, b| a.1.y.total_cmp(&b.1.y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Quat;

    #[test]
    fn driven_rotation_keeps_signed_velocity_across_whole_turns_about_the_box_origin() {
        let origin = Vec3::new(3., 2., -4.);
        let transform = Mat4::from_scale_rotation_translation(
            Vec3::splat(2.),
            Quat::from_rotation_x(0.4),
            origin,
        );
        let mut deck = SupportFace::from_box(1, transform, 0.6).unwrap();
        let dt = 1.;
        let omega = -2.5 * std::f32::consts::TAU;
        deck.set_driven_rotation(omega);
        let point = origin + Vec3::X;
        assert!(deck.velocity_at(point).abs_diff_eq(-omega * Vec3::Z, 1e-5));
        assert!(deck.carry(point, dt).abs_diff_eq(origin - Vec3::X, 2e-5));
        assert_eq!(deck.velocity_at(origin), Vec3::ZERO);
    }

    #[test]
    fn anticipated_landing_uses_actual_height_footprint_and_relative_motion() {
        let deck = SupportFace::from_box(
            1,
            Mat4::from_scale_rotation_translation(
                Vec3::new(2., 1., 2.),
                Quat::IDENTITY,
                Vec3::new(0., -0.5, 0.),
            ),
            0.65,
        )
        .unwrap();
        let at = Vec3::Y;
        let t = deck.landing_time(at, Vec3::ZERO, 10.).unwrap();
        assert!((t - (0.2f32).sqrt()).abs() < 1e-6);
        assert!(deck.landing_time(at, Vec3::X * 10., 10.).is_none());
        let moving = SupportFace {
            velocity: Vec3::X * 10. + Vec3::Y,
            ..deck
        };
        let landing = moving.landing_time(at, Vec3::X * 10., 10.).unwrap();
        assert!(landing < t && (1. - 5. * landing * landing - landing).abs() < 1e-5);
    }
}
