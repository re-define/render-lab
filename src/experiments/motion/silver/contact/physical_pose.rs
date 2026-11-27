use super::*;
use crate::physics::{SupportFace, articulation::LimbIsland};

/// Partial physical animation. The fixed root of each island follows its animation
/// socket, so this layer does not compete with the controller for root ownership.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct PhysicalPose {
    weight: f32,
    limbs: Option<[LimbIsland; 3]>,
}

impl PhysicalPose {
    pub fn configure(layer: &mut Option<Arc<Self>>, arg: &str) -> Result<()> {
        let weight = match arg {
            "off" => 0.,
            "on" => 1.,
            other => other.parse()?,
        };
        ensure!(
            (0.0..=1.0).contains(&weight),
            "limb blend weight must be in 0..=1"
        );
        if weight == 0. {
            *layer = None;
        } else if let Some(current) = layer {
            Arc::make_mut(current).weight = weight;
        } else {
            *layer = Some(Arc::new(Self {
                weight,
                limbs: None,
            }));
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (0.0..=1.0).contains(&self.weight),
            "invalid physical pose weight"
        );
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
        for (([a, b, c], goal), limb) in chains.into_iter().zip(targets).zip(limbs) {
            let solved = limb.advance(goal, gravity, faces, dt)?;
            pose.aim(a, b, goal[1].lerp(solved[1], self.weight));
            pose.aim(b, c, goal[2].lerp(solved[2], self.weight));
        }
        Ok(())
    }
}
