//! Translational dynamics in SI units. The caller owns the fixed clock and collision geometry.
mod support;
use glam::Vec3;
use serde::{Deserialize, Serialize};
pub use support::SupportFace;

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Particle {
    pub position: Vec3,
    pub momentum: Vec3,
    pub mass: f32,
}
impl Particle {
    pub fn velocity(&self) -> Vec3 {
        self.momentum / self.mass
    }
    pub fn impulse(&mut self, j: Vec3) {
        self.momentum += j;
    }
    /// Exact free flight over a step with constant acceleration.
    pub fn advance(&mut self, acceleration: Vec3, dt: f32) {
        self.position += (self.velocity() + 0.5 * acceleration * dt) * dt;
        self.impulse(self.mass * acceleration * dt);
    }
    /// Inelastic normal impulse against a support, optionally free to move along world Y.
    /// Return the impulse on the particle. Apply its opposite to the support's free axis.
    pub fn contact(
        &mut self,
        normal: Vec3,
        support_velocity: Vec3,
        support_inv_mass: f32,
        friction: f32,
    ) -> Vec3 {
        let relative = self.velocity() - support_velocity;
        let inv_mass = 1. / self.mass;
        let jn =
            (-relative.dot(normal) / (inv_mass + normal.y * normal.y * support_inv_mass)).max(0.);
        let mut impulse = jn * normal;
        let tangent = relative - normal * relative.dot(normal);
        if let Some(dir) = tangent.try_normalize() {
            let jt = tangent.length() / (inv_mass + dir.y * dir.y * support_inv_mass);
            impulse -= dir * jt.min(friction * jn);
        }
        self.impulse(impulse);
        impulse
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impact_conserves_vertical_momentum_and_limits_sliding_impulse() {
        let mut b = Particle {
            position: Vec3::ZERO,
            momentum: Vec3::new(200., -144., 0.),
            mass: 48.,
        };
        let (m, vy) = (22., 0.25);
        let before = b.momentum.y + m * vy;
        let impulse = b.contact(Vec3::Y, Vec3::Y * vy, 1. / m, 0.3);
        let out = vy - impulse.y / m;
        assert!((b.momentum.y + m * out - before).abs() < 1e-4);
        assert!((b.velocity().y - out).abs() < 1e-5);
        assert!((impulse.x.abs() - 0.3 * impulse.y).abs() < 1e-4);
        assert_eq!(
            b.contact(Vec3::Y, Vec3::Y * (out - 1.), 1. / m, 0.3),
            Vec3::ZERO
        );
    }
}
