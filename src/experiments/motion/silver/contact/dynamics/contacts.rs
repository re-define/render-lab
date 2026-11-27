//! Support-face motion and continuous capsule sweeps against solid boxes.
use super::*;
use crate::physics::{BoxSurface, sweep_capsule};

pub(super) fn support_faces(
    candidates: &[collision::Candidate],
    plate_id: Id,
    plate: Option<SupportFace>,
    spring_active: bool,
    previous_faces: &[(Id, PreviousFace)],
    dt: f32,
) -> Vec<SupportFace> {
    let mut floors = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let id = candidate.id;
        let mut face = if id == plate_id {
            let Some(face) = plate else { continue };
            face
        } else {
            candidate.face
        };
        let prior = candidate
            .prior_face
            .map(|(at, q)| (at, Some(q)))
            .or_else(|| {
                previous_faces
                    .iter()
                    .find(|(k, _)| *k == id)
                    .map(|(_, p)| p.pose())
            });
        if let Some((center, rotation)) = prior {
            face.set_previous(center, rotation, dt);
        }
        if let Some((rate, pivot, previous)) = candidate.drive {
            let velocity = previous.map_or(Vec3::ZERO, |old| (pivot - old) / dt);
            face.set_driven_rotation(rate, pivot, velocity);
        }
        if spring_active
            && id == plate_id
            && let Some(plate) = plate
        {
            face.velocity.y = plate.velocity.y;
        }
        floors.push(face);
    }
    floors
}

/// Clip the remaining translation at the first blocking face and continue along it.
pub(super) fn collide_solids(
    body: &mut Body,
    start: Vec3,
    boxes: &[collision::Candidate],
    dt: f32,
) -> Option<Vec3> {
    let mut at = start;
    let mut end = body.particle.position;
    let mut elapsed = 0.;
    let mut blocking = None;
    for _ in 0..4 {
        let hit = boxes
            .iter()
            .filter(|b| b.surface == BoxSurface::Solid)
            .filter_map(|b| {
                let delta = b.previous.map_or(Vec3::ZERO, |old| {
                    (b.transform.w_axis - old.w_axis).truncate()
                });
                let (t, n) =
                    sweep_capsule(body.capsule, at, end, b.transform, delta * (1. - elapsed))?;
                // Walkable top faces are solved once by the platform/landing impulse below.
                (n.y < 0.65 || n.dot(b.face.normal) < 0.9999).then_some((t, n, delta))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        let Some((time, n, delta)) = hit else { break };
        at = at.lerp(end, time);
        elapsed += (1. - elapsed) * time;
        let remaining = end - at;
        let inward = (remaining - delta * (1. - elapsed)).dot(n).min(0.);
        end = at + remaining - inward * n;
        body.particle.contact(n, delta / dt, 0., 0.);
        blocking = Some(n);
        if time >= 1. {
            break;
        }
    }
    body.particle.position = end;
    blocking
}
