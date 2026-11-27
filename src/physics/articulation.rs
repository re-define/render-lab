//! An anchored two-link constraint island for physical animation. Positions use metres
//! in the character parent's frame. The two dynamic joints each have unit inverse mass.
use super::SupportFace;
use anyhow::{Result, ensure};
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
            return Ok(targets);
        }
        let steps = (dt * 120.).ceil().max(1.) as u32;
        let h = dt / steps as f32;
        // Metres per newton for a one kg joint driven toward the animation.
        let alpha = 0.008 / (h * h);
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            let goals = std::array::from_fn::<_, 3, _>(|i| self.last_target[i].lerp(targets[i], t));
            let lengths = [goals[0].distance(goals[1]), goals[1].distance(goals[2])];
            let previous = self.position;
            self.position[0] = goals[0];
            let mut multipliers = [Vec3::ZERO; 3];
            for i in 1..3 {
                self.velocity[i] = self.velocity[i] * (-5.0 * h).exp() + gravity * h;
                self.position[i] += self.velocity[i] * h;
            }
            for _ in 0..8 {
                for i in 1..3 {
                    let delta =
                        (goals[i] - self.position[i] - alpha * multipliers[i]) / (1. + alpha);
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
mod tests {
    use super::*;

    #[test]
    fn unilateral_contact_catches_a_falling_wrist_and_leaves_overhead_platforms_alone() -> Result<()>
    {
        let pose = [
            Vec3::Y * 0.06,
            Vec3::new(0.3, 0.06, 0.),
            Vec3::new(0.6, 0.06, 0.),
        ];
        let platform = |y: f32| {
            SupportFace::from_box(
                1,
                glam::Mat4::from_scale_rotation_translation(
                    Vec3::new(3., 0.1, 3.),
                    glam::Quat::IDENTITY,
                    Vec3::Y * (y - 0.05),
                ),
                0.65,
            )
            .unwrap()
        };
        let mut grounded = LimbIsland::new(pose);
        let mut below = LimbIsland::new(pose);
        let mut free = LimbIsland::new(pose);
        for _ in 0..120 {
            let on_ground =
                grounded.advance(pose, Vec3::NEG_Y * 9.81, &[platform(0.)], 1. / 60.)?;
            let under = below.advance(pose, Vec3::NEG_Y * 9.81, &[platform(2.)], 1. / 60.)?;
            let without = free.advance(pose, Vec3::NEG_Y * 9.81, &[], 1. / 60.)?;
            assert!(on_ground[2].y >= 0.0149);
            assert_eq!(under, without);
        }
        assert!((grounded.position[2].y - 0.015).abs() < 0.001);
        assert!(free.position[2].y < 0.);
        Ok(())
    }

    #[test]
    fn limb_lengths_and_target_drives_remain_stable_after_reload() -> Result<()> {
        let pose = [Vec3::Y, Vec3::Y + 0.35 * Vec3::X, Vec3::Y + 0.7 * Vec3::X];
        let mut at_60 = LimbIsland::new(pose);
        let mut at_120 = at_60.clone();
        for _ in 0..120 {
            at_60.advance(pose, Vec3::NEG_Y * 9.81, &[], 1. / 60.)?;
        }
        for _ in 0..240 {
            at_120.advance(pose, Vec3::NEG_Y * 9.81, &[], 1. / 120.)?;
        }
        for i in 1..3 {
            assert!((at_60.position[i - 1].distance(at_60.position[i]) - 0.35).abs() < 0.001);
            assert!(at_60.position[i].abs_diff_eq(at_120.position[i], 1e-4));
        }
        assert!(pose[2].y - at_60.position[2].y > 0.02);
        let mut loaded: LimbIsland = serde_json::from_str(&serde_json::to_string(&at_60)?)?;
        let raised = pose.map(|p| p + 0.1 * Vec3::Y);
        assert_eq!(
            at_60.advance(raised, Vec3::NEG_Y * 9.81, &[], 1. / 60.)?,
            loaded.advance(raised, Vec3::NEG_Y * 9.81, &[], 1. / 60.)?
        );
        Ok(())
    }
}
