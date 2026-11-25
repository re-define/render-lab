//! Character locomotion uses a translation-only body; posed limbs supply contact intents.
use super::*;
use crate::{
    engine::SimulationData::{Component, LocalTransformsById},
    physics::Particle,
};

pub(super) const BODY: &str = "lab.silver_wolf.body";
const PLATE_MASS: f32 = 22.;
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
            previous_centers: Vec::new(),
            base_velocity: Vec3::ZERO,
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
#[derive(Clone, Copy)]
pub(super) struct Surface {
    pub id: Id,
    pub world: Mat4,
    inverse: Mat4,
    pub normal: Vec3,
    center: Vec3,
    velocity: Vec3,
}
impl Surface {
    pub fn new(id: Id, world: Mat4) -> Self {
        let inverse = world.inverse();
        let normal = inverse.transpose().transform_vector3(Vec3::Y).normalize();
        Self {
            id,
            world,
            inverse,
            normal,
            velocity: Vec3::ZERO,
            center: world.transform_point3(0.5 * Vec3::Y),
        }
    }
    fn clearance(self, p: Vec3) -> f32 {
        (p - self.center).dot(self.normal)
    }
    pub fn below(self, p: Vec3) -> Option<Vec3> {
        if self.normal.y < 0.65 {
            return None;
        }
        let at = p - Vec3::Y * self.clearance(p) / self.normal.y;
        let q = self.inverse.transform_point3(at);
        (q.x.abs() <= 0.50001 && q.z.abs() <= 0.50001).then_some(at)
    }
    /// Earliest descending ballistic intersection with this finite translating face.
    fn landing_time(self, at: Vec3, velocity: Vec3, gravity: f32) -> Option<f32> {
        if self.normal.y < 0.65 {
            return None;
        }
        let relative = velocity - self.velocity;
        let gn = gravity * self.normal.y;
        let vn = relative.dot(self.normal);
        let disc = vn * vn + 2. * gn * self.clearance(at);
        if disc < 0. {
            return None;
        }
        let time = (vn + disc.sqrt()) / gn;
        if !time.is_finite() || time <= 0. {
            return None;
        }
        let point = at + relative * time - 0.5 * Vec3::Y * gravity * time * time;
        self.below(point).map(|_| time)
    }
    // Intersect the swept sole point against the finite top face in the translating support frame.
    fn sweep(self, start: Vec3, end: Vec3, support_delta: Vec3) -> Option<(f32, Vec3)> {
        let old = start + support_delta;
        let (a, b) = (self.clearance(old), self.clearance(end));
        if a < -1e-4 || b > 0. || a - b < 1e-7 {
            return None;
        }
        let t = (a / (a - b)).clamp(0., 1.);
        self.below(old.lerp(end, t)).map(|p| (t, p))
    }
}
pub(super) fn highest(p: Vec3, surfaces: &[Surface], max_y: f32) -> Option<(Surface, Vec3)> {
    surfaces
        .iter()
        .filter_map(|&s| s.below(p).filter(|v| v.y <= max_y).map(|v| (s, v)))
        .max_by(|a, b| a.1.y.total_cmp(&b.1.y))
}
pub(super) fn surfaces(s: &Stage, cx: &SystemContext<'_>) -> Result<Vec<Surface>> {
    s.supports
        .iter()
        .map(|&id| Ok(Surface::new(id, cx.local(id)?.context("missing support")?)))
        .collect()
}

pub(super) struct Integrate(pub Arc<Stage>);
impl SimulationSystem for Integrate {
    fn order(&self) -> SystemOrder {
        SystemOrder {
            key: Some(BODY),
            after: &[PLAYER, PREPARE],
        }
    }
    fn access(&self) -> SystemAccess {
        SystemAccess {
            name: "Character momentum and support impulses",
            reads: &[Component(BODY), Component(SETTING), LocalTransformsById],
            writes: &[Component(BODY), Component(SETTING), LocalTransformsById],
        }
    }
    fn transform_targets(&self, _: &TransformBindings<'_>) -> Result<TransformTargets> {
        let s = &self.0;
        let mut reads = s.supports.clone();
        reads.push(s.actor);
        Ok(TransformTargets {
            reads,
            writes: vec![s.actor, s.plate],
        })
    }
    fn run(&self, cx: &mut SystemContext<'_>, dt: f32) -> Result<()> {
        let s = &self.0;
        let mut c = s.state(cx)?;
        let mut body = cx
            .component::<Body>(s.actor, BODY)?
            .context("missing character body")?
            .into_owned();
        let mut root = cx.local(s.actor)?.context("missing character root")?;
        let mut plate = cx.local(s.plate)?.context("missing spring plate")?;
        let old_y = plate.w_axis.y;
        c.velocity += (-7200. * (old_y - c.rest_y) - 670. * c.velocity) * dt / PLATE_MASS;
        let y = old_y + c.velocity * dt;
        plate.w_axis.y = y.clamp(c.rest_y - 0.075, c.rest_y + 0.01);
        if y != plate.w_axis.y {
            c.velocity = 0.;
        }
        let mut floors = surfaces(s, cx)?;

        for floor in &mut floors {
            if floor.id == s.plate {
                *floor = Surface::new(s.plate, plate);
            }
            if let Some((_, prior)) = body.previous_centers.iter().find(|(id, _)| *id == floor.id) {
                floor.velocity = (floor.center - *prior) / dt;
            }
            // The plate has its own vertical degree of freedom.
            if floor.id == s.plate {
                floor.velocity.y = c.velocity;
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
                c.velocity -= impulse.y / PLATE_MASS;
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
            contact = highest(end, &floors, before.y + MAX_STEP)
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
                c.velocity += dv;
                let remaining = if grounded { dt } else { dt * (1. - fraction) };
                let corrected =
                    (plate.w_axis.y + dv * remaining).clamp(c.rest_y - 0.075, c.rest_y + 0.01);
                body.particle.position.y += corrected - plate.w_axis.y;
                plate.w_axis.y = corrected;
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
        body.previous_centers.extend(floors.iter().map(|s| {
            (
                s.id,
                if s.id == self.0.plate {
                    plate.transform_point3(0.5 * Vec3::Y)
                } else {
                    s.center
                },
            )
        }));
        root.w_axis = body.particle.position.extend(1.);
        cx.set_pose([(s.actor, root), (s.plate, plate)])?;
        cx.set_typed_component(s.actor, BODY, body)?;
        cx.set_typed_component(s.actor, SETTING, c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn anticipated_landing_uses_actual_height_footprint_and_relative_motion() {
        let deck = Surface::new(
            1,
            Mat4::from_scale_rotation_translation(
                Vec3::new(2., 1., 2.),
                Quat::IDENTITY,
                Vec3::new(0., -0.5, 0.),
            ),
        );
        let at = Vec3::Y;
        let t = deck.landing_time(at, Vec3::ZERO, 10.).unwrap();
        assert!((t - (0.2f32).sqrt()).abs() < 1e-6);
        assert!(deck.landing_time(at, Vec3::X * 10., 10.).is_none());
        let moving = Surface {
            velocity: Vec3::X * 10. + Vec3::Y,
            ..deck
        };
        let landing = moving.landing_time(at, Vec3::X * 10., 10.).unwrap();
        assert!(landing < t && (1. - 5. * landing * landing - landing).abs() < 1e-5);
    }
}
