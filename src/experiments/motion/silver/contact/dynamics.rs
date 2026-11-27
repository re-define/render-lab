//! Character locomotion uses a translation-only body; posed limbs supply contact intents.
use super::*;
use crate::{
    engine::SimulationData::{Component, LocalTransformsById},
    physics::{Particle, SupportFace},
};

pub(super) fn register(systems: &mut CharacterSystems, schedule: &mut Schedule) -> Result<()> {
    schedule.add_fixed_system(character_clock(), Arc::new(Turntables))?;
    systems.register(|actors| {
        let mut platforms = BTreeMap::<Id, Vec<Arc<Stage>>>::new();
        for actor in actors.values() {
            platforms
                .entry(actor.plate)
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
const PLATE_MASS: f32 = 22.;

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(super) struct Spring {
    pub rest_y: f32,
    pub velocity: f32,
}
impl Spring {
    fn advance(&mut self, plate: &mut Mat4, dt: f32) {
        let old_y = plate.w_axis.y;
        self.velocity += (-7200. * (old_y - self.rest_y) - 670. * self.velocity) * dt / PLATE_MASS;
        let y = old_y + self.velocity * dt;
        plate.w_axis.y = self.clamp_height(y);
        if y != plate.w_axis.y {
            self.velocity = 0.;
        }
    }
    fn clamp_height(self, y: f32) -> f32 {
        y.clamp(self.rest_y - 0.075, self.rest_y + 0.01)
    }
}
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
    /// Final support poses for same-substep IK. Recomputed before the first IK after load.
    #[serde(skip)]
    pub faces: Arc<[SupportFace]>,
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
            faces: Arc::default(),
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
        self.faces = Arc::default();
    }
    pub fn validate(&self) -> Result<()> {
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
fn support(id: Id, model: Mat4) -> Result<SupportFace> {
    SupportFace::from_box(id, model, 0.65).context("invalid walkable support transform")
}
/// Optional yaw drive, registered independently on the character clock. Its state is
/// the support entity's transform plus its speed in radians per second.
pub(super) struct Turntables;
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
// A platform is integrated once per substep. All occupants see its same predicted state.
pub(super) struct Integrate(pub Vec<Vec<Arc<Stage>>>);
struct Solved {
    request: RootMotionRequest,
    before: Mat4,
    policy: MotionPolicy,
    body: Body,
    contact: Contact,
    plate_velocity_delta: f32,
    plate_height_delta: f32,
    faces: Vec<SupportFace>,
}
impl SimulationSystem for Integrate {
    fn order(&self) -> SystemOrder {
        SystemOrder {
            key: Some(BODY),
            after: &[PLAYER, TURNTABLE],
        }
    }
    fn access(&self) -> SystemAccess {
        SystemAccess {
            name: "Character momentum and support impulses",
            reads: &[
                Component(MOTION_POLICY),
                Component(BODY),
                Component(TURNTABLE),
                Component(SETTING),
                Component(SPRING),
                LocalTransformsById,
            ],
            writes: &[
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
            targets.writes.push(group[0].plate);
            for s in group {
                targets.reads.extend_from_slice(&s.supports);
                targets.reads.push(s.actor);
                targets.writes.push(s.actor);
            }
        }
        Some(targets)
    }
    fn run(&self, cx: &mut SystemContext<'_>, dt: f32) -> Result<()> {
        cx.for_each_parallel(&self.0, |group, cx| {
            let id = group[0].plate;
            let mut plate = cx.local(id)?.context("missing spring plate")?;
            let mut spring = *cx
                .component::<Spring>(id, SPRING)?
                .context("missing spring state")?;
            let plate_policy = cx.motion_policy(id, MotionPolicy::DYNAMIC)?;
            let spring_active = plate_policy.physics == PhysicsParticipation::Simulated;
            if spring_active {
                spring.advance(&mut plate, dt);
            } else {
                spring.velocity = 0.;
            }
            let predicted_y = plate.w_axis.y;
            let face = if plate_policy.physics != PhysicsParticipation::Off {
                let mut face = support(id, plate)?;
                face.velocity.y = spring.velocity;
                Some(face)
            } else {
                None
            };
            let mut delta_y = 0.;
            let mut delta_v = 0.;
            let mut solved = Vec::with_capacity(group.len());
            for actor in group {
                let result = solve(actor, cx, dt, face, spring_active)?;
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
                Some(support(id, plate)?)
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
                result.body.faces = result.faces.into();
                let simulated = (result.policy.physics == PhysicsParticipation::Simulated)
                    .then_some(result.body.particle.position);
                let root = result
                    .request
                    .resolve(result.policy, result.before, simulated)?;
                if root != result.before {
                    cx.set_pose([(s.actor, root)])?;
                }
                cx.set_typed_component(s.actor, BODY, result.body)?;
                cx.set_typed_component(s.actor, SETTING, result.contact)?;
            }
            if spring_active {
                cx.set_pose([(id, plate)])?;
            }
            cx.set_typed_component(id, SPRING, spring)
        })
    }
}

fn solve(
    s: &Stage,
    cx: &SystemContext<'_>,
    dt: f32,
    plate: Option<SupportFace>,
    spring_active: bool,
) -> Result<Solved> {
    let mut c = s.state(cx)?;
    let mut body = cx
        .component::<Body>(s.actor, BODY)?
        .context("missing character body")?
        .into_owned();
    let before_root = cx.local(s.actor)?.context("missing character root")?;
    let motion = c
        .request
        .take()
        .context("animation did not publish root motion this substep")?;
    if let Some(velocity) = motion.inherited_velocity {
        body.reset_motion(before_root.w_axis.truncate());
        body.particle.momentum = velocity * body.particle.mass;
    }
    let request = motion.root;
    let policy = cx.motion_policy(s.actor, MotionPolicy::DYNAMIC)?;
    let mut plate_velocity_delta = 0.;
    let mut plate_height_delta = 0.;
    let mut floors = Vec::with_capacity(s.supports.len());
    if policy.physics != PhysicsParticipation::Off {
        for &id in &s.supports {
            if id == s.plate {
                floors.extend(plate);
            } else {
                let support_policy = cx.motion_policy(id, MotionPolicy::FIXED_COLLIDER)?;
                if support_policy.physics != PhysicsParticipation::Off {
                    floors.push(support(id, cx.local(id)?.context("missing support")?)?);
                }
            }
        }
    }
    if body
        .support
        .is_some_and(|id| !floors.iter().any(|face| face.id == id))
    {
        body.support = None;
        body.base_velocity = Vec3::ZERO;
    }

    for floor in &mut floors {
        if let Some((_, prior)) = body.previous_faces.iter().find(|(id, _)| *id == floor.id) {
            let (center, rotation) = prior.pose();
            floor.set_previous(center, rotation, dt);
        }
        if let Some(rate) = cx.component::<f32>(floor.id, TURNTABLE)? {
            // The drive rotates about parent Y through the object's origin. Its signed
            // angular velocity is known even when the poses differ by whole turns.
            let movable = cx
                .motion_policy(floor.id, MotionPolicy::MOVING_COLLIDER)?
                .mobility
                == Mobility::Movable;
            floor.set_driven_rotation(if movable { *rate } else { 0. });
        }
        // The plate has its own vertical degree of freedom.
        if spring_active
            && floor.id == s.plate
            && let Some(plate) = plate
        {
            floor.velocity.y = plate.velocity.y;
        }
    }
    if policy.physics != PhysicsParticipation::Simulated {
        let resolved = request.resolve(policy, before_root, None)?;
        let at = resolved.w_axis.truncate();
        let velocity = (at - before_root.w_axis.truncate()) / dt;
        body.reset_motion(at);
        body.particle.momentum = velocity * body.particle.mass;
        body.support = SupportFace::highest_below(at, &floors, at.y + MAX_STEP)
            .filter(|(_, point)| point.y >= at.y - MAX_STEP)
            .map(|(face, _)| face.id);
        body.previous_faces
            .extend(floors.iter().map(|f| (f.id, PreviousFace::of(*f))));
        return Ok(Solved {
            before: before_root,
            request,
            policy,
            body,
            contact: c,
            plate_velocity_delta,
            plate_height_delta,
            faces: floors,
        });
    }
    let previous_support = body.support;
    let mut base = body
        .support
        .and_then(|id| floors.iter().find(|f| f.id == id))
        .map_or(Vec3::ZERO, |f| f.velocity_at(body.particle.position));
    if spring_active && body.support == Some(s.plate) {
        base.y = 0.;
    }
    if body.support.is_some() {
        let carrier_impulse = (base - body.base_velocity) * body.particle.mass;
        body.particle.impulse(carrier_impulse);
    }
    let before = body.particle.position;
    let was_grounded = body.support.is_some();
    let mass = body.particle.mass;
    let launch = c
        .play
        .and_then(|p| c.section.launch(p))
        .filter(|_| was_grounded);
    let goal = request.target_local().w_axis.truncate();
    if was_grounded && let Some(play) = &mut c.play {
        play.ground_offset += base * Vec3::new(1., 0., 1.) * dt;
    }
    if let Some((speed, scale)) = launch {
        // A single push at takeoff. The authored span sets desired forward launch speed.
        let dv = Vec3::Z * speed - (body.particle.velocity() - base) * Vec3::new(1., 0., 1.);
        let impulse = dv * mass + Vec3::Y * (body.jump_impulse * scale);
        if spring_active && body.support == Some(s.plate) {
            plate_velocity_delta -= impulse.y / PLATE_MASS;
        }
        body.particle.impulse(impulse);
        body.support = None;
        body.flight_time = 0.;
        body.palm_impulse = 0.;
        c.play.as_mut().unwrap().launched = true;
    } else if was_grounded {
        let desired = (goal - before) * Vec3::new(1., 0., 1.) / dt;
        let current = (body.particle.velocity() - base) * Vec3::new(1., 0., 1.);
        let impulse =
            ((desired - current) * mass).clamp_length_max(TRACTION * mass * body.gravity * dt);
        body.particle.impulse(impulse);
    }
    let mut acceleration = -body.gravity * Vec3::Y;
    if body.support.is_none()
        && c.palm_load > 0.
        && c.hand_ik
        && c.section == traversal::Section::Rail
    {
        // A fraction of body weight is supported by the planted palm during the preceding pose.
        let lift = 0.35 * body.gravity * c.palm_load;
        acceleration += Vec3::Y * lift;
        body.palm_impulse += mass * lift * dt;
    }
    body.particle.advance(acceleration, dt);
    let end = body.particle.position;
    let grounded = body.support.is_some();
    let mut contact = None;
    if grounded {
        contact = SupportFace::highest_below(end, &floors, before.y + MAX_STEP)
            .filter(|(_, p)| p.y >= before.y - MAX_STEP)
            .map(|(s, p)| (1., s, p));
    }
    if contact.is_none() {
        contact = floors
            .iter()
            .filter_map(|&surface| {
                surface
                    .sweep(before, end, surface.carry(before, dt) - before)
                    .map(|(t, p)| (t, surface, p))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
    }
    body.support = contact.map(|(_, s, _)| s.id);
    if grounded && contact.is_none() {
        body.flight_time = dt;
    } else if !grounded {
        body.flight_time += contact.map_or(dt, |(t, _, _)| dt * t);
    }
    if contact.is_none()
        && let Some(play) = &mut c.play
        && play.launched
        && !play.landed
        && c.section == traversal::Section::Gap
    {
        let remaining = floors
            .iter()
            .filter_map(|f| f.landing_time(end, body.particle.velocity(), body.gravity))
            .min_by(f32::total_cmp);
        if let Some(remaining) = remaining {
            c.section.sample_flight(play, body.flight_time, remaining);
        }
    }
    if let Some((fraction, surf, at)) = contact {
        let movable = spring_active && surf.id == s.plate;
        let inv_mass = if movable { 1. / PLATE_MASS } else { 0. };
        let impulse = body.particle.contact(
            surf.normal,
            surf.velocity_at(at),
            inv_mass,
            if grounded { 0. } else { 0.38 },
        );
        if grounded {
            body.particle.position.y = at.y;
        } else {
            // Sweep returns the impact point expressed on the end-of-step face.
            body.particle.position =
                at + (body.particle.velocity() - surf.velocity_at(at)) * dt * (1. - fraction);
            if let Some(point) = surf.below(body.particle.position) {
                body.particle.position.y = point.y;
            }
        }
        if movable {
            let dv = -impulse.y / PLATE_MASS;
            plate_velocity_delta += dv;
            let remaining = if grounded { dt } else { dt * (1. - fraction) };
            plate_height_delta += dv * remaining;
        }
        if !grounded {
            body.landing_impulse = impulse.dot(surf.normal);
            if let Some(play) = &mut c.play
                && play.launched
                && !play.landed
            {
                c.section.land(play, body.particle.position);
            }
        }
    }
    body.base_velocity = if body.support == previous_support && body.support.is_some() {
        // The applied carrier impulse used the point velocity at the start of the step.
        // Keep that sample so a rotating carrier supplies the next centripetal change.
        base
    } else {
        body.support
            .and_then(|id| floors.iter().find(|s| s.id == id))
            .map_or(Vec3::ZERO, |s| s.velocity_at(body.particle.position))
    };
    if spring_active && body.support == Some(s.plate) {
        body.base_velocity.y = 0.;
    }
    body.previous_faces.clear();
    body.previous_faces
        .extend(floors.iter().map(|s| (s.id, PreviousFace::of(*s))));
    Ok(Solved {
        before: before_root,
        request,
        policy,
        body,
        contact: c,
        plate_velocity_delta,
        plate_height_delta,
        faces: floors,
    })
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
