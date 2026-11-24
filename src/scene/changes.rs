//! Bounded, multi-reader edit history. Each extractor keeps its own cursor.
use super::*;
use std::{collections::VecDeque, ops::Deref};

const CAPACITY: usize = 4096;

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

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Write {
    Inspect,
    Local,
    Material,
    Component(&'static str),
}

pub(crate) struct Cursor {
    identity: Arc<()>,
    sequence: u64,
}

#[derive(Default)]
struct Journal {
    identity: Arc<()>,
    sequence: u64,
    edits: VecDeque<(Id, Write)>,
}
impl Journal {
    fn advance(&mut self) {
        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("ECS edit sequence exhausted");
    }
    fn invalidate(&mut self) {
        self.advance();
        self.edits.clear();
    }
    fn write(&mut self, id: Id, kind: Write) {
        self.advance();
        if self.edits.len() == CAPACITY {
            self.edits.pop_front();
        }
        self.edits.push_back((id, kind));
    }
}

/// Ordered scene entities. Shared access is read-only. All mutable access is journaled.
#[derive(Default, Deserialize)]
#[serde(transparent)]
pub struct Entities {
    rows: BTreeMap<Id, Entity>,
    #[serde(skip)]
    journal: Journal,
    #[serde(skip)]
    owners: std::sync::OnceLock<BTreeMap<String, Vec<Id>>>,
}
impl Clone for Entities {
    fn clone(&self) -> Self {
        Self {
            rows: self.rows.clone(),
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
        Cursor {
            identity: self.journal.identity.clone(),
            sequence: self.journal.sequence,
        }
    }
    pub(crate) fn changes(
        &self,
        cursor: &Cursor,
    ) -> Option<impl Iterator<Item = (Id, Write)> + '_> {
        if !Arc::ptr_eq(&self.journal.identity, &cursor.identity) {
            return None;
        }
        let count = usize::try_from(self.journal.sequence.checked_sub(cursor.sequence)?).ok()?;
        let start = self.journal.edits.len().checked_sub(count)?;
        Some(self.journal.edits.range(start..).copied())
    }
    /// A raw row edit may alter several fields. Asset layouts and extension membership
    /// use full reconciliation; sparse rows are checked against their old bindings.
    pub fn get_mut(&mut self, id: &Id) -> Option<&mut Entity> {
        let row = self.rows.get_mut(id)?;
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
            && e.skin.is_none()
            && e.skin_binding.is_none()
            && e.light.is_none()
            && e.texture.is_none()
            && e.components.is_empty()
    }
    fn edit(&mut self, id: Id, kind: Write) -> Option<&mut Entity> {
        let row = self.rows.get_mut(&id)?;
        if let Write::Component(key) = kind
            && !row.components.contains_key(key)
        {
            self.owners.take();
        }
        self.journal.write(id, kind);
        Some(row)
    }
    pub fn insert(&mut self, id: Id, entity: Entity) -> Option<Entity> {
        self.owners.take();
        self.journal.invalidate();
        self.rows.insert(id, entity)
    }
    pub fn remove(&mut self, id: &Id) -> Option<Entity> {
        let row = self.rows.remove(id)?;
        self.owners.take();
        self.journal.invalidate();
        Some(row)
    }
    pub fn values_mut(&mut self) -> std::collections::btree_map::ValuesMut<'_, Id, Entity> {
        self.owners.take();
        self.journal.invalidate();
        self.rows.values_mut()
    }
    pub fn iter_mut(&mut self) -> std::collections::btree_map::IterMut<'_, Id, Entity> {
        self.owners.take();
        self.journal.invalidate();
        self.rows.iter_mut()
    }
    pub fn retain(&mut self, keep: impl FnMut(&Id, &mut Entity) -> bool) {
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
            .edit(id, Write::Component(key))
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
