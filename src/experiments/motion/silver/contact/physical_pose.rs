use super::*;
use crate::physics::{
    SupportFace,
    articulation::{LimbDrive, LimbIsland},
};

/// Partial physical animation. The fixed root of each island follows its animation
/// socket, so this layer does not compete with the controller for root ownership.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct PhysicalPose {
    weight: f32,
    limbs: Option<[LimbIsland; 3]>,
    #[serde(default, skip_serializing_if = "default_drives")]
    drives: [LimbDrive; 3],
}

mod controls;
fn default_drives(drives: &[LimbDrive; 3]) -> bool {
    *drives == [LimbDrive::default(); 3]
}

impl PhysicalPose {
    pub fn active(&self) -> bool {
        self.weight > 0.
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (0.0..=1.0).contains(&self.weight),
            "invalid physical pose weight"
        );
        for drive in self.drives {
            drive.validate()?;
        }
        Ok(())
    }
    pub fn reset(&mut self) {
        self.limbs = None;
    }
    pub fn apply(
        &mut self,
        rig: &Rig,
        pose: &mut rig::Pose<'_>,
        gravity: Vec3,
        faces: &[SupportFace],
        dt: f32,
    ) -> Result<()> {
        let leg = |[thigh, knee, ankle, _]: [usize; 4]| [thigh, knee, ankle];
        let chains = [rig.arm, leg(rig.legs[0]), leg(rig.legs[1])];
        let targets = chains.map(|chain| chain.map(|joint| pose.position(joint)));
        let limbs = self
            .limbs
            .get_or_insert_with(|| targets.map(LimbIsland::new));
        for (i, (([a, b, c], goal), limb)) in chains.into_iter().zip(targets).zip(limbs).enumerate()
        {
            let solved = limb.advance_with_drive(goal, gravity, faces, dt, self.drives[i])?;
            pose.aim(a, b, goal[1].lerp(solved[1], self.weight));
            pose.aim(b, c, goal[2].lerp(solved[2], self.weight));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
