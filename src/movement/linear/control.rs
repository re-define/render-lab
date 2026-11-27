//! Convert player input to parent-space planar velocity for a selected ordinary body.
use crate::movement::{AnalogMove, MovementDriver};
use crate::scene::{Id, MotionPolicy, PhysicsParticipation, World};
use anyhow::{Context, Result, ensure};
use glam::Vec2;
use serde::{Deserialize, Serialize};

/// Opt-in parent-space planar controls. Bodies with only LINEAR_BODY retain authored motion.
pub const CONTROL: &str = "engine.motion.linear_control";
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct LinearControl {
    pub speed: f32,
    #[serde(default)]
    engaged: bool,
}
pub const DRIVER: MovementDriver = MovementDriver {
    component: CONTROL,
    camera_follows: |_, _| true,
    active: |world, id| {
        Ok(world
            .typed_component::<LinearControl>(id, CONTROL)?
            .context("missing linear controller")?
            .engaged)
    },
    apply: drive,
};

fn drive(world: &mut World, id: Id, input: AnalogMove) -> Result<()> {
    let mut controller = *world
        .typed_component::<LinearControl>(id, CONTROL)?
        .context("missing linear controller")?;
    let mut body = world
        .linear_body(id)?
        .context("linear control requires a body")?;
    let velocity = Vec2::new(input.side, input.forward).clamp_length_max(1.0) * controller.speed;
    body.target_velocity.x = velocity.x;
    body.target_velocity.z = velocity.y;
    if world.motion_policy(id, MotionPolicy::KINEMATIC)?.physics == PhysicsParticipation::Simulated
    {
        body.momentum.x = velocity.x * body.mass;
        body.momentum.z = velocity.y * body.mass;
    }
    controller.engaged = velocity != Vec2::ZERO;
    world.set_linear_body(id, body)?;
    world.set_typed_component(id, CONTROL, controller);
    Ok(())
}

impl World {
    pub fn disable_linear_control(&mut self, id: Id) -> Result<()> {
        if let Some(control) = self.typed_component::<LinearControl>(id, CONTROL)? {
            if control.engaged {
                drive(self, id, AnalogMove::default())?;
            }
            self.remove_typed_component(id, CONTROL);
            if self.control_selection() == Some(Some(id)) {
                self.select_control_target(None);
            }
        }
        Ok(())
    }

    pub fn enable_linear_control(&mut self, id: Id, speed: f32) -> Result<()> {
        ensure!(
            speed.is_finite() && speed > 0.,
            "control speed must be finite and positive"
        );
        self.linear_body(id)?
            .context("linear control requires a body")?;
        let engaged = self
            .typed_component::<LinearControl>(id, CONTROL)?
            .is_some_and(|c| c.engaged);
        self.set_typed_component(id, CONTROL, LinearControl { speed, engaged });
        Ok(())
    }
}
