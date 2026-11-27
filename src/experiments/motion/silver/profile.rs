//! Named animation assets used when a player does not override its semantic rig or action timings.
use super::*;
use std::{collections::BTreeMap, sync::LazyLock};

#[derive(Deserialize)]
pub(super) struct Profile {
    pub clips: BTreeMap<String, search::Candidate>,
    pub rig: RigMap,
    pub actions: Actions,
}

pub(super) static DEFAULT: LazyLock<Profile> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../../assets/motion/character-profile.json"
    ))
    .expect("invalid bundled character profile")
});

/// Slots address the retargeted bank channels, not entity names or runtime draw slots.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct RigMap {
    pub pelvis: usize,
    pub right_leg: [usize; 4],
    pub left_leg: [usize; 4],
    pub reach_arm: [usize; 3],
    pub fingertip_name: String,
    pub fingertip_offset: Vec3,
    pub palm: PalmMarker,
    /// Source parent -> destination parent, and destination joint -> source joint bind-space rotations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub basis: Vec<ChannelBasis>,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(super) struct PalmMarker {
    pub offset: Vec3,
    pub fingers: Vec3,
    pub normal: Vec3,
}
impl PalmMarker {
    pub fn frame(&self) -> Result<glam::Mat3> {
        let fingers = self
            .fingers
            .try_normalize()
            .context("invalid palm direction")?;
        let tangent = fingers
            .cross(self.normal)
            .try_normalize()
            .context("invalid palm contact normal")?;
        ensure!(self.offset.is_finite(), "invalid palm offset");
        Ok(glam::Mat3::from_cols(
            tangent,
            fingers,
            tangent.cross(fingers),
        ))
    }
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(super) struct ChannelBasis {
    pub parent: Quat,
    pub joint: Quat,
}
impl Default for ChannelBasis {
    fn default() -> Self {
        Self {
            parent: Quat::IDENTITY,
            joint: Quat::IDENTITY,
        }
    }
}
impl RigMap {
    pub fn validate(&self, count: usize) -> Result<()> {
        self.palm.frame()?;
        let mut assigned = std::collections::BTreeSet::new();
        let channels = std::iter::once(self.pelvis)
            .chain(self.right_leg)
            .chain(self.left_leg)
            .chain(self.reach_arm);
        ensure!(
            channels
                .into_iter()
                .all(|slot| slot < count && assigned.insert(slot)),
            "invalid or overlapping semantic channels"
        );
        ensure!(
            self.fingertip_offset.is_finite()
                && (self.basis.is_empty()
                    || self.basis.len() == count
                        && self
                            .basis
                            .iter()
                            .all(|b| b.parent.is_normalized() && b.joint.is_normalized())),
            "invalid skeleton bind-space correction"
        );
        Ok(())
    }
    pub fn rotation(&self, channel: usize, q: Quat) -> Quat {
        self.basis
            .get(channel)
            .map_or(q, |b| (b.parent * q * b.joint).normalize())
    }
    pub fn hip_offset(&self, delta: Vec3) -> Vec3 {
        self.basis
            .get(self.pelvis)
            .map_or(delta, |b| b.parent * delta)
    }
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RootSource {
    #[default]
    Controller,
    Clip,
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Action {
    pub flight: Option<Flight>,
    /// Start, fully planted, start of release, end in source frames at 30 Hz.
    pub palm: Option<[f32; 4]>,
}
impl Action {
    pub fn validate(self, end: f32) -> Result<()> {
        if let Some(f) = self.flight {
            ensure!(
                f.takeoff > 0.
                    && f.takeoff < f.landing
                    && f.landing <= end
                    && f.impulse_scale.is_finite()
                    && f.impulse_scale > 0.,
                "invalid flight event"
            );
        }
        if let Some([a, b, c, d]) = self.palm {
            ensure!(
                a >= 0. && a < b && b <= c && c < d && d <= end,
                "invalid palm contact window"
            );
        }
        Ok(())
    }
    pub fn palm_weight(self, frame: f32) -> f32 {
        let Some([start, planted, release, end]) = self.palm else {
            return 0.;
        };
        let smooth = |t: f32| {
            let t = t.clamp(0., 1.);
            t * t * (3. - 2. * t)
        };
        smooth((frame - start) / (planted - start)) * smooth((end - frame) / (end - release))
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(super) struct Flight {
    pub takeoff: f32,
    pub landing: f32,
    pub impulse_scale: f32,
}
impl Flight {
    pub fn crossed_takeoff(self, previous: f32, current: f32) -> bool {
        previous < self.takeoff && current >= self.takeoff
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Actions {
    pub jump: Action,
    pub vault: Action,
    pub duck: Action,
}
