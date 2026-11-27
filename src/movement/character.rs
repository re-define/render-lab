//! One exchange per fixed character substep. Transforms, velocities and normals all use
//! the character parent's frame. A completed exchange is sufficient to resume from a save.
use super::RootMotionRequest;
use crate::{physics::SupportFace, scene::Id};
use anyhow::{Context, Result, ensure};
use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const PROPOSED: &str = "engine.character.proposed_motion";
pub const RESOLVED: &str = "engine.character.resolved_motion";

/// The authorized fraction of the character's weight carried by a named support.
/// Visual arm placement can be disabled independently.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct GripSupport {
    pub support: Id,
    pub load: f32,
    pub fraction: f32,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct ProposedMotion {
    pub step: u64,
    pub root: RootMotionRequest,
    pub inherited_velocity: Option<Vec3>,
    pub grip: Option<GripSupport>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ResolvedMotion {
    pub step: u64,
    pub root_local: Mat4,
    /// Displacement actually achieved during the substep, divided by dt (metres/second).
    pub velocity: Vec3,
    /// Acceleration in the parent frame available to secondary limb dynamics.
    #[serde(default)]
    pub gravity: Vec3,
    pub support: Option<Id>,
    pub support_velocity: Vec3,
    pub blocking_normal: Option<Vec3>,
    /// Reconstructed by collision detection before post-physics IK, including after loading.
    #[serde(skip)]
    pub surfaces: Arc<[SupportFace]>,
}

impl ProposedMotion {
    pub fn next(
        previous: Option<&ResolvedMotion>,
        root: RootMotionRequest,
        inherited_velocity: Option<Vec3>,
        grip: Option<GripSupport>,
    ) -> Result<Self> {
        let step = previous
            .map_or(0, |state| state.step)
            .checked_add(1)
            .context("character substep overflow")?;
        let proposal = Self {
            step,
            root,
            inherited_velocity,
            grip,
        };
        proposal.validate_after(previous)?;
        Ok(proposal)
    }
    pub fn validate_after(&self, previous: Option<&ResolvedMotion>) -> Result<()> {
        ensure!(
            previous.map_or(0, |state| state.step).checked_add(1) == Some(self.step),
            "stale or missing character motion proposal"
        );
        ensure!(
            self.root.target_local().is_finite()
                && self.inherited_velocity.is_none_or(|v| v.is_finite()),
            "invalid proposed motion"
        );
        ensure!(
            self.grip
                .is_none_or(|g| (0.0..=1.0).contains(&g.load) && (0.0..=1.0).contains(&g.fraction)),
            "invalid grip support"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_consumes_its_numbered_proposal_once() -> Result<()> {
        let mut previous = ResolvedMotion {
            step: 3,
            root_local: Mat4::IDENTITY,
            velocity: Vec3::ZERO,
            gravity: Vec3::NEG_Y * 9.81,
            support: None,
            support_velocity: Vec3::ZERO,
            blocking_normal: None,
            surfaces: Arc::default(),
        };
        let root = RootMotionRequest::new(Mat4::IDENTITY)?;
        let next = ProposedMotion::next(Some(&previous), root, None, None)?;
        assert_eq!(next.step, 4);
        assert!(next.validate_after(None).is_err());
        previous.step = 4;
        assert!(next.validate_after(Some(&previous)).is_err());
        previous.step = u64::MAX;
        assert!(ProposedMotion::next(Some(&previous), root, None, None).is_err());
        assert!(ProposedMotion::next(None, root, Some(Vec3::NAN), None).is_err());
        assert!(
            ProposedMotion::next(
                None,
                root,
                None,
                Some(GripSupport {
                    support: 1,
                    load: 0.5,
                    fraction: 1.5
                })
            )
            .is_err()
        );
        Ok(())
    }
}
