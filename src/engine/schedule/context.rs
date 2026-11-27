use super::{Edit, SimulationData, SystemAccess, TransformTargets};
use crate::scene::{Field, Id, LocalPose, PoseLayout, Stored, World};
use anyhow::{Context, Result, ensure};
use glam::Mat4;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{borrow::Cow, sync::Arc};

pub(super) enum ComponentUpdate {
    Raw(Value),
    Binding(Arc<dyn Stored>),
    State(Arc<dyn Stored>),
}

pub(super) enum Change {
    Pose(LocalPose),
    PublishedPose(Id, PoseLayout, Vec<Mat4>),
    Component(Id, &'static str, ComponentUpdate),
    Resource(
        &'static str,
        std::sync::Arc<dyn std::any::Any + Send + Sync>,
    ),
    Whole(Edit),
}

pub(super) enum Undo {
    Field(Id, &'static str, Option<Field>),
    Other(Edit),
}
impl Undo {
    pub fn restore(self, world: &mut World) {
        match self {
            Self::Field(id, key, Some(value)) => {
                world.entities.replace_field(id, key, value);
                world.binding_changed(key);
            }
            Self::Field(id, key, None) => world.remove_typed_component(id, key),
            Self::Other(revert) => revert(world),
        }
    }
}

impl Change {
    fn capture_undo(
        &self,
        world: &World,
        counts: Option<&mut super::CommitCounts>,
    ) -> Option<Edit> {
        Some(match self {
            Self::Pose(pose) => {
                let old = pose.previous(world);
                if let Some(c) = counts {
                    c.undo_pose_bytes += old.matrix_bytes();
                }
                Box::new(move |w| old.apply(w))
            }
            Self::PublishedPose(_, layout, _) => {
                let old: Vec<_> = layout
                    .joints()
                    .iter()
                    .filter_map(|id| world.entities.get(id).map(|e| (*id, e.local)))
                    .collect();
                if let Some(c) = counts {
                    c.undo_pose_bytes += old.len() * size_of::<Mat4>();
                }
                Box::new(move |w| {
                    for (id, local) in old {
                        let _ = w.set_local(id, local);
                    }
                })
            }
            Self::Component(..) => unreachable!("field swaps retain the previous value"),
            Self::Resource(key, _) => {
                let key = *key;
                let old = world.simulation_resources.get(key).cloned();
                Box::new(move |w| match old {
                    Some(old) => {
                        w.simulation_resources.insert(key, old);
                    }
                    None => {
                        w.simulation_resources.remove(key);
                    }
                })
            }
            Self::Whole(_) => {
                let old = world.clone();
                Box::new(move |w| *w = old)
            }
        })
    }
    pub(super) fn apply(self, world: &mut World, undo: Option<&mut Vec<Undo>>) {
        self.apply_timed(world, undo, None, None);
    }
    pub(super) fn apply_timed(
        self,
        world: &mut World,
        undo: Option<&mut Vec<Undo>>,
        capture: Option<&mut std::time::Duration>,
        mut counts: Option<&mut super::CommitCounts>,
    ) {
        if let Some(c) = counts.as_deref_mut() {
            c.writes += 1;
            match &self {
                Self::Component(..) => c.components += 1,
                Self::Pose(p) => c.pose_bytes += p.matrix_bytes(),
                Self::PublishedPose(_, _, locals) => c.pose_bytes += size_of_val(locals.as_slice()),
                _ => {}
            }
        }
        if let Self::Component(id, key, update) = self {
            let started = if undo.is_some() {
                capture.as_ref().map(|_| std::time::Instant::now())
            } else {
                None
            };
            let binding = !matches!(update, ComponentUpdate::State(_));
            let field = match update {
                ComponentUpdate::Raw(v) => Field::Json(v),
                ComponentUpdate::State(v) | ComponentUpdate::Binding(v) => Field::typed(v),
            };
            if let Some(old) = world.entities.replace_field(id, key, field) {
                if binding {
                    world.binding_changed(key);
                }
                if let Some(undo) = undo {
                    undo.push(Undo::Field(id, key, old));
                    if let (Some(total), Some(started)) = (capture, started) {
                        *total += started.elapsed();
                    }
                    if let Some(c) = counts {
                        c.undo_records += 1;
                    }
                }
            }
            return;
        }
        if let Some(undo) = undo
            && let Some(edit) =
                super::timed(capture, || self.capture_undo(world, counts.as_deref_mut()))
        {
            undo.push(Undo::Other(edit));
            if let Some(c) = counts {
                c.undo_records += 1;
            }
        }
        match self {
            Self::Pose(pose) => pose.apply(world),
            Self::PublishedPose(owner, layout, locals) => {
                world.entities.publish_pose(owner, layout, locals)
            }
            Self::Component(..) => unreachable!(),
            Self::Resource(key, value) => {
                world.simulation_resources.insert(key, value);
            }
            Self::Whole(edit) => edit(world),
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
    /// End of this system tick on the world simulation timeline, including in nested jobs.
    pub fn time_seconds(&self) -> f64 {
        self.world.time_seconds()
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
        self.for_each_parallel_init(items, || (), |(), item, cx| f(item, cx))
    }

    /// Each Rayon job reuses private scratch across its items. Only staged world edits
    /// survive a job; errors discard them with the same semantics as for_each_parallel.
    pub(crate) fn for_each_parallel_init<T: Sync, S>(
        &mut self,
        items: &[T],
        init: impl Fn() -> S + Sync + Send,
        f: impl Fn(&mut S, &T, &mut SystemContext<'w>) -> Result<()> + Sync,
    ) -> Result<()> {
        use rayon::prelude::*;
        if items.len() <= 1 {
            let checkpoint = self.changes.len();
            if let Some(one) = items.first()
                && let Err(err) = f(&mut init(), one, self)
            {
                self.changes.truncate(checkpoint);
                return Err(err);
            }
            return Ok(());
        }
        let (world, access, targets) = (self.world, self.access, self.targets);
        // Keep several independent chunks per worker and one ordered edit buffer per chunk.
        let grain = items
            .len()
            .div_ceil(4 * rayon::current_num_threads())
            .clamp(1, 16);
        let batches = items
            .par_chunks(grain)
            .map_init(init, |scratch, chunk| {
                let mut child = SystemContext::new(world, access, targets);
                for item in chunk {
                    f(scratch, item, &mut child)?;
                }
                Ok(child.finish())
            })
            .collect::<Result<Vec<_>>>()?;
        self.changes.reserve(batches.iter().map(Vec::len).sum());
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
    /// Monotonic revision of authored state for an explicitly declared component.
    pub(crate) fn component_binding_version(&self, key: &'static str) -> Result<u64> {
        self.access.require(SimulationData::Component(key), false)?;
        Ok(self.world.binding_version(key))
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
    /// Read motion policy within the system's declared component access.
    pub fn motion_policy(
        &self,
        id: Id,
        fallback: crate::scene::MotionPolicy,
    ) -> Result<crate::scene::MotionPolicy> {
        self.access.require(
            SimulationData::Component(crate::scene::MOTION_POLICY),
            false,
        )?;
        self.world.motion_policy(id, fallback)
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
        self.changes
            .push(Change::Component(id, key, ComponentUpdate::Raw(value)));
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
            ComponentUpdate::Binding(Arc::new(value)),
        ));
        Ok(())
    }
    /// Write dynamic state from a bound system whose rig, fixtures and owner stay unchanged.
    pub(crate) fn set_bound_state<T: Serialize + Send + Sync + 'static>(
        &mut self,
        id: Id,
        key: &'static str,
        value: T,
    ) -> Result<()> {
        self.check_component_target(id, key)?;
        self.changes.push(Change::Component(
            id,
            key,
            ComponentUpdate::State(Arc::new(value)),
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
