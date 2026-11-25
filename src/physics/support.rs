//! Finite support queries shared by character dynamics and animation contact IK.
use crate::scene::Id;
use glam::{Mat4, Vec3};

#[derive(Clone, Copy)]
pub struct SupportFace {
    pub id: Id,
    pub world: Mat4,
    inverse: Mat4,
    pub normal: Vec3,
    pub center: Vec3,
    pub velocity: Vec3,
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
        Some(Self {
            id,
            min_normal_y,
            world,
            inverse,
            normal,
            velocity: Vec3::ZERO,
            center: world.transform_point3(0.5 * Vec3::Y),
        })
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
    /// Earliest descending ballistic intersection with this finite translating face.
    pub fn landing_time(self, at: Vec3, velocity: Vec3, gravity: f32) -> Option<f32> {
        if self.normal.y <= 0. || self.normal.y < self.min_normal_y {
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
