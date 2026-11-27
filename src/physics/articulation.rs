//! An anchored two-link constraint island for physical animation. Positions use metres
//! in the character parent's frame. The two dynamic joints each have unit inverse mass.
mod drive;
use super::SupportFace;
use anyhow::{Result, ensure};
pub use drive::{BendRange, LimbDrive};
use glam::Vec3;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct LimbIsland {
    position: [Vec3; 3],
    velocity: [Vec3; 3],
    last_target: [Vec3; 3],
}

impl LimbIsland {
    pub fn new(pose: [Vec3; 3]) -> Self {
        Self {
            position: pose,
            velocity: [Vec3::ZERO; 3],
            last_target: pose,
        }
    }

    /// XPBD position drives and fixed-length links, with unilateral contact against
    /// nearby walkable surfaces. Interpolate the moving anchor through internal substeps.
    pub fn advance(
        &mut self,
        targets: [Vec3; 3],
        gravity: Vec3,
        faces: &[SupportFace],
        dt: f32,
    ) -> Result<[Vec3; 3]> {
        self.advance_with_drive(targets, gravity, faces, dt, LimbDrive::default())
    }

    pub fn flexion_degrees(&self) -> Option<f32> {
        let [a, b, c] = self.position;
        let upper = (b - a).try_normalize()?;
        let lower = (c - b).try_normalize()?;
        Some(
            upper
                .cross(lower)
                .length()
                .atan2(upper.dot(lower))
                .to_degrees(),
        )
    }

    pub fn advance_with_drive(
        &mut self,
        targets: [Vec3; 3],
        gravity: Vec3,
        faces: &[SupportFace],
        dt: f32,
        drive: LimbDrive,
    ) -> Result<[Vec3; 3]> {
        drive.validate()?;
        let limit = drive.bend.map(BendRange::projection);
        ensure!(
            dt.is_finite()
                && dt > 0.
                && gravity.is_finite()
                && targets
                    .iter()
                    .chain(&self.position)
                    .chain(&self.velocity)
                    .chain(&self.last_target)
                    .all(|p| p.is_finite()),
            "invalid physical animation state"
        );
        if dt > 0.25 || self.last_target[0].distance(targets[0]) > 1.0 {
            *self = Self::new(targets);
            if let Some(limit) = &limit {
                limit.apply(&mut self.position, targets, targets);
            }
            return Ok(self.position);
        }
        let steps = (dt * 120.).ceil().max(1.) as u32;
        let h = dt / steps as f32;
        // Metres per newton for a one kg joint driven toward the animation.
        let alpha = if drive.compliance == 0. {
            0.
        } else {
            drive.compliance / (h * h)
        };
        let drag = (-drive.damping * h).exp();
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            let goals = std::array::from_fn::<_, 3, _>(|i| self.last_target[i].lerp(targets[i], t));
            let lengths = [goals[0].distance(goals[1]), goals[1].distance(goals[2])];
            let previous = self.position;
            self.position[0] = goals[0];
            let mut multipliers = [Vec3::ZERO; 3];
            for i in 1..3 {
                self.velocity[i] = self.velocity[i] * drag + gravity * h;
                self.position[i] += self.velocity[i] * h;
            }
            for _ in 0..8 {
                for i in 1..3 {
                    let delta = if alpha.is_finite() {
                        (goals[i] - self.position[i] - alpha * multipliers[i]) / (1. + alpha)
                    } else {
                        // Limiting form as the substep approaches zero.
                        -multipliers[i]
                    };
                    self.position[i] += delta;
                    multipliers[i] += delta;
                    let link = self.position[i] - self.position[i - 1];
                    if let Some(axis) = link.try_normalize() {
                        let correction = axis * (link.length() - lengths[i - 1]);
                        if i == 1 {
                            self.position[i] -= correction;
                        } else {
                            self.position[i - 1] += correction * 0.5;
                            self.position[i] -= correction * 0.5;
                        }
                    }
                    if i == 2
                        && let Some(limit) = &limit
                    {
                        limit.apply(&mut self.position, previous, goals);
                    }
                    for face in faces {
                        if let Some(point) = face.below(self.position[i]) {
                            let separation = (self.position[i] - point).dot(face.normal);
                            // Approach the finite face from above. A hand under a platform stays below it.
                            if separation < 0.015
                                && (previous[i] - point).dot(face.normal) >= -0.015
                            {
                                self.position[i] += face.normal * (0.015 - separation);
                            }
                        }
                    }
                }
            }
            for (i, last) in previous.iter().enumerate().skip(1) {
                self.velocity[i] = (self.position[i] - *last) / h;
            }
        }
        self.last_target = targets;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests;
