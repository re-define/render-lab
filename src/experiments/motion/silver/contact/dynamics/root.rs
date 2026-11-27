//! Solve one character root against the current collision and support snapshot.
use super::contacts::{collide_solids, support_faces};
use super::*;
use crate::physics::collision::Bounds;

pub(super) fn solve(
    s: &Stage,
    cx: &SystemContext<'_>,
    dt: f32,
    plate: Option<SupportFace>,
    spring_active: bool,
    query: &collision::Query,
) -> Result<Solved> {
    let c = cx
        .component::<Contact>(s.actor, SETTING)?
        .context("missing character state")?;
    let section = c.section;
    let mut play = c.play;
    let mut play_changed = false;
    let mut body = cx
        .component::<Body>(s.actor, BODY)?
        .context("missing character body")?
        .into_owned();
    let before_root = cx.local(s.actor)?.context("missing character root")?;
    let motion = *cx
        .component::<ProposedMotion>(s.actor, PROPOSED)?
        .context("missing motion proposal")?;
    motion.validate_after(
        cx.component::<ResolvedMotion>(s.actor, RESOLVED)?
            .as_deref(),
    )?;
    if let Some(velocity) = motion.inherited_velocity {
        body.reset_motion(before_root.w_axis.truncate());
        body.particle.momentum = velocity * body.particle.mass;
    }
    let request = motion.root;
    let policy = cx.motion_policy(s.actor, MotionPolicy::DYNAMIC)?;
    let mut plate_velocity_delta = 0.;
    let mut plate_height_delta = 0.;
    let start = before_root.w_axis.truncate();
    let target = request.target_local().w_axis.truncate();
    let predicted =
        start + body.particle.velocity() * dt - Vec3::Y * (0.5 * body.gravity * dt * dt);
    // Include both feet and the root sweep. During jump matching retain distant landing candidates.
    let margin = Vec3::new(
        body.capsule.radius.max(0.75),
        0.4,
        body.capsule.radius.max(0.75),
    );
    let mut area = Bounds {
        lo: start.min(target).min(predicted) - margin,
        hi: start.max(target).max(predicted) + margin + Vec3::Y * body.capsule.height,
    };
    if play.is_some_and(|p| p.launched && !p.landed) {
        let end = start + body.particle.velocity() * 3.;
        area.lo = area.lo.min(end - Vec3::Y * (4.5 * body.gravity) - margin);
        area.hi = area.hi.max(end + margin);
    }
    let mut candidates = if policy.physics != PhysicsParticipation::Off {
        query.near(s.actor, body.capsule, area)
    } else {
        vec![]
    };
    // A missing sample after hot rebinding is a discontinuity. Persisted face poses only seed the first snapshot after load.
    let saved_faces = if query.restore_history {
        body.previous_faces.as_slice()
    } else {
        &[]
    };
    let mut floors = support_faces(
        &candidates,
        s.binding.plate,
        plate,
        spring_active,
        saved_faces,
        dt,
    );
    if body.support.is_some_and(|id| {
        !candidates
            .iter()
            .any(|c| c.id == id && (c.previous.is_some() || query.restore_history))
    }) {
        body.support = None;
        body.base_velocity = Vec3::ZERO;
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
            step: motion.step,
            blocking_normal: None,
            before: before_root,
            request,
            policy,
            body,
            contact: None,
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
    if spring_active && body.support == Some(s.binding.plate) {
        base.y = 0.;
    }
    if body.support.is_some() {
        let carrier_impulse = (base - body.base_velocity) * body.particle.mass;
        body.particle.impulse(carrier_impulse);
    }
    let before = body.particle.position;
    let was_grounded = body.support.is_some();
    let mass = body.particle.mass;
    let launch = play
        .and_then(|p| section.launch(p))
        .filter(|_| was_grounded);
    let goal = request.target_local().w_axis.truncate();
    if was_grounded && let Some(play) = &mut play {
        let offset = play.ground_offset + base * Vec3::new(1., 0., 1.) * dt;
        play_changed |= offset != play.ground_offset;
        play.ground_offset = offset;
    }
    if let Some((speed, scale)) = launch {
        // A single push at takeoff. The authored span sets desired forward launch speed.
        let dv = play.unwrap().forward() * speed
            - (body.particle.velocity() - base) * Vec3::new(1., 0., 1.);
        let impulse = dv * mass + Vec3::Y * (body.jump_impulse * scale);
        if spring_active && body.support == Some(s.binding.plate) {
            plate_velocity_delta -= impulse.y / PLATE_MASS;
        }
        body.particle.impulse(impulse);
        body.support = None;
        body.flight_time = 0.;
        body.palm_impulse = 0.;
        play.as_mut().unwrap().launched = true;
        play_changed = true;
    } else if was_grounded {
        let desired = (goal - before) * Vec3::new(1., 0., 1.) / dt;
        let current = (body.particle.velocity() - base) * Vec3::new(1., 0., 1.);
        let impulse =
            ((desired - current) * mass).clamp_length_max(TRACTION * mass * body.gravity * dt);
        body.particle.impulse(impulse);
    }
    let mut acceleration = -body.gravity * Vec3::Y;
    if body.support.is_none()
        && let Some(grip) = motion.grip
        && query.permits_grip(grip.support, body.capsule.mask)
    {
        let lift = grip.fraction * body.gravity * grip.load;
        acceleration += Vec3::Y * lift;
        body.palm_impulse += mass * lift * dt;
    }
    body.particle.advance(acceleration, dt);
    // Carrier impulses and animation takeoff can send the capsule beyond the initial velocity prediction.
    let end = body.particle.position;
    let radius = Vec3::new(body.capsule.radius, 0., body.capsule.radius);
    let swept = Bounds {
        lo: before.min(end) - radius,
        hi: before.max(end) + radius + Vec3::Y * body.capsule.height,
    };
    if swept.lo.cmplt(area.lo).any() || swept.hi.cmpgt(area.hi).any() {
        candidates = query.near(s.actor, body.capsule, area.union(swept));
        floors = support_faces(
            &candidates,
            s.binding.plate,
            plate,
            spring_active,
            saved_faces,
            dt,
        );
    }
    let blocking_normal = collide_solids(&mut body, before, &candidates, dt);
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
        && let Some(play) = &mut play
        && play.launched
        && !play.landed
        && section == traversal::Section::Gap
    {
        let remaining = floors
            .iter()
            .filter_map(|f| f.landing_time(end, body.particle.velocity(), body.gravity))
            .min_by(f32::total_cmp);
        if let Some(remaining) = remaining {
            let old = play.frame;
            section.sample_flight(play, body.flight_time, remaining);
            play_changed |= play.frame != old;
        }
    }
    if let Some((fraction, surf, at)) = contact {
        let movable = spring_active && surf.id == s.binding.plate;
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
            if let Some(play) = &mut play
                && play.launched
                && !play.landed
            {
                section.land(play, body.particle.position);
                play_changed = true;
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
    if spring_active && body.support == Some(s.binding.plate) {
        body.base_velocity.y = 0.;
    }
    body.previous_faces.clear();
    body.previous_faces
        .extend(floors.iter().map(|s| (s.id, PreviousFace::of(*s))));
    Ok(Solved {
        step: motion.step,
        blocking_normal,
        before: before_root,
        request,
        policy,
        body,
        contact: play_changed.then(|| {
            let mut state = c.into_owned();
            state.play = play;
            state
        }),
        plate_velocity_delta,
        plate_height_delta,
        faces: floors,
    })
}
