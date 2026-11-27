//! Resolve kinematic or simulated roots on the fixed movement tick.
use super::{CLOCK, LINEAR_BODY, LinearBody};
use crate::{
    engine::{
        FixedDomain, Schedule,
        SimulationData::{Component, LocalTransformsById},
        SimulationSystem, SystemAccess, SystemContext, TransformBindings, TransformTargets,
    },
    movement::{RootMotionRequest, SolvedRoot},
    physics::{ANGULAR_BODY, AngularBody, Particle},
    scene::{MOTION_POLICY, Mobility, MotionPolicy, PhysicsParticipation},
};
use anyhow::{Context, Result, ensure};
use std::{sync::Arc, time::Duration};

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
