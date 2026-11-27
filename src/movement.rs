//! Per-entity control input and the handoff from proposed to resolved root motion.
pub mod character;
pub mod linear;
use crate::scene::{Id, Mobility, MotionPolicy, PhysicsParticipation, World};
use anyhow::{Context, Result, ensure};
use glam::{Mat4, Quat, Vec3};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MoveIntent {
    pub side: i8,
    pub forward: i8,
}

/// Normalized local input axes supplied by replay, gamepad, AI, or the keyboard adapter.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AnalogMove {
    pub side: f32,
    pub forward: f32,
}
impl From<MoveIntent> for AnalogMove {
    fn from(keys: MoveIntent) -> Self {
        Self {
            side: f32::from(keys.side),
            forward: f32::from(keys.forward),
        }
    }
}

#[derive(Clone, Copy)]
pub struct MovementDriver {
    pub component: &'static str,
    pub camera_follows: fn(&World, Id) -> bool,
    pub active: fn(&World, Id) -> Result<bool>,
    pub apply: fn(&mut World, Id, AnalogMove) -> Result<()>,
}

#[derive(Default)]
pub struct MovementControls {
    drivers: Vec<MovementDriver>,
}
impl MovementControls {
    pub fn new(driver: Option<MovementDriver>) -> Self {
        Self {
            drivers: driver.into_iter().collect(),
        }
    }
    pub fn register(&mut self, driver: MovementDriver) -> Result<()> {
        ensure!(
            !self
                .drivers
                .iter()
                .any(|other| other.component == driver.component),
            "duplicate movement driver for {}",
            driver.component
        );
        self.drivers.push(driver);
        Ok(())
    }
    pub fn target(&self, world: &World) -> Option<Id> {
        if let Some(target) = world.control_selection() {
            return target.filter(|&id| self.driver(world, id).is_ok());
        }
        self.drivers
            .iter()
            .filter_map(|d| {
                world
                    .entities
                    .component_owners(d.component)
                    .first()
                    .copied()
            })
            .min()
    }
    pub fn candidates(&self, world: &World) -> Vec<Id> {
        let mut ids: Vec<_> = self
            .drivers
            .iter()
            .flat_map(|d| world.entities.component_owners(d.component).iter().copied())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
    pub(crate) fn validate(&self, world: &World) -> Result<()> {
        if self.drivers.len() > 1 {
            for id in self.candidates(world) {
                self.driver(world, id)?;
            }
        }
        Ok(())
    }
    fn driver(&self, world: &World, id: Id) -> Result<&MovementDriver> {
        let e = world
            .entities
            .get(&id)
            .context("missing controlled entity")?;
        let mut matching = self
            .drivers
            .iter()
            .filter(|d| e.components.contains_key(d.component));
        let driver = matching
            .next()
            .with_context(|| format!("entity {id} has no registered movement driver"))?;
        ensure!(
            matching.next().is_none(),
            "entity {id} has ambiguous movement drivers"
        );
        Ok(driver)
    }
    pub fn select(&mut self, world: &mut World, id: Id) -> Result<()> {
        self.driver(world, id)?;
        if let Some(old) = self.target(world)
            && old != id
            && self.active(world, old)?
        {
            self.apply_analog(world, old, AnalogMove::default())?;
        }
        world.select_control_target(Some(id));
        Ok(())
    }
    pub fn camera_follows(&self, world: &World, id: Id) -> bool {
        self.driver(world, id)
            .is_ok_and(|d| (d.camera_follows)(world, id))
    }
    pub fn active(&self, world: &World, id: Id) -> Result<bool> {
        (self.driver(world, id)?.active)(world, id)
    }
    pub fn apply(&self, world: &mut World, id: Id, keys: MoveIntent) -> Result<()> {
        self.apply_analog(world, id, keys.into())
    }
    pub fn apply_analog(&self, world: &mut World, id: Id, intent: AnalogMove) -> Result<()> {
        ensure!(
            (-1.0..=1.0).contains(&intent.side) && (-1.0..=1.0).contains(&intent.forward),
            "movement axes must be finite and in -1..=1"
        );
        (self.driver(world, id)?.apply)(world, id, intent)
    }
}

/// Desired end-of-substep transform in the parent frame (metres). Producers do not write
/// the entity transform. A simulated translation must come from that substep's solver.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct RootMotionRequest(Mat4);

/// Physical translation in parent space and optional rotation from an angular solver.
#[derive(Clone, Copy)]
pub struct SolvedRoot {
    pub position: Vec3,
    pub rotation: Option<Quat>,
}

impl RootMotionRequest {
    pub fn new(target_local: Mat4) -> Result<Self> {
        ensure!(target_local.is_finite(), "non-finite root motion target");
        Ok(Self(target_local))
    }

    pub fn target_local(self) -> Mat4 {
        self.0
    }

    pub fn resolve(
        self,
        policy: MotionPolicy,
        current_local: Mat4,
        simulated_position: Option<Vec3>,
    ) -> Result<Mat4> {
        self.resolve_body(
            policy,
            current_local,
            simulated_position.map(|position| SolvedRoot {
                position,
                rotation: None,
            }),
        )
    }
    pub fn resolve_body(
        self,
        policy: MotionPolicy,
        current_local: Mat4,
        solved: Option<SolvedRoot>,
    ) -> Result<Mat4> {
        ensure!(current_local.is_finite(), "non-finite current root");
        policy.validate()?;
        if policy.physics == PhysicsParticipation::Simulated {
            let solved = solved.context("simulated root motion requires a physics solver")?;
            ensure!(
                solved.position.is_finite(),
                "non-finite simulated root position"
            );
            if let Some(rotation) = solved.rotation {
                ensure!(
                    rotation.is_finite() && rotation.is_normalized(),
                    "invalid simulated root rotation"
                );
                let determinant = self.0.determinant();
                ensure!(
                    determinant.is_finite() && determinant != 0.,
                    "singular rigid root transform"
                );
                let (scale, _, _) = self.0.to_scale_rotation_translation();
                ensure!(scale.is_finite(), "invalid rigid root scale");
                return Ok(Mat4::from_scale_rotation_translation(
                    scale,
                    rotation,
                    solved.position,
                ));
            }
            let mut resolved = self.0;
            resolved.w_axis = solved.position.extend(1.);
            Ok(resolved)
        } else {
            ensure!(
                solved.is_none(),
                "physics cannot submit a translation for a nonsimulated root"
            );
            Ok(if policy.mobility == Mobility::Static {
                current_local
            } else {
                self.0
            })
        }
    }
}
