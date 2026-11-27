use super::*;
use std::collections::HashMap;

/// The preorder is stable between structural edits. Skin palettes and render users
/// may keep row indices for the lifetime of this topology.
#[derive(Default)]
pub(super) struct TransformCache {
    pub slots: HashMap<Id, usize>,
    pub rows: Vec<Transform>,
    revision: u64,
}
/// Most joints feed exactly one skin. Keep that common reverse edge inline.
#[derive(Default)]
pub(super) enum SkinUsers {
    #[default]
    None,
    One((usize, usize)),
    Several(Vec<(usize, usize)>),
}
impl SkinUsers {
    pub fn add(&mut self, next: (usize, usize)) {
        match self {
            Self::None => *self = Self::One(next),
            Self::One(first) => *self = Self::Several(vec![*first, next]),
            Self::Several(users) => users.push(next),
        }
    }
    pub fn entries(&self) -> &[(usize, usize)] {
        match self {
            Self::None => &[],
            Self::One(user) => std::slice::from_ref(user),
            Self::Several(users) => users,
        }
    }
}
pub(super) struct Transform {
    pub id: Id,
    pub parent: Option<Id>,
    parent_slot: Option<usize>,
    pub end: usize,
    pub local: Mat4,
    pub world: Mat4,
    pub version: u64,
    pub instance: Option<usize>,
    pub skins: SkinUsers,
    pub light: bool,
}
impl TransformCache {
    pub fn get(&self, id: Id) -> Option<&Transform> {
        self.slots.get(&id).map(|&i| &self.rows[i])
    }
    pub fn row_mut(&mut self, id: Id) -> &mut Transform {
        &mut self.rows[self.slots[&id]]
    }

    /// Rebuild topology only when extraction has to reconcile a structural edit.
    pub fn update(&mut self, world: &World) -> Result<usize> {
        let mut children = HashMap::<Id, Vec<Id>>::new();
        let mut pending = Vec::new();
        for (&id, e) in &world.entities {
            if let Some(parent) = e.parent {
                children.entry(parent).or_default().push(id);
            } else {
                pending.push((id, None));
            }
        }
        pending.reverse();
        let mut next = Self {
            slots: HashMap::with_capacity(world.entities.len()),
            rows: Vec::with_capacity(world.entities.len()),
            revision: self.revision,
        };
        let mut updated = 0;
        while let Some((id, parent_slot)) = pending.pop() {
            let e = &world.entities[&id];
            let model = parent_slot.map_or(e.local, |s: usize| next.rows[s].world * e.local);
            let version = if let Some(prev) = self
                .get(id)
                .filter(|t| t.world == model && t.parent == e.parent)
            {
                prev.version
            } else {
                updated += 1;
                next.revision = next
                    .revision
                    .checked_add(1)
                    .expect("transform versions exhausted");
                next.revision
            };
            let slot = next.rows.len();
            next.slots.insert(id, slot);
            next.rows.push(Transform {
                id,
                parent: e.parent,
                parent_slot,
                end: slot + 1,
                local: e.local,
                world: model,
                version,
                instance: None,
                skins: SkinUsers::None,
                light: false,
            });
            if let Some(descendants) = children.get(&id) {
                pending.extend(descendants.iter().rev().map(|&id| (id, Some(slot))));
            }
        }
        ensure!(
            next.rows.len() == world.entities.len(),
            "missing parent or cycle in scene transform hierarchy"
        );
        for slot in (0..next.rows.len()).rev() {
            if let Some(parent) = next.rows[slot].parent_slot {
                next.rows[parent].end = next.rows[parent].end.max(next.rows[slot].end);
            }
        }
        *self = next;
        Ok(updated)
    }
    pub fn resolve(&mut self, slot: usize) -> bool {
        let t = &self.rows[slot];
        let model = t
            .parent_slot
            .map_or(t.local, |s| self.rows[s].world * t.local);
        if model == t.world {
            return false;
        }
        self.revision = self
            .revision
            .checked_add(1)
            .expect("transform versions exhausted");
        self.rows[slot].world = model;
        self.rows[slot].version = self.revision;
        true
    }
}
