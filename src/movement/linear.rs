//! Translation in parent coordinates. Distance is measured in metres and time in seconds.
use super::{AnalogMove, MovementDriver, RootMotionRequest, SolvedRoot};
use crate::{
    engine::{
        FixedDomain, Schedule,
        SimulationData::{Component, LocalTransformsById},
        SimulationSystem, SystemAccess, SystemContext, TransformBindings, TransformTargets,
    },
    physics::{ANGULAR_BODY, AngularBody, Particle},
    scene::{Id, MOTION_POLICY, Mobility, MotionPolicy, PhysicsParticipation, World},
};
use anyhow::{Context, Result, ensure};
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};

pub const CLOCK: &str = "movement";
pub const LINEAR_BODY: &str = "engine.motion.linear_body";

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

/// Parameters and current momentum survive save/load. Kinematic velocity and gravity are independent.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinearBody {
    pub mass: f32,
    pub momentum: Vec3,
    pub target_velocity: Vec3,
    pub acceleration: Vec3,
}
impl Default for LinearBody {
    fn default() -> Self {
        Self {
            mass: 1.,
            momentum: Vec3::ZERO,
            target_velocity: Vec3::ZERO,
            acceleration: -9.81 * Vec3::Y,
        }
    }
}
impl LinearBody {
    pub fn validate(self) -> Result<()> {
        ensure!(
            self.mass.is_finite()
                && self.mass > 0.
                && self.momentum.is_finite()
                && self.target_velocity.is_finite()
                && self.acceleration.is_finite(),
            "invalid linear motion parameters"
        );
        Ok(())
    }
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

    pub fn linear_body(&self, id: Id) -> Result<Option<LinearBody>> {
        Ok(self
            .typed_component::<LinearBody>(id, LINEAR_BODY)?
            .map(|body| *body))
    }

    pub fn set_linear_body(&mut self, id: Id, body: LinearBody) -> Result<()> {
        body.validate()?;
        let entity = self
            .entities
            .get(&id)
            .context("missing linear body entity")?;
        ensure!(
            self.root_provider(entity).is_none(),
            "entity {id} already has a root motion provider"
        );
        self.set_typed_component(id, LINEAR_BODY, body);
        Ok(())
    }

    /// Resolve authored participation once on assembly, before any module claims a root.
    pub(crate) fn install_motion_providers(
        &mut self,
        providers: &'static [crate::scene::RootWriter],
    ) -> Result<()> {
        self.motion_writers = providers;
        // Stored JSON is decoded once before the policy and body validation passes.
        self.hydrate_components::<MotionPolicy>(MOTION_POLICY)?;
        self.hydrate_components::<LinearBody>(LINEAR_BODY)?;
        self.hydrate_components::<AngularBody>(ANGULAR_BODY)?;
        self.hydrate_components::<LinearControl>(CONTROL)?;
        for id in self.validate_motion_components()? {
            self.set_linear_body(id, LinearBody::default())?;
        }
        Ok(())
    }

    /// Return ordinary simulated roots which still need their implicit linear body.
    pub(crate) fn validate_motion_components(&self) -> Result<Vec<Id>> {
        let mut claimed = std::collections::BTreeMap::new();
        for writer in self.motion_writers {
            for &id in self.entities.component_owners(writer.component) {
                if writer.active(&self.entities[&id])
                    && let Some(other) = claimed.insert(id, writer.component)
                {
                    anyhow::bail!(
                        "entity {id} has conflicting root motion providers {other} and {}",
                        writer.component
                    );
                }
            }
        }
        for &id in self.entities.component_owners(LINEAR_BODY) {
            self.linear_body(id)?
                .context("missing linear body")?
                .validate()?;
            ensure!(
                self.root_provider(&self.entities[&id]).is_none(),
                "entity {id} has conflicting root motion providers"
            );
        }
        let mut ordinary = Vec::new();
        for &id in self.entities.component_owners(MOTION_POLICY) {
            let policy = self.motion_policy(id, MotionPolicy::STATIC)?;
            if policy.physics != PhysicsParticipation::Simulated {
                continue;
            }
            let entity = &self.entities[&id];
            match self.root_provider(entity) {
                Some(provider) => ensure!(
                    provider.physics == PhysicsParticipation::Simulated,
                    "entity {id} needs a physics solver for simulated root motion"
                ),
                None if !entity.components.contains_key(LINEAR_BODY) => ordinary.push(id),
                None => {}
            }
        }
        for &id in self.entities.component_owners(CONTROL) {
            let control = self
                .typed_component::<LinearControl>(id, CONTROL)?
                .context("missing linear control")?;
            ensure!(
                control.speed.is_finite() && control.speed > 0.,
                "invalid linear control speed on {id}"
            );
            ensure!(
                self.linear_body(id)?.is_some() || ordinary.binary_search(&id).is_ok(),
                "linear control on {id} requires a body"
            );
        }
        Ok(ordinary)
    }
}

struct Integrate;
impl SimulationSystem for Integrate {
    fn access(&self) -> SystemAccess {
        SystemAccess {
            name: "Linear motion",
            reads: &[
                Component(LINEAR_BODY),
                Component(ANGULAR_BODY),
                Component(MOTION_POLICY),
                LocalTransformsById,
            ],
            writes: &[
                Component(LINEAR_BODY),
                Component(ANGULAR_BODY),
                LocalTransformsById,
            ],
        }
    }
    fn transform_targets(&self, bindings: &TransformBindings<'_>) -> Result<TransformTargets> {
        let ids = bindings.component_owners(LINEAR_BODY)?;
        Ok(TransformTargets {
            reads: ids.to_vec(),
            writes: ids.to_vec(),
        })
    }
    fn run(&self, cx: &mut SystemContext<'_>, dt: f32) -> Result<()> {
        if dt == 0. {
            return Ok(());
        }
        let ids = cx.component_owners(LINEAR_BODY)?;
        cx.for_each_parallel(ids, |&id, cx| {
            let policy = cx.motion_policy(id, MotionPolicy::KINEMATIC)?;
            if policy.mobility == Mobility::Static {
                return Ok(());
            }
            let local = cx.local(id)?.context("missing linear transform")?;
            let mut body = *cx
                .component::<LinearBody>(id, LINEAR_BODY)?
                .context("missing body")?;
            let mut desired = local;
            desired.w_axis += (body.target_velocity * dt).extend(0.);
            let simulated = if policy.physics == PhysicsParticipation::Simulated {
                let mut particle = Particle {
                    position: local.w_axis.truncate(),
                    momentum: body.momentum,
                    mass: body.mass,
                };
                particle.advance(body.acceleration, dt);
                ensure!(
                    particle.position.is_finite() && particle.momentum.is_finite(),
                    "non-finite linear motion on entity {id}"
                );
                if body.momentum != particle.momentum {
                    body.momentum = particle.momentum;
                    cx.set_bound_state(id, LINEAR_BODY, body)?;
                }
                let rotation =
                    if let Some(angular) = cx.component::<AngularBody>(id, ANGULAR_BODY)? {
                        let mut angular = angular.into_owned();
                        let previous = angular.momentum;
                        ensure!(
                            local.determinant().is_finite() && local.determinant() != 0.,
                            "singular angular body transform {id}"
                        );
                        let current = local.to_scale_rotation_translation().1;
                        let next = angular.advance(current, dt)?;
                        if angular.momentum != previous {
                            cx.set_bound_state(id, ANGULAR_BODY, angular)?;
                        }
                        Some(next)
                    } else {
                        None
                    };
                Some(SolvedRoot {
                    position: particle.position,
                    rotation,
                })
            } else {
                None
            };
            let resolved =
                RootMotionRequest::new(desired)?.resolve_body(policy, local, simulated)?;
            if local != resolved {
                cx.set_pose([(id, resolved)])?;
            }
            Ok(())
        })
    }
}

pub fn register(schedule: &mut Schedule) -> Result<()> {
    schedule.add_fixed_system(
        FixedDomain {
            name: CLOCK,
            step: Duration::from_secs_f64(1. / 60.),
            max_steps: 8,
        },
        Arc::new(Integrate),
    )
}
