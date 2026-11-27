//! Root movement authority and participation in physical queries are independent of rendering.
use super::*;

pub const MOTION_POLICY: &str = "engine.motion.policy";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mobility {
    Static,
    Movable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhysicsParticipation {
    Off,
    QueryOnly,
    Simulated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MotionPolicy {
    pub mobility: Mobility,
    pub physics: PhysicsParticipation,
}

impl MotionPolicy {
    pub const STATIC: Self = Self {
        mobility: Mobility::Static,
        physics: PhysicsParticipation::Off,
    };
    pub const FIXED_COLLIDER: Self = Self {
        mobility: Mobility::Static,
        physics: PhysicsParticipation::QueryOnly,
    };
    pub const MOVING_COLLIDER: Self = Self {
        mobility: Mobility::Movable,
        physics: PhysicsParticipation::QueryOnly,
    };
    pub const KINEMATIC: Self = Self {
        mobility: Mobility::Movable,
        physics: PhysicsParticipation::Off,
    };
    pub const DYNAMIC: Self = Self {
        mobility: Mobility::Movable,
        physics: PhysicsParticipation::Simulated,
    };

    pub fn validate(self) -> Result<()> {
        ensure!(
            self.mobility == Mobility::Movable || self.physics != PhysicsParticipation::Simulated,
            "a simulated body requires a movable root"
        );
        Ok(())
    }
}

/// A module's ownership of the local root. An animation provider may delegate to a physical one.
#[derive(Clone, Copy)]
pub(crate) struct RootWriter {
    pub component: &'static str,
    policy: MotionPolicy,
    delegate: Option<&'static str>,
}

impl RootWriter {
    pub const fn new(component: &'static str, policy: MotionPolicy) -> Self {
        Self {
            component,
            policy,
            delegate: None,
        }
    }
    pub const fn delegates_to(mut self, component: &'static str) -> Self {
        self.delegate = Some(component);
        self
    }
    pub(crate) fn active(self, entity: &Entity) -> bool {
        entity.components.contains_key(self.component)
            && !self
                .delegate
                .is_some_and(|other| entity.components.contains_key(other))
    }
}

impl World {
    /// A contact solver, when installed on an animated root, claims its final translation.
    pub(crate) fn root_provider(&self, entity: &Entity) -> Option<MotionPolicy> {
        self.motion_writers
            .iter()
            .find(|writer| writer.active(entity))
            .map(|writer| writer.policy)
    }

    /// Explicit scene data overrides the default supplied by the system owning this entity.
    pub fn motion_policy(&self, id: Id, fallback: MotionPolicy) -> Result<MotionPolicy> {
        let entity = self
            .entities
            .get(&id)
            .context("motion entity does not exist")?;
        let policy = match entity.components.read::<MotionPolicy>(MOTION_POLICY)? {
            Some(value) => *value,
            None => fallback,
        };
        policy.validate()?;
        Ok(policy)
    }

    pub fn set_motion_policy(&mut self, id: Id, policy: MotionPolicy) -> Result<()> {
        policy.validate()?;
        let entity = self
            .entities
            .get(&id)
            .context("motion entity does not exist")?;
        if entity.components.contains_key(MOTION_POLICY) {
            let previous = self.motion_policy(id, policy)?;
            if previous == policy {
                return Ok(());
            }
            // Transfer the live parent-space velocity between the two movable linear solvers.
            // Initial authoring and a static interval preserve the explicitly authored command.
            if previous.mobility == Mobility::Movable
                && policy.mobility == Mobility::Movable
                && (previous.physics == PhysicsParticipation::Simulated)
                    != (policy.physics == PhysicsParticipation::Simulated)
                && let Some(mut body) = self.linear_body(id)?
            {
                if policy.physics == PhysicsParticipation::Simulated {
                    body.momentum = body.target_velocity * body.mass;
                } else {
                    body.target_velocity = body.momentum / body.mass;
                }
                self.set_linear_body(id, body)?;
            }
        }
        self.set_typed_component(id, MOTION_POLICY, policy);
        Ok(())
    }
}
