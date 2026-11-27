//! Per-entity control input and the handoff from proposed to resolved root motion.
pub mod linear;
use crate::scene::{Id, Mobility, MotionPolicy, PhysicsParticipation, World};
use anyhow::{Context, Result, ensure};
use glam::{Mat4, Vec3};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MoveIntent {
    pub side: i8,
    pub forward: i8,
}

#[derive(Clone, Copy)]
pub struct MovementDriver {
    pub component: &'static str,
    pub camera_follows: fn(&World, Id) -> bool,
    pub active: fn(&World, Id) -> Result<bool>,
    pub apply: fn(&mut World, Id, MoveIntent) -> Result<()>,
}

#[derive(Default)]
pub struct MovementControls {
    driver: Option<MovementDriver>,
    target: Option<Id>,
}
impl MovementControls {
    pub fn new(driver: Option<MovementDriver>) -> Self {
        Self {
            driver,
            target: None,
        }
    }
    pub(crate) fn rebind(&mut self, driver: Option<MovementDriver>) {
        if self.driver.map(|d| d.component) != driver.map(|d| d.component) {
            self.target = None;
        }
        self.driver = driver;
    }
    pub fn target(&self, world: &World) -> Option<Id> {
        let owners = self.candidates(world);
        match self.target {
            Some(id) => owners.binary_search(&id).ok().map(|_| id),
            None => owners.first().copied(),
        }
    }
    pub fn candidates<'w>(&self, world: &'w World) -> &'w [Id] {
        self.driver
            .map_or(&[], |d| world.entities.component_owners(d.component))
    }

    pub fn select(&mut self, world: &mut World, id: Id) -> Result<()> {
        let driver = self.driver.context("scene has no movement driver")?;
        ensure!(
            world
                .entities
                .component_owners(driver.component)
                .binary_search(&id)
                .is_ok(),
            "entity {id} has no {} controller",
            driver.component
        );
        if let Some(old) = self.target(world)
            && old != id
            && (driver.active)(world, old)?
        {
            (driver.apply)(world, old, MoveIntent::default())?;
        }
        self.target = Some(id);
        Ok(())
    }
    pub fn camera_follows(&self, world: &World, id: Id) -> bool {
        self.driver.is_some_and(|d| (d.camera_follows)(world, id))
    }
    pub fn active(&self, world: &World, id: Id) -> Result<bool> {
        (self.driver.context("scene has no movement driver")?.active)(world, id)
    }
    /// Used by window input, replay, and headless tools. A deleted target is rejected.
    pub fn apply(&self, world: &mut World, id: Id, intent: MoveIntent) -> Result<()> {
        let driver = self.driver.context("scene has no movement driver")?;
        ensure!(
            world
                .entities
                .component_owners(driver.component)
                .binary_search(&id)
                .is_ok(),
            "entity {id} has no {} controller",
            driver.component
        );
        ensure!(
            (-1..=1).contains(&intent.side) && (-1..=1).contains(&intent.forward),
            "movement axes must be in -1..=1"
        );
        (driver.apply)(world, id, intent)
    }
}

/// Desired end-of-substep transform in the parent frame (metres). Producers do not write
/// the entity transform. A simulated translation must come from that substep's solver.
#[derive(Clone, Copy)]
pub struct RootMotionRequest(Mat4);

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
        ensure!(current_local.is_finite(), "non-finite current root");
        policy.validate()?;
        if policy.physics == PhysicsParticipation::Simulated {
            let position =
                simulated_position.context("simulated root motion requires a physics solver")?;
            ensure!(position.is_finite(), "non-finite simulated root position");
            let mut resolved = self.0;
            resolved.w_axis = position.extend(1.);
            Ok(resolved)
        } else {
            ensure!(
                simulated_position.is_none(),
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
