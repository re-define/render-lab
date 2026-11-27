//! Apply an instantaneous parent-frame impulse to the ordinary rigid-motion components.
use super::{ANGULAR_BODY, AngularBody, Particle};
use crate::scene::{Id, MotionPolicy, World};
use anyhow::{Context, Result, ensure};
use glam::Vec3;

impl World {
    pub fn angular_body(&self, id: Id) -> Result<Option<AngularBody>> {
        Ok(self
            .typed_component::<AngularBody>(id, ANGULAR_BODY)?
            .map(|body| *body))
    }

    /// Impulse is in N·s, the optional application point in parent-space metres.
    /// The ordinary rigid body uses its entity origin as its centre of mass.
    pub fn apply_body_impulse(&mut self, id: Id, impulse: Vec3, point: Option<Vec3>) -> Result<()> {
        ensure!(
            self.motion_policy(id, MotionPolicy::KINEMATIC)? == MotionPolicy::DYNAMIC,
            "impulses require a movable, simulated body"
        );
        let mut linear = self
            .linear_body(id)?
            .context("impulse requires an ordinary linear body")?;
        linear.validate()?;
        let center = self.entities[&id].local.w_axis.truncate();
        let at = point.unwrap_or(center);
        ensure!(
            impulse.is_finite() && at.is_finite() && center.is_finite(),
            "non-finite impulse or point"
        );
        let mut particle = Particle {
            position: center,
            mass: linear.mass,
            momentum: linear.momentum,
        };
        let mut angular = self.angular_body(id)?;
        if let Some(a) = &mut angular {
            a.validate()?;
            a.impulse_at(&mut particle, at, impulse)?;
        } else {
            ensure!(
                (at - center).cross(impulse) == Vec3::ZERO,
                "an off-centre torque requires angular inertia"
            );
            particle.impulse(impulse);
        }
        // Stage both momentum changes before publishing either component.
        linear.momentum = particle.momentum;
        self.set_linear_body(id, linear)?;
        if let Some(a) = angular {
            self.set_typed_component(id, ANGULAR_BODY, a);
        }
        Ok(())
    }
}
