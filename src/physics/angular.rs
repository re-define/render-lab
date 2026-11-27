//! Rigid rotation about the centre of mass. Angular momentum and external torque are
//! expressed in the parent simulation frame. Principal inertias use the body's local frame.
use super::Particle;
use crate::scene::{Id, World};
use anyhow::{Result, ensure};
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

pub const ANGULAR_BODY: &str = "engine.physics.angular_body";
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct AngularBody {
    /// Diagonal principal moments (kg m²).
    pub inertia: Vec3,
    pub momentum: Vec3,
    pub torque: Vec3,
}
impl AngularBody {
    pub fn validate(self) -> Result<()> {
        ensure!(
            self.inertia.is_finite()
                && self.inertia.min_element() > 0.
                && self.momentum.is_finite()
                && self.torque.is_finite(),
            "invalid angular dynamics"
        );
        Ok(())
    }
    fn velocity(self, orientation: Quat, momentum: Vec3) -> Vec3 {
        orientation * ((orientation.conjugate() * momentum) / self.inertia)
    }
    /// Midpoint integration on rotations. Keep the parent-space momentum constant in free flight.
    pub fn advance(&mut self, orientation: Quat, dt: f32) -> Result<Quat> {
        self.validate()?;
        ensure!(
            orientation.is_finite() && orientation.is_normalized() && dt.is_finite() && dt >= 0.,
            "invalid angular integration input"
        );
        let next = self.momentum + self.torque * dt;
        let angle = self.momentum.length().max(next.length()) * dt / self.inertia.min_element();
        ensure!(angle.is_finite(), "angular step overflow");
        // Resolve asymmetric-body precession even at a coarse outer update frequency.
        let count = (angle / 0.25).ceil().clamp(1., 256.) as u32;
        let step = dt / count as f32;
        let mut q = orientation;
        for i in 0..count {
            let l = self.momentum + self.torque * (step * (i as f32 + 0.5));
            let half = (Quat::from_scaled_axis(self.velocity(q, l) * (0.5 * step)) * q).normalize();
            q = (Quat::from_scaled_axis(self.velocity(half, l) * step) * q).normalize();
        }
        ensure!(q.is_finite(), "non-finite angular solution");
        self.momentum = next;
        Ok(q)
    }
    /// Apply the same impulse to the centre of mass and its lever arm.
    pub fn impulse_at(&mut self, body: &mut Particle, point: Vec3, impulse: Vec3) -> Result<()> {
        ensure!(
            point.is_finite() && impulse.is_finite(),
            "invalid rigid impulse"
        );
        let momentum = self.momentum + (point - body.position).cross(impulse);
        ensure!(
            momentum.is_finite() && (body.momentum + impulse).is_finite(),
            "rigid impulse overflow"
        );
        body.impulse(impulse);
        self.momentum = momentum;
        Ok(())
    }
}
impl World {
    pub fn set_angular_body(&mut self, id: Id, body: AngularBody) -> Result<()> {
        ensure!(
            self.linear_body(id)?.is_some(),
            "angular integration requires a linear body on entity {id}"
        );
        body.validate()?;
        self.set_typed_component(id, ANGULAR_BODY, body);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_angular_body_runs_on_the_movement_clock_and_reloads() -> Result<()> {
        use crate::{
            engine::Schedule,
            experiments::movement_lab,
            movement::linear::{self, LINEAR_BODY},
        };
        use std::sync::Arc;
        let mut w = movement_lab::scene()?;
        let id = w.entities.component_owners(LINEAR_BODY)[2];
        let (scale, orientation, _) = w.entities[&id].local.to_scale_rotation_translation();
        w.set_angular_body(
            id,
            AngularBody {
                inertia: Vec3::splat(2.),
                momentum: Vec3::Y * 4.,
                torque: Vec3::ZERO,
            },
        )?;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        let mut w = Arc::new(w);
        let mut schedule = Schedule::default();
        linear::register(&mut schedule)?;
        for _ in 0..30 {
            schedule.tick(&mut w, 1. / 60., &pool)?;
        }
        let (s, q, _) = w.entities[&id].local.to_scale_rotation_translation();
        assert!(s.abs_diff_eq(scale, 1e-5));
        assert!(q.dot(Quat::from_rotation_y(1.) * orientation).abs() > 0.99999);
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("spin.json");
        w.save(&file)?;
        let mut loaded = Arc::new(World::load(&file)?);
        let mut second = Schedule::default();
        linear::register(&mut second)?;
        second.tick(&mut loaded, 1. / 60., &pool)?;
        schedule.tick(&mut w, 1. / 60., &pool)?;
        assert!(
            loaded.entities[&id]
                .local
                .abs_diff_eq(w.entities[&id].local, 1e-6)
        );
        Ok(())
    }
    #[test]
    fn eccentric_impulse_free_precession_and_applied_torque() -> Result<()> {
        let mut p = Particle {
            position: Vec3::new(4., 1., 2.),
            momentum: Vec3::ZERO,
            mass: 2.,
        };
        let mut body = AngularBody {
            inertia: Vec3::new(1., 2., 3.),
            momentum: Vec3::ZERO,
            torque: Vec3::ZERO,
        };
        let arm = p.position + Vec3::X;
        body.impulse_at(&mut p, arm, Vec3::Y * 6.)?;
        assert_eq!(p.velocity(), Vec3::Y * 3.);
        assert_eq!(body.momentum, Vec3::Z * 6.);
        let q = body.advance(Quat::IDENTITY, 0.5)?;
        assert!(q.abs_diff_eq(Quat::from_rotation_z(1.), 1e-5));
        body.momentum = Vec3::new(0.3, 1.7, 2.4);
        let l = body.momentum;
        let initial_energy = 0.5 * l.dot(body.velocity(q, l));
        let mut r = q;
        for _ in 0..600 {
            r = body.advance(r, 1. / 120.)?;
        }
        assert_eq!(body.momentum, l);
        assert!((0.5 * l.dot(body.velocity(r, l)) - initial_energy).abs() < 1e-4);
        body.torque = Vec3::X * 1.3;
        body.advance(r, 0.25)?;
        assert!(body.momentum.abs_diff_eq(l + Vec3::X * 0.325, 1e-5));
        Ok(())
    }
}
