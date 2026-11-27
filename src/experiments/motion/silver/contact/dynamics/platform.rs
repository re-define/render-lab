//! Spring translation and turntable drives for shared supports.
use super::*;

pub(super) const PLATE_MASS: f32 = 22.;

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Spring {
    pub rest_y: f32,
    pub velocity: f32,
}
impl Spring {
    pub(super) fn advance(&mut self, plate: &mut Mat4, dt: f32) {
        let old_y = plate.w_axis.y;
        self.velocity += (-7200. * (old_y - self.rest_y) - 670. * self.velocity) * dt / PLATE_MASS;
        let y = old_y + self.velocity * dt;
        plate.w_axis.y = self.clamp_height(y);
        if y != plate.w_axis.y {
            self.velocity = 0.;
        }
    }
    pub(super) fn clamp_height(self, y: f32) -> f32 {
        y.clamp(self.rest_y - 0.075, self.rest_y + 0.01)
    }
}
/// Optional yaw drive, registered independently on the character clock. Its state is
/// the support entity's transform plus its speed in radians per second.
pub struct Turntables;
impl SimulationSystem for Turntables {
    fn order(&self) -> SystemOrder {
        SystemOrder {
            key: Some(TURNTABLE),
            after: &[],
        }
    }
    fn access(&self) -> SystemAccess {
        SystemAccess {
            name: "Rotating support drives",
            reads: &[
                Component(TURNTABLE),
                Component(MOTION_POLICY),
                LocalTransformsById,
            ],
            writes: &[LocalTransformsById],
        }
    }
    fn transform_targets(&self, bindings: &TransformBindings<'_>) -> Result<TransformTargets> {
        let ids = bindings.component_owners(TURNTABLE)?.to_vec();
        Ok(TransformTargets {
            reads: ids.clone(),
            writes: ids,
        })
    }
    fn run(&self, cx: &mut SystemContext<'_>, dt: f32) -> Result<()> {
        for id in cx.component_owners(TURNTABLE)?.to_vec() {
            let rate = *cx
                .component::<f32>(id, TURNTABLE)?
                .context("missing turntable speed")?;
            let policy = cx.motion_policy(id, MotionPolicy::MOVING_COLLIDER)?;
            if policy.mobility == Mobility::Static {
                continue;
            }
            ensure!(
                policy.physics != PhysicsParticipation::Simulated,
                "turntable {id} is a kinematic drive; a dynamic angular solver is required for simulated rotation"
            );
            let angle = rate * dt;
            ensure!(angle.is_finite(), "non-finite turntable rotation");
            if angle != 0. {
                let transform = cx.local(id)?.context("missing turntable transform")?;
                let (scale, rotation, translation) = transform.to_scale_rotation_translation();
                let rotated = (Quat::from_rotation_y(angle) * rotation).normalize();
                cx.set_pose([(
                    id,
                    Mat4::from_scale_rotation_translation(scale, rotated, translation),
                )])?;
            }
        }
        Ok(())
    }
}
