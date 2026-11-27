use super::{Edit, SimulationData, SystemAccess, TransformTargets};
use crate::scene::{Id, LocalPose, PoseLayout, World};
use anyhow::{Context, Result, ensure};
use glam::Mat4;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::borrow::Cow;

pub(super) enum Change {
    Pose(LocalPose),
    PublishedPose(Id, PoseLayout, Vec<Mat4>),
    Component(Id, &'static str, Edit),
    Resource(
        &'static str,
        std::sync::Arc<dyn std::any::Any + Send + Sync>,
    ),
    Whole(Edit),
}

impl Change {
    pub(super) fn apply(self, world: &mut World, undo: Option<&mut Vec<Edit>>) {
        match self {
            Self::Pose(pose) => {
                if let Some(undo) = undo {
                    let previous = pose.previous(world);
                    undo.push(Box::new(move |world| previous.apply(world)));
                }
                pose.apply(world);
            }
            Self::PublishedPose(owner, joints, locals) => {
                if let Some(undo) = undo {
                    let previous = joints
                        .joints()
                        .iter()
                        .filter_map(|id| world.entities.get(id).map(|row| (*id, row.local)))
                        .collect::<Vec<_>>();
                    undo.push(Box::new(move |world| {
                        for (id, local) in previous {
                            let _ = world.set_local(id, local);
                        }
                    }));
                }
                world.entities.publish_pose(owner, joints, locals);
            }
            Self::Component(id, key, edit) => {
                // A preceding whole-world command in this batch may have removed the entity.
                if !world.entities.contains_key(&id) {
                    return;
                }
                if let Some(undo) = undo {
                    undo.push(Box::new(world.restore_component_on_rollback(id, key)));
                }
                edit(world);
            }
            Self::Resource(key, value) => {
                let old = world.simulation_resources.insert(key, value);
                if let Some(undo) = undo {
                    undo.push(Box::new(move |world| match old {
                        Some(value) => {
                            world.simulation_resources.insert(key, value);
                        }
                        None => {
                            world.simulation_resources.remove(key);
                        }
                    }));
                }
            }
            Self::Whole(edit) => {
                if let Some(undo) = undo {
                    let previous = world.clone();
                    undo.push(Box::new(move |world| *world = previous));
                }
                edit(world);
            }
        }
    }
}

/// A system can only inspect its declared inputs and enqueue writes to its declared outputs.
pub struct SystemContext<'w> {
    world: &'w World,
    access: SystemAccess,
    targets: Option<&'w TransformTargets>,
    changes: Vec<Change>,
}

impl<'w> SystemContext<'w> {
    pub(super) fn new(
        world: &'w World,
        access: SystemAccess,
        targets: Option<&'w TransformTargets>,
    ) -> Self {
        Self {
            world,
            access,
            targets,
            changes: Vec::new(),
        }
    }
    pub(super) fn finish(self) -> Vec<Change> {
        self.changes
    }

    /// Evaluate independently against the same immutable substep snapshot. An error
    /// discards the whole group. Successful edit lists retain input order on commit.
    pub fn for_each_parallel<T: Sync>(
        &mut self,
        items: &[T],
        f: impl Fn(&T, &mut SystemContext<'w>) -> Result<()> + Sync,
    ) -> Result<()> {
        use rayon::prelude::*;
        if items.len() <= 1 {
            let checkpoint = self.changes.len();
            if let Some(one) = items.first()
                && let Err(err) = f(one, self)
            {
                self.changes.truncate(checkpoint);
                return Err(err);
            }
            return Ok(());
        }
        let (world, access, targets) = (self.world, self.access, self.targets);
        let batches = items
            .par_iter()
            .map(|item| {
                let mut child = SystemContext::new(world, access, targets);
                f(item, &mut child)?;
                Ok(child.finish())
            })
            .collect::<Result<Vec<_>>>()?;
        self.changes.extend(batches.into_iter().flatten());
        Ok(())
    }

    fn local_scope(&self, writing: bool) -> Result<Option<&[Id]>> {
        let declared = if writing {
            self.access.writes
        } else {
            self.access.reads
        };
        if declared.contains(&SimulationData::World)
            || declared.contains(&SimulationData::LocalTransform)
        {
            return Ok(None);
        }
        self.access
            .require(SimulationData::LocalTransformsById, writing)?;
        Ok(Some(
            self.targets
                .context("missing local transform binding")?
                .ids(writing),
        ))
    }

    pub fn component_owners(&self, key: &'static str) -> Result<&'w [Id]> {
        self.access.require(SimulationData::Component(key), false)?;
        Ok(self.world.entities.component_owners(key))
    }
    pub fn first_raw_component(&self, key: &'static str) -> Result<Option<(Id, &'w Value)>> {
        self.access.require(SimulationData::Component(key), false)?;
        Ok(self.world.first_raw_component(key))
    }
    pub fn component<T: Clone + DeserializeOwned + 'static>(
        &self,
        id: Id,
        key: &'static str,
    ) -> Result<Option<Cow<'w, T>>> {
        self.access.require(SimulationData::Component(key), false)?;
        self.world.typed_component(id, key)
    }
    pub fn local(&self, id: Id) -> Result<Option<Mat4>> {
        if let Some(ids) = self.local_scope(false)? {
            ensure!(
                ids.binary_search(&id).is_ok(),
                "{}: undeclared read of LocalTransform entity {id}",
                self.access.name
            );
        }
        Ok(self.world.entities.get(&id).map(|e| e.local))
    }
    pub fn set_pose(&mut self, transforms: impl IntoIterator<Item = (Id, Mat4)>) -> Result<()> {
        let scope = self.local_scope(true)?;
        let pose = LocalPose::prepare_checked(self.world, transforms, |id| {
            if let Some(ids) = scope {
                ensure!(
                    ids.binary_search(&id).is_ok(),
                    "{}: undeclared write of LocalTransform entity {id}",
                    self.access.name
                );
            }
            Ok(())
        })?;
        self.changes.push(Change::Pose(pose));
        Ok(())
    }
    /// Publish one complete layout of local joint transforms. Readers that missed
    /// substeps can consume the union of dirty joints and the latest pose version.
    pub fn publish_pose(&mut self, owner: Id, layout: PoseLayout, locals: Vec<Mat4>) -> Result<()> {
        ensure!(
            self.world.entities.contains_key(&owner),
            "missing pose owner {owner}"
        );
        let joints = layout.joints();
        ensure!(
            locals.len() == joints.len(),
            "pose does not match its joint layout"
        );
        let scope = self.local_scope(true)?;
        for (&id, local) in joints.iter().zip(&locals) {
            ensure!(
                scope.is_none_or(|ids| ids.binary_search(&id).is_ok()),
                "{}: undeclared write of LocalTransform entity {id}",
                self.access.name
            );
            ensure!(
                local.is_finite() && self.world.entities.contains_key(&id),
                "invalid pose joint {id}"
            );
        }
        self.changes
            .push(Change::PublishedPose(owner, layout, locals));
        Ok(())
    }
    pub fn set_component(&mut self, id: Id, key: &'static str, value: Value) -> Result<()> {
        self.check_component_target(id, key)?;
        self.changes.push(Change::Component(
            id,
            key,
            Box::new(move |world| {
                world.set_setting_if_present(id, key, value);
            }),
        ));
        Ok(())
    }
    pub fn set_typed_component<T: Serialize + Send + Sync + 'static>(
        &mut self,
        id: Id,
        key: &'static str,
        value: T,
    ) -> Result<()> {
        self.check_component_target(id, key)?;
        self.changes.push(Change::Component(
            id,
            key,
            Box::new(move |world| {
                world.set_typed_component(id, key, value);
            }),
        ));
        Ok(())
    }
    fn check_component_target(&self, id: Id, key: &'static str) -> Result<()> {
        self.access.require(SimulationData::Component(key), true)?;
        self.world
            .entities
            .get(&id)
            .context("missing component entity")?;
        Ok(())
    }
    pub fn resource<T: Send + Sync + 'static>(&self, key: &'static str) -> Result<Option<&'w T>> {
        self.access.require(SimulationData::Resource(key), false)?;
        self.world
            .simulation_resources
            .get(key)
            .map(|v| {
                v.downcast_ref()
                    .context("simulation resource type mismatch")
            })
            .transpose()
    }
    pub fn set_resource<T: Send + Sync + 'static>(
        &mut self,
        key: &'static str,
        value: T,
    ) -> Result<()> {
        self.access.require(SimulationData::Resource(key), true)?;
        let existing = self
            .changes
            .iter()
            .rev()
            .find_map(|change| match change {
                Change::Resource(k, value) if *k == key => Some(value),
                _ => None,
            })
            .or_else(|| self.world.simulation_resources.get(key));
        ensure!(
            existing.is_none_or(|old| old.is::<T>()),
            "{}: type mismatch writing resource {key}",
            self.access.name
        );

        self.changes
            .push(Change::Resource(key, std::sync::Arc::new(value)));
        Ok(())
    }
    pub fn world(&self) -> Result<&'w World> {
        self.access.require(SimulationData::World, false)?;
        Ok(self.world)
    }
    pub fn edit_world(&mut self, edit: Edit) -> Result<()> {
        // A closure with &mut World can both inspect and modify arbitrary data.
        self.access.require(SimulationData::World, false)?;
        self.access.require(SimulationData::World, true)?;
        self.changes.push(Change::Whole(edit));
        Ok(())
    }
}

#[cfg(test)]
mod parallel_tests {
    use super::*;
    #[test]
    fn staged_items_commit_in_input_order_and_failed_group_has_no_writes() -> Result<()> {
        let mut w = World::demo();
        let id = *w.entities.keys().next().unwrap();
        let make = |w| {
            SystemContext::new(
                w,
                SystemAccess {
                    name: "items",
                    reads: &[],
                    writes: &[SimulationData::Component("test.items")],
                },
                None,
            )
        };
        let mut cx = make(&w);
        let items = [3u32, 2, 4, 1];
        let result = cx.for_each_parallel(&items, |n, child| {
            child.set_component(id, "test.items", (*n).into())?;
            ensure!(*n != 4, "test failure");
            Ok(())
        });
        assert!(result.is_err() && cx.changes.is_empty());
        cx.for_each_parallel(&items, |n, child| {
            child.set_component(id, "test.items", (*n).into())
        })?;
        for edit in cx.finish() {
            edit.apply(&mut w, None);
        }
        assert_eq!(w.entities[&id].components["test.items"], 1);
        Ok(())
    }
}
