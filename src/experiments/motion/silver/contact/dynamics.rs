//! Character locomotion uses a translation-only body; posed limbs supply contact intents.
use super::*;
use crate::{
    engine::SimulationData::{Component, LocalTransformsById, Resource as SimResource},
    movement::character::{PROPOSED, ProposedMotion, RESOLVED, ResolvedMotion},
    physics::{Capsule, Particle, SupportFace},
};

mod contacts;
mod platform;
mod root;
use platform::PLATE_MASS;
pub(super) use platform::{Spring, Turntables};
use root::solve;

pub(super) fn register(systems: &mut CharacterSystems, schedule: &mut Schedule) -> Result<()> {
    schedule.add_fixed_system(character_clock(), Arc::new(Turntables))?;
    systems.register(|actors| {
        let mut platforms = BTreeMap::<Id, Vec<Arc<Stage>>>::new();
        for actor in actors.values() {
            platforms
                .entry(actor.binding.plate)
                .or_default()
                .push(actor.clone());
        }
        Integrate(platforms.into_values().collect())
    });
    Ok(())
}

pub(super) const BODY: &str = "lab.silver_wolf.body";
pub(crate) const TURNTABLE: &str = "lab.physics.turntable";
pub(crate) const SPRING: &str = "lab.physics.spring_plate";
const MAX_STEP: f32 = 0.16;
const TRACTION: f32 = 1.4;

// Old saves recorded the top-face position only. They recover translation on the first
// substep and start measuring rotation once a full orientation has been observed.
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(untagged)]
enum PreviousFace {
    Rigid { center: Vec3, rotation: glam::Quat },
    Translation(Vec3),
}
impl PreviousFace {
    fn of(face: SupportFace) -> Self {
        Self::Rigid {
            center: face.center,
            rotation: face.rotation,
        }
    }
    fn pose(self) -> (Vec3, Option<glam::Quat>) {
        match self {
            Self::Rigid { center, rotation } => (center, Some(rotation)),
            Self::Translation(center) => (center, None),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Body {
    pub particle: Particle,
    pub gravity: f32,
    pub jump_impulse: f32,
    pub support: Option<Id>,
    pub flight_time: f32,
    pub landing_impulse: f32,
    pub palm_impulse: f32,
    #[serde(default)]
    pub capsule: Capsule,
    #[serde(default, alias = "previous_centers")]
    previous_faces: Vec<(Id, PreviousFace)>,
    #[serde(default)]
    base_velocity: Vec3,
}
impl Body {
    pub fn new(at: Vec3) -> Self {
        Self {
            particle: Particle {
                position: at,
                momentum: Vec3::ZERO,
                mass: 48.,
            },
            gravity: 9.81,
            jump_impulse: 137.34,
            support: None,
            flight_time: 0.,
            landing_impulse: 0.,
            palm_impulse: 0.,
            capsule: Capsule::default(),
            previous_faces: Vec::new(),
            base_velocity: Vec3::ZERO,
        }
    }
    pub(super) fn remap_supports(&mut self, map: impl Fn(Id) -> Id) {
        self.support = self.support.map(&map);
        for (id, _) in &mut self.previous_faces {
            *id = map(*id);
        }
    }
    pub fn reset_motion(&mut self, at: Vec3) {
        self.particle.position = at;
        self.particle.momentum = Vec3::ZERO;
        self.support = None;
        self.flight_time = 0.;
        self.landing_impulse = 0.;
        self.palm_impulse = 0.;
        self.base_velocity = Vec3::ZERO;
        self.previous_faces.clear();
    }
    pub fn validate(&self) -> Result<()> {
        self.capsule.validate()?;
        ensure!(
            self.particle.position.is_finite()
                && self.particle.momentum.is_finite()
                && self.particle.mass.is_finite()
                && self.particle.mass > 0.
                && self.base_velocity.is_finite()
                && self.previous_faces.iter().all(|(_, prior)| {
                    let (p, q) = prior.pose();
                    p.is_finite() && q.is_none_or(|q| q.is_finite() && q.is_normalized())
                })
                && self.gravity.is_finite()
                && self.gravity > 0.
                && self.jump_impulse.is_finite()
                && self.jump_impulse > 0.,
            "invalid character dynamics"
        );
        Ok(())
    }
}
// A platform is integrated once per substep. All occupants see its same predicted state.
pub(super) struct Integrate(pub Vec<Vec<Arc<Stage>>>);
struct Solved {
    step: u64,
    blocking_normal: Option<Vec3>,
    request: RootMotionRequest,
    before: Mat4,
    policy: MotionPolicy,
    body: Body,
    contact: Option<Contact>,
    plate_velocity_delta: f32,
    plate_height_delta: f32,
    faces: Vec<SupportFace>,
}
impl SimulationSystem for Integrate {
    fn order(&self) -> SystemOrder {
        SystemOrder {
            key: Some(BODY),
            after: &[PLAYER, collision::QUERY],
        }
    }
    fn access(&self) -> SystemAccess {
        SystemAccess {
            name: "Character momentum and support impulses",
            reads: &[
                Component(MOTION_POLICY),
                Component(BODY),
                Component(PROPOSED),
                Component(RESOLVED),
                SimResource(collision::QUERY),
                Component(SETTING),
                Component(SPRING),
                LocalTransformsById,
            ],
            writes: &[
                Component(RESOLVED),
                Component(BODY),
                Component(SETTING),
                Component(SPRING),
                LocalTransformsById,
            ],
        }
    }
    fn bound_transforms(&self) -> Option<TransformTargets> {
        let mut targets = TransformTargets::default();
        for group in &self.0 {
            targets.writes.push(group[0].binding.plate);
            for s in group {
                targets.reads.push(s.binding.plate);
                targets.reads.push(s.actor);
                targets.writes.push(s.actor);
            }
        }
        Some(targets)
    }
    fn run(&self, cx: &mut SystemContext<'_>, dt: f32) -> Result<()> {
        if self.0.is_empty() {
            return Ok(());
        }
        let query = cx
            .resource::<collision::Query>(collision::QUERY)?
            .context("missing collision snapshot")?;
        cx.for_each_parallel(&self.0, |group, cx| {
            let id = group[0].binding.plate;
            let plate_before = cx.local(id)?.context("missing spring plate")?;
            let mut plate = plate_before;
            let spring_before = *cx
                .component::<Spring>(id, SPRING)?
                .context("missing spring state")?;
            let mut spring = spring_before;
            let plate_policy = cx.motion_policy(id, MotionPolicy::DYNAMIC)?;
            let spring_active = plate_policy.physics == PhysicsParticipation::Simulated;
            if spring_active {
                spring.advance(&mut plate, dt);
            } else {
                spring.velocity = 0.;
            }
            let predicted_y = plate.w_axis.y;
            let shape = query.walkable_shape(id);
            let plate_face = |transform| {
                shape.and_then(|s| SupportFace::from_box(id, s.box_transform(transform), 0.65))
            };
            let mut face = plate_face(plate);
            if let Some(face) = &mut face {
                face.velocity.y = spring.velocity;
            }
            let mut delta_y = 0.;
            let mut delta_v = 0.;
            let mut solved = Vec::with_capacity(group.len());
            for actor in group {
                let result = solve(actor, cx, dt, face, spring_active, query)?;
                delta_y += result.plate_height_delta;
                delta_v += result.plate_velocity_delta;
                solved.push(result);
            }
            spring.velocity += delta_v;
            if spring_active {
                plate.w_axis.y = spring.clamp_height(predicted_y + delta_y);
            }
            let final_face = if plate.w_axis.y == predicted_y {
                face
            } else {
                plate_face(plate)
            };

            for (s, mut result) in group.iter().zip(solved) {
                if result.policy.physics == PhysicsParticipation::Simulated
                    && result.body.support == Some(id)
                {
                    result.body.particle.position.y += plate.w_axis.y - predicted_y;
                }
                if let Some((_, last)) = result
                    .body
                    .previous_faces
                    .iter_mut()
                    .find(|(k, _)| *k == id)
                {
                    *last = PreviousFace::of(final_face.context("missing final plate face")?);
                }
                for f in &mut result.faces {
                    if f.id == id {
                        let twist = (f.velocity, f.angular_velocity);
                        *f = final_face.context("missing final plate face")?;
                        (f.velocity, f.angular_velocity) = twist;
                    }
                }

                let simulated = (result.policy.physics == PhysicsParticipation::Simulated)
                    .then_some(result.body.particle.position);
                let root = result
                    .request
                    .resolve(result.policy, result.before, simulated)?;
                if root != result.before {
                    cx.set_pose([(s.actor, root)])?;
                }
                let velocity = (root.w_axis.truncate() - result.before.w_axis.truncate()) / dt;
                let support_velocity = result
                    .body
                    .support
                    .and_then(|id| result.faces.iter().find(|f| f.id == id))
                    .map_or(Vec3::ZERO, |f| f.velocity_at(root.w_axis.truncate()));
                cx.set_bound_state(
                    s.actor,
                    RESOLVED,
                    ResolvedMotion {
                        step: result.step,
                        root_local: root,
                        velocity,
                        gravity: if result.policy.physics == PhysicsParticipation::Simulated {
                            Vec3::NEG_Y * result.body.gravity
                        } else {
                            Vec3::ZERO
                        },
                        support: result.body.support,
                        support_velocity,
                        blocking_normal: result.blocking_normal,
                        surfaces: result.faces.into(),
                    },
                )?;
                cx.set_bound_state(s.actor, BODY, result.body)?;
                if let Some(contact) = result.contact {
                    cx.set_bound_state(s.actor, SETTING, contact)?;
                }
            }
            if spring_active && plate != plate_before {
                cx.set_pose([(id, plate)])?;
            }
            if spring != spring_before {
                cx.set_bound_state(id, SPRING, spring)?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
#[test]
fn serialized_translation_history_upgrades_to_rigid_history() -> Result<()> {
    let mut saved = serde_json::to_value(Body::new(Vec3::ZERO))?;
    saved.as_object_mut().unwrap().remove("previous_faces");
    saved["previous_centers"] = serde_json::json!([[11, [1.0, 2.0, 3.0]]]);
    let body: Body = serde_json::from_value(saved)?;
    body.validate()?;
    assert_eq!(body.previous_faces[0].0, 11);
    assert_eq!(
        body.previous_faces[0].1.pose(),
        (Vec3::new(1., 2., 3.), None)
    );
    Ok(())
}
