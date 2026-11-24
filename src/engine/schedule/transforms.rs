use super::{SimulationData, SystemAccess};
use crate::scene::{Id, World};
use anyhow::Result;
use serde::de::DeserializeOwned;
use std::borrow::Cow;

/// Entity IDs for a system's LocalTransformsById declaration, in each direction.
/// Order and duplicate IDs are insignificant.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransformTargets {
    pub reads: Vec<Id>,
    pub writes: Vec<Id>,
}

impl TransformTargets {
    pub(super) fn normalize(&mut self) {
        for ids in [&mut self.reads, &mut self.writes] {
            ids.sort_unstable();
            ids.dedup();
        }
    }
    pub(super) fn ids(&self, writing: bool) -> &[Id] {
        if writing { &self.writes } else { &self.reads }
    }
}

/// Resolve pose bindings from declared components without reading or changing poses.
pub struct TransformBindings<'w> {
    pub(super) world: &'w World,
    pub(super) access: SystemAccess,
}
impl<'w> TransformBindings<'w> {
    pub fn component_owners(&self, key: &'static str) -> Result<&'w [Id]> {
        self.access.require(SimulationData::Component(key), false)?;
        Ok(self.world.entities.component_owners(key))
    }
    pub fn component<T: Clone + DeserializeOwned + 'static>(
        &self,
        id: Id,
        key: &'static str,
    ) -> Result<Option<Cow<'w, T>>> {
        self.access.require(SimulationData::Component(key), false)?;
        self.world.typed_component(id, key)
    }
}

/// None is the whole column (also used by the conservative plan before binding).
/// An empty slice has no conflicts. Return one witness ID for the schedule report.
pub(super) fn overlap(a: Option<&[Id]>, b: Option<&[Id]>) -> Option<Option<Id>> {
    match (a, b) {
        (None, None) => Some(None),
        (Some(ids), None) | (None, Some(ids)) => ids.first().copied().map(Some),
        (Some(a), Some(b)) => {
            let (mut i, mut j) = (0, 0);
            while i < a.len() && j < b.len() {
                match a[i].cmp(&b[j]) {
                    std::cmp::Ordering::Less => i += 1,
                    std::cmp::Ordering::Greater => j += 1,
                    std::cmp::Ordering::Equal => return Some(Some(a[i])),
                }
            }
            None
        }
    }
}
