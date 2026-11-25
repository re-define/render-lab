//! Character locomotion uses a translation-only body; posed limbs supply contact intents.
use super::*;
use crate::{
    engine::SimulationData::{Component, LocalTransformsById},
    physics::{Particle, SupportFace},
};

pub(super) const BODY: &str = "lab.silver_wolf.body";
pub(super) const SPRING: &str = "lab.physics.spring_plate";
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
    // Prior top centers supply a translating movement base's velocity, including on replay.
    #[serde(default)]
    previous_centers: Vec<(Id, Vec3)>,
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
            previous_centers: Vec::new(),
            base_velocity: Vec3::ZERO,
        }
    }
    #[cfg(test)]
    pub(super) fn remap_supports(&mut self, map: impl Fn(Id) -> Id) {
        self.support = self.support.map(&map);
        for (id, _) in &mut self.previous_centers {
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
        self.previous_centers.clear();
        self.faces = Arc::default();
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.particle.position.is_finite()
                && self.particle.momentum.is_finite()
                && self.particle.mass.is_finite()
                && self.particle.mass > 0.
                && self.base_velocity.is_finite()
                && self.previous_centers.iter().all(|(_, p)| p.is_finite())
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
// A platform is integrated once per substep. All occupants see its same predicted state.
pub(super) struct Integrate(pub Vec<Vec<Arc<Stage>>>);
struct Solved {
    root: Mat4,
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
            after: &[PLAYER],
        }
    }
    fn access(&self) -> SystemAccess {
        SystemAccess {
            name: "Character momentum and support impulses",
            reads: &[
                Component(BODY),
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
    fn transform_targets(&self, _: &TransformBindings<'_>) -> Result<TransformTargets> {
        let mut targets = TransformTargets::default();
        for group in &self.0 {
            targets.writes.push(group[0].plate);
            for s in group {
                targets.reads.extend_from_slice(&s.supports);
                targets.reads.push(s.actor);
                targets.writes.push(s.actor);
            }
        }
        Ok(targets)
    }
    fn run(&self, cx: &mut SystemContext<'_>, dt: f32) -> Result<()> {
        cx.for_each_parallel(&self.0, |group, cx| {
            let id = group[0].plate;
            let mut plate = cx.local(id)?.context("missing spring plate")?;
            let mut spring = *cx
                .component::<Spring>(id, SPRING)?
                .context("missing spring state")?;
            spring.advance(&mut plate, dt);
            let predicted_y = plate.w_axis.y;
            let mut face = support(id, plate)?;
            face.velocity.y = spring.velocity;
            let mut delta_y = 0.;
            let mut delta_v = 0.;
            let mut solved = Vec::with_capacity(group.len());
            for actor in group {
                let result = solve(actor, cx, dt, face)?;
                delta_y += result.plate_height_delta;
                delta_v += result.plate_velocity_delta;
                solved.push(result);
            }
            spring.velocity += delta_v;
            plate.w_axis.y = spring.clamp_height(predicted_y + delta_y);
            let final_face = if plate.w_axis.y == predicted_y {
                face
            } else {
                support(id, plate)?
            };
            let center = final_face.center;
            for (s, mut result) in group.iter().zip(solved) {
                if result.body.support == Some(id) {
                    result.body.particle.position.y += plate.w_axis.y - predicted_y;
                }
                if let Some((_, last)) = result
                    .body
                    .previous_centers
                    .iter_mut()
                    .find(|(k, _)| *k == id)
                {
                    *last = center;
                }
                for f in &mut result.faces {
                    if f.id == id {
                        let velocity = f.velocity;
                        *f = final_face;
                        f.velocity = velocity;
                    }
                }
                result.body.faces = result.faces.into();
                result.root.w_axis = result.body.particle.position.extend(1.);
                cx.set_pose([(s.actor, result.root)])?;
                cx.set_typed_component(s.actor, BODY, result.body)?;
                cx.set_typed_component(s.actor, SETTING, result.contact)?;
            }
            cx.set_pose([(id, plate)])?;
            cx.set_typed_component(id, SPRING, spring)
        })
    }
}

fn solve(s: &Stage, cx: &SystemContext<'_>, dt: f32, plate: SupportFace) -> Result<Solved> {
    let mut c = s.state(cx)?;
    let mut body = cx
        .component::<Body>(s.actor, BODY)?
        .context("missing character body")?
        .into_owned();
    let root = cx.local(s.actor)?.context("missing character root")?;
    let mut plate_velocity_delta = 0.;
    let mut plate_height_delta = 0.;
    let mut floors = s
        .supports
        .iter()
        .map(|&id| {
            if id == s.plate {
                Ok(plate)
            } else {
                support(id, cx.local(id)?.context("missing support")?)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    if body.support.is_some_and(|id| !s.supports.contains(&id)) {
        body.support = None;
        body.base_velocity = Vec3::ZERO;
    }

    for floor in &mut floors {
        if let Some((_, prior)) = body.previous_centers.iter().find(|(id, _)| *id == floor.id) {
            floor.velocity = (floor.center - *prior) / dt;
        }
        // The plate has its own vertical degree of freedom.
        if floor.id == s.plate {
            floor.velocity.y = plate.velocity.y;
        }
    }
    let mut base = body
        .support
        .and_then(|id| floors.iter().find(|f| f.id == id))
        .map_or(Vec3::ZERO, |f| f.velocity);
    if body.support == Some(s.plate) {
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
    let goal = if let Some(play) = c.play {
        c.section.ground_goal(play)
    } else {
        root.w_axis.truncate()
    };
    if was_grounded && let Some(play) = &mut c.play {
        play.ground_offset += base * Vec3::new(1., 0., 1.) * dt;
    }
    if let Some((speed, scale)) = launch {
        // A single push at takeoff. The authored span sets desired forward launch speed.
        let dv = Vec3::Z * speed - (body.particle.velocity() - base) * Vec3::new(1., 0., 1.);
        let impulse = dv * mass + Vec3::Y * (body.jump_impulse * scale);
        if body.support == Some(s.plate) {
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
                    .sweep(before, end, surface.velocity * dt)
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
        let movable = surf.id == s.plate;
        let inv_mass = if movable { 1. / PLATE_MASS } else { 0. };
        let impulse = body.particle.contact(
            surf.normal,
            surf.velocity,
            inv_mass,
            if grounded { 0. } else { 0.38 },
        );
        if grounded {
            body.particle.position.y = at.y;
        } else {
            // Sweep returns the impact point expressed on the end-of-step face.
            body.particle.position =
                at + (body.particle.velocity() - surf.velocity) * dt * (1. - fraction);
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
    body.base_velocity = body
        .support
        .and_then(|id| floors.iter().find(|s| s.id == id))
        .map_or(Vec3::ZERO, |s| s.velocity);
    if body.support == Some(s.plate) {
        body.base_velocity.y = 0.;
    }
    body.previous_centers.clear();
    body.previous_centers
        .extend(floors.iter().map(|s| (s.id, s.center)));
    Ok(Solved {
        root,
        body,
        contact: c,
        plate_velocity_delta,
        plate_height_delta,
        faces: floors,
    })
}
