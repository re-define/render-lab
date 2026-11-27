//! Bounded, multi-reader edit history. Each extractor keeps its own cursor.
use super::*;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::Deref,
    sync::atomic::{AtomicBool, Ordering},
};

const RETAINED_EPOCHS: usize = 8;

/// Local transforms prepared against a simulation snapshot before any edit commits.
pub struct LocalPose(Vec<(Id, Mat4)>);
impl LocalPose {
    pub fn prepare(
        world: &World,
        transforms: impl IntoIterator<Item = (Id, Mat4)>,
    ) -> Result<Self> {
        Self::prepare_checked(world, transforms, |_| Ok(()))
    }
    pub(crate) fn prepare_checked(
        world: &World,
        transforms: impl IntoIterator<Item = (Id, Mat4)>,
        check: impl Fn(Id) -> Result<()>,
    ) -> Result<Self> {
        let transforms: Vec<_> = transforms.into_iter().collect();
        for (id, local) in &transforms {
            check(*id)?;
            anyhow::ensure!(world.entities.contains_key(id), "missing pose entity {id}");
            anyhow::ensure!(local.is_finite(), "non-finite local pose for entity {id}");
        }
        Ok(Self(transforms))
    }
    pub(crate) fn previous(&self, world: &World) -> Self {
        Self(
            self.0
                .iter()
                .filter_map(|(id, _)| world.entities.get(id).map(|e| (*id, e.local)))
                .collect(),
        )
    }
    /// Preserve input order for duplicate joints. An earlier structural edit may have removed a row.
    pub fn apply(self, world: &mut World) {
        for (id, local) in self.0 {
            world.entities.set_local_if_present(id, local);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Write {
    Inspect,
    Local,
    Material,
    Texture,
    Light,
    GeometryAssets,
    GeometryBinding,
    Component(&'static str),
    TypedValue(&'static str),
}

pub(crate) struct Cursor {
    identity: Arc<()>,
    sequence: u64,
}

/// Taking a snapshot seals its epoch. Repeated writes to one property occupy one entry
/// until any reader takes a new cursor. A slow reader can consume several sealed epochs.
#[derive(Default)]
struct Journal {
    identity: Arc<()>,
    epoch: u64,
    sealed: AtomicBool,
    history: VecDeque<Epoch>,
    pose_revision: u64,
}

#[derive(Default)]
struct Epoch {
    writes: HashSet<(Id, Write)>,
    poses: HashMap<Id, PublishedPose>,
}

pub(crate) struct Edits<'a>(std::collections::vec_deque::Iter<'a, Epoch>);
impl Edits<'_> {
    pub fn writes(&self) -> impl Iterator<Item = (Id, Write)> + '_ {
        self.0
            .clone()
            .flat_map(|epoch| epoch.writes.iter().copied())
    }
    pub fn poses(&self) -> impl Iterator<Item = &PublishedPose> {
        self.0.clone().flat_map(|epoch| epoch.poses.values())
    }
}

/// Stable local-joint order for a pose producer. Validate the binding once at rig setup.
#[derive(Clone)]
pub struct PoseLayout(Arc<[Id]>);
impl PoseLayout {
    pub fn new(ids: impl IntoIterator<Item = Id>) -> Result<Self> {
        let ids: Arc<[Id]> = ids.into_iter().collect();
        ensure!(
            !ids.is_empty() && ids.iter().copied().collect::<HashSet<_>>().len() == ids.len(),
            "pose layout needs distinct joints"
        );
        Ok(Self(ids))
    }
    pub fn joints(&self) -> &[Id] {
        &self.0
    }
}
/// The latest complete authored pose in this journal epoch, with the union of
/// changes since it opened. Older snapshots retain their own immutable palette.
pub(crate) struct PublishedPose {
    pub revision: u64,
    pub joints: Arc<[Id]>,
    pub locals: Vec<Mat4>,
    dirty: Vec<u64>,
}
impl PublishedPose {
    pub fn changes(&self) -> impl Iterator<Item = (Id, Mat4)> + '_ {
        self.dirty
            .iter()
            .enumerate()
            .flat_map(move |(word, &mask)| {
                let mut remaining = mask;
                std::iter::from_fn(move || {
                    if remaining == 0 {
                        return None;
                    }
                    let index = word * 64 + remaining.trailing_zeros() as usize;
                    remaining &= remaining - 1;
                    Some((self.joints[index], self.locals[index]))
                })
            })
    }
}
impl Journal {
    fn invalidate(&mut self) {
        if !self.history.is_empty() || *self.sealed.get_mut() {
            *self = Self::default();
        }
    }
    fn current(&mut self) -> &mut Epoch {
        if self.history.is_empty() || *self.sealed.get_mut() {
            *self.sealed.get_mut() = false;
            self.epoch = self.epoch.checked_add(1).expect("ECS epoch exhausted");
            let mut batch = if self.history.len() == RETAINED_EPOCHS {
                self.history.pop_front().unwrap()
            } else {
                Epoch::default()
            };
            batch.writes.clear();
            batch.poses.clear();
            self.history.push_back(batch);
        }
        self.history.back_mut().unwrap()
    }
    fn write(&mut self, id: Id, kind: Write) {
        self.current().writes.insert((id, kind));
    }
}

/// Ordered scene entities. Shared access is read-only. All mutable access is journaled.
#[derive(Default, Deserialize)]
#[serde(transparent)]
pub struct Entities {
    rows: BTreeMap<Id, Entity>,
    #[serde(skip)]
    hierarchy_revision: u64,
    #[serde(skip)]
    journal: Journal,
    #[serde(skip)]
    owners: std::sync::OnceLock<BTreeMap<String, Vec<Id>>>,
}
impl Clone for Entities {
    fn clone(&self) -> Self {
        Self {
            rows: self.rows.clone(),
            hierarchy_revision: self.hierarchy_revision,
            journal: Default::default(),
            owners: Default::default(),
        }
    }
}
impl Deref for Entities {
    type Target = BTreeMap<Id, Entity>;
    fn deref(&self) -> &Self::Target {
        &self.rows
    }
}
impl<'a> IntoIterator for &'a Entities {
    type Item = (&'a Id, &'a Entity);
    type IntoIter = std::collections::btree_map::Iter<'a, Id, Entity>;
    fn into_iter(self) -> Self::IntoIter {
        self.rows.iter()
    }
}
impl Entities {
    /// Raw row edits may change a name or parent. Typed value and local-pose updates preserve this revision.
    pub(crate) fn hierarchy_revision(&self) -> u64 {
        self.hierarchy_revision
    }
    fn set_local_if_present(&mut self, id: Id, local: Mat4) -> Option<()> {
        let row = self.rows.get_mut(&id)?;
        if row.local != local {
            row.local = local;
            self.journal.write(id, Write::Local);
        }
        Some(())
    }
    /// Stable ID order, including when a component has several owners.
    pub fn component_owners(&self, key: &str) -> &[Id] {
        let index = self.owners.get_or_init(|| {
            let mut index = BTreeMap::<String, Vec<Id>>::new();
            for (&id, row) in &self.rows {
                for key in row.components.keys() {
                    index.entry(key.clone()).or_default().push(id);
                }
            }
            index
        });
        index.get(key).map_or(&[], Vec::as_slice)
    }
    pub(crate) fn cursor(&self) -> Cursor {
        self.journal.sealed.store(true, Ordering::Relaxed);
        Cursor {
            identity: self.journal.identity.clone(),
            sequence: self.journal.epoch,
        }
    }
    pub(crate) fn publish_pose(&mut self, owner: Id, layout: PoseLayout, locals: Vec<Mat4>) {
        let joints = layout.0;
        debug_assert_eq!(joints.len(), locals.len());
        let (rows, journal) = (&mut self.rows, &mut self.journal);
        let mut changed = joints
            .iter()
            .zip(&locals)
            .enumerate()
            .filter_map(|(i, (id, local))| {
                let row = rows.get_mut(id)?;
                if row.local == *local {
                    return None;
                }
                row.local = *local;
                Some(i)
            });
        let Some(first) = changed.next() else {
            return;
        };
        journal.pose_revision = journal
            .pose_revision
            .checked_add(1)
            .expect("pose versions exhausted");
        let revision = journal.pose_revision;
        let epoch = journal.current();
        if let Some(old) = epoch.poses.get(&owner)
            && !Arc::ptr_eq(&old.joints, &joints)
            && *old.joints != *joints
        {
            // A new binding can omit joints changed earlier in the same epoch.
            for (id, _) in old.changes() {
                epoch.writes.insert((id, Write::Local));
            }
            epoch.poses.remove(&owner);
        }
        let word_count = joints.len().div_ceil(64);
        let pose = epoch.poses.entry(owner).or_insert_with(|| PublishedPose {
            revision,
            joints: joints.clone(),
            locals: Vec::new(),
            dirty: vec![0; word_count],
        });
        for index in std::iter::once(first).chain(changed) {
            pose.dirty[index / 64] |= 1 << (index % 64);
        }
        pose.revision = revision;
        pose.locals = locals;
    }
    pub(crate) fn changes(&self, cursor: &Cursor) -> Option<Edits<'_>> {
        if !Arc::ptr_eq(&self.journal.identity, &cursor.identity) {
            return None;
        }
        let count = usize::try_from(self.journal.epoch.checked_sub(cursor.sequence)?).ok()?;
        let start = self.journal.history.len().checked_sub(count)?;
        Some(Edits(self.journal.history.range(start..)))
    }
    /// A raw row edit may alter several fields. Asset layouts and extension membership
    /// use full reconciliation; sparse rows are checked against their old bindings.
    pub fn get_mut(&mut self, id: &Id) -> Option<&mut Entity> {
        let row = self.rows.get_mut(id)?;
        self.hierarchy_revision = self.hierarchy_revision.wrapping_add(1);
        self.owners.take();
        if Self::inspectable(row) {
            self.journal.write(*id, Write::Inspect);
        } else {
            self.journal.invalidate();
        }
        Some(row)
    }
    pub(crate) fn inspectable(e: &Entity) -> bool {
        e.mesh.is_none()
            && e.geometry_assets.is_none()
            && e.skin.is_none()
            && e.skin_binding.is_none()
            && e.light.is_none()
            && e.texture.is_none()
            && e.components.is_empty()
    }
    pub(super) fn edit(&mut self, id: Id, kind: Write) -> Option<&mut Entity> {
        let row = self.rows.get_mut(&id)?;
        let kind = match kind {
            Write::TypedValue(key) | Write::Component(key) if !row.components.contains_key(key) => {
                self.owners.take();
                Write::Component(key)
            }
            _ => kind,
        };
        self.journal.write(id, kind);
        Some(row)
    }
    pub fn insert(&mut self, id: Id, entity: Entity) -> Option<Entity> {
        self.hierarchy_revision = self.hierarchy_revision.wrapping_add(1);
        self.owners.take();
        self.journal.invalidate();
        self.rows.insert(id, entity)
    }
    pub fn remove(&mut self, id: &Id) -> Option<Entity> {
        let row = self.rows.remove(id)?;
        self.hierarchy_revision = self.hierarchy_revision.wrapping_add(1);
        self.owners.take();
        self.journal.invalidate();
        Some(row)
    }
    pub fn values_mut(&mut self) -> std::collections::btree_map::ValuesMut<'_, Id, Entity> {
        self.hierarchy_revision = self.hierarchy_revision.wrapping_add(1);
        self.owners.take();
        self.journal.invalidate();
        self.rows.values_mut()
    }
    pub fn iter_mut(&mut self) -> std::collections::btree_map::IterMut<'_, Id, Entity> {
        self.hierarchy_revision = self.hierarchy_revision.wrapping_add(1);
        self.owners.take();
        self.journal.invalidate();
        self.rows.iter_mut()
    }
    pub fn retain(&mut self, keep: impl FnMut(&Id, &mut Entity) -> bool) {
        self.hierarchy_revision = self.hierarchy_revision.wrapping_add(1);
        self.owners.take();
        self.journal.invalidate();
        self.rows.retain(keep)
    }
}
impl World {
    pub fn set_local(&mut self, id: Id, local: Mat4) -> Result<()> {
        self.entities
            .set_local_if_present(id, local)
            .context("missing transform entity")
    }
    /// Update an existing light, validating its new world-space direction before either edit commits.
    pub fn set_light_instance(&mut self, id: Id, light: Light, local: Mat4) -> Result<()> {
        let entity = self.entities.get(&id).context("missing light entity")?;
        let old = entity.light.context("entity has no light")?;
        let mut parent = entity.parent;
        let mut chain = Vec::new();
        while let Some(next) = parent {
            ensure!(
                chain.len() < self.entities.len(),
                "cycle in light hierarchy"
            );
            let ancestor = self.entities.get(&next).context("missing light parent")?;
            chain.push(ancestor.local);
            parent = ancestor.parent;
        }
        light.validate(chain.into_iter().rev().fold(Mat4::IDENTITY, |m, t| m * t) * local)?;
        if old != light {
            self.entities.edit(id, Write::Light).unwrap().light = Some(light);
        }
        self.set_local(id, local)
    }
    /// Replace a texture asset while retaining its entity ID and all material bindings.
    pub fn set_texture(&mut self, id: Id, texture: Arc<Texture>) -> Result<()> {
        let old = self
            .entities
            .get(&id)
            .and_then(|e| e.texture.as_ref())
            .context("missing texture entity")?;
        if !Arc::ptr_eq(old, &texture) {
            texture.validate()?;
            self.entities.edit(id, Write::Texture).unwrap().texture = Some(texture);
        }
        Ok(())
    }
    pub fn set_material(&mut self, id: Id, material: Material) -> Result<()> {
        let old = self
            .entities
            .get(&id)
            .and_then(|e| e.material.as_ref())
            .context("missing material entity")?;
        if old != &material {
            self.entities.edit(id, Write::Material).unwrap().material = Some(material);
        }
        Ok(())
    }
    pub(crate) fn set_setting(&mut self, id: Id, key: &'static str, value: Value) {
        self.set_setting_if_present(id, key, value)
            .expect("missing component entity");
    }
    pub(crate) fn set_setting_if_present(
        &mut self,
        id: Id,
        key: &'static str,
        value: Value,
    ) -> Option<()> {
        self.runtime.remove_component(id, key);
        self.entities
            .edit(id, Write::Component(key))?
            .components
            .insert(key.into(), value);
        Some(())
    }
    pub(super) fn mark_component(&mut self, id: Id, key: &'static str) -> &mut Entity {
        self.entities
            .edit(id, Write::TypedValue(key))
            .expect("missing component entity")
    }
}

#[cfg(test)]
mod component_index_tests {
    use super::*;
    #[test]
    fn owner_index_tracks_membership_for_sparse_and_unrestricted_edits() {
        let mut w = World::default();
        let a = w.spawn(Entity::default());
        let b = w.spawn(Entity::default());
        assert!(w.first_raw_component("type").is_none());
        w.set_setting(b, "type", 1.into());
        w.set_setting(a, "type", 2.into());
        assert_eq!(w.entities.component_owners("type"), [a, b]);
        w.set_setting(a, "type", 3.into());
        assert_eq!(w.first_raw_component("type"), Some((a, &Value::from(3))));
        w.entities.get_mut(&a).unwrap().components.remove("type");
        assert_eq!(w.entities.component_owners("type"), [b]);
        let mut copy = w.clone();
        copy.entities.remove(&b);
        assert!(copy.entities.component_owners("type").is_empty());
        for e in w.entities.values_mut() {
            e.components.insert("type".into(), true.into());
        }
        assert_eq!(w.entities.component_owners("type"), [a, b]);
        w.entities.retain(|&id, _| id == a);
        assert_eq!(w.entities.component_owners("type"), [a]);
        w.entities.insert(a, Entity::default());
        assert!(w.first_raw_component("type").is_none());
    }
}
