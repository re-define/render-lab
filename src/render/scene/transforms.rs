use super::*;
use std::collections::HashMap;

/// The preorder is stable between structural edits. Skin palettes and render users
/// may keep row indices for the lifetime of this topology.
#[derive(Default)]
pub(super) struct TransformCache {
    pub slots: HashMap<Id, usize>,
    pub rows: Vec<Transform>,
    revision: u64,
    // Runtime leaves outside the dense DFS ordering, indexed by their parent's dense slot.
    attached: BTreeMap<usize, BTreeSet<usize>>,
    vacant: BTreeSet<usize>,
    dense_len: usize,
    // For a purely skeletal interior row, the first following row needing eager world FK.
    lazy_end: Vec<usize>,
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
    pub(super) parent_slot: Option<usize>,
    pub end: usize,
    pub local: Mat4,
    pub world: Mat4,
    pub version: u64,
    pub instance: Option<usize>,
    pub skins: SkinUsers,
    pub light: bool,
}
impl TransformCache {
    pub fn configure_pose_interiors(&mut self, roots: &[usize]) {
        if roots.is_empty() {
            return;
        }
        let mut lazy = vec![false; self.rows.len()];
        for &root in roots {
            lazy[root + 1..self.rows[root].end].fill(true);
        }
        for &root in roots {
            lazy[root] = false;
        }
        for (i, row) in self.rows.iter().enumerate().rev() {
            if row.instance.is_some() || row.light || !row.skins.entries().is_empty() {
                lazy[i] = false;
            }
            if !lazy[i]
                && let Some(parent) = row.parent_slot
            {
                lazy[parent] = false;
            }
        }
        self.lazy_end = vec![0; lazy.len()];
        let mut end = lazy.len();
        for i in (0..lazy.len()).rev() {
            if lazy[i] {
                self.lazy_end[i] = end;
            } else {
                end = i;
            }
        }
    }
    pub fn eager_spans(&self, moved: &[(usize, u64, Mat4)]) -> Vec<std::ops::Range<usize>> {
        let mut ranges = Vec::new();
        self.changed_subtrees(moved, &mut ranges);
        if self.lazy_end.is_empty() {
            return ranges;
        }
        let mut spans = Vec::new();
        for range in ranges {
            let end = range.end;
            let mut start = range.start;
            while start < end {
                if let Some(skip) = self.lazy_until(start) {
                    start = skip;
                    continue;
                }
                let mut next = start + 1;
                while next < end && self.lazy_until(next).is_none() {
                    next += 1;
                }
                spans.push(start..next);
                start = next;
            }
        }
        spans
    }
    fn lazy_until(&self, slot: usize) -> Option<usize> {
        self.lazy_end.get(slot).copied().filter(|&end| end != 0)
    }
    pub fn install_local(&mut self, slot: usize, local: Mat4) -> bool {
        if self.rows[slot].local == local {
            return false;
        }
        self.rows[slot].local = local;
        if self.lazy_until(slot).is_some() {
            self.revision = self
                .revision
                .checked_add(1)
                .expect("transform versions exhausted");
            self.rows[slot].version = self.revision;
        }
        true
    }
    pub fn world_version(&self, mut slot: usize) -> u64 {
        let mut version = self.rows[slot].version;
        while self.lazy_until(slot).is_some() {
            slot = self.rows[slot].parent_slot.unwrap();
            version = version.max(self.rows[slot].version);
        }
        version
    }
    pub fn world_matrix(&self, mut slot: usize) -> Mat4 {
        if self.lazy_until(slot).is_none() {
            return self.rows[slot].world;
        }
        let mut chain = Vec::new();
        while self.lazy_until(slot).is_some() {
            chain.push(slot);
            slot = self.rows[slot].parent_slot.unwrap();
        }
        chain
            .into_iter()
            .rev()
            .fold(self.rows[slot].world, |m, node| m * self.rows[node].local)
    }
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
            ..Default::default()
        };
        let mut updated = 0;
        while let Some((id, parent_slot)) = pending.pop() {
            let e = &world.entities[&id];
            let model = parent_slot.map_or(e.local, |s: usize| next.rows[s].world * e.local);
            let version = if let Some(prev) = self
                .slots
                .get(&id)
                .copied()
                .filter(|&old| {
                    self.lazy_until(old).is_none()
                        && self.rows[old].world == model
                        && self.rows[old].parent == e.parent
                })
                .map(|old| &self.rows[old])
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
        next.dense_len = next.rows.len();
        *self = next;
        Ok(updated)
    }
    pub fn removable_leaf(&self, slot: usize) -> bool {
        let row = &self.rows[slot];
        row.end == slot + 1 && row.skins.entries().is_empty() && !self.attached.contains_key(&slot)
    }
    pub fn remove_leaf(&mut self, slot: usize) {
        let row = &self.rows[slot];
        self.slots.remove(&row.id);
        if let Some(parent) = row.parent_slot
            && let Some(children) = self.attached.get_mut(&parent)
        {
            children.remove(&slot);
            if children.is_empty() {
                self.attached.remove(&parent);
            }
        }
        self.vacant.insert(slot);
    }
    pub fn insert_leaf(&mut self, id: Id, e: &Entity) -> Result<usize> {
        let parent = e
            .parent
            .map(|id| self.slots.get(&id).copied().context("missing leaf parent"))
            .transpose()?;
        let model = parent.map_or(e.local, |p| self.world_matrix(p) * e.local);
        self.revision = self
            .revision
            .checked_add(1)
            .expect("transform versions exhausted");
        // Only recycle slots outside the dense forest to preserve all dense subtree intervals.
        let slot = self
            .vacant
            .range(self.dense_len..)
            .find(|&&s| parent.is_none_or(|p| s > p))
            .copied()
            .unwrap_or(self.rows.len());
        let row = Transform {
            id,
            parent: e.parent,
            parent_slot: parent,
            end: slot + 1,
            local: e.local,
            world: model,
            version: self.revision,
            instance: None,
            skins: Default::default(),
            light: e.light.is_some(),
        };
        if self.vacant.remove(&slot) {
            self.rows[slot] = row;
        } else {
            self.rows.push(row);
        }
        if let Some(parent) = parent {
            self.attached.entry(parent).or_default().insert(slot);
        }
        self.slots.insert(id, slot);
        Ok(slot)
    }
    pub fn live(&self, slot: usize) -> bool {
        self.vacant.is_empty() || !self.vacant.contains(&slot)
    }
    pub fn live_count(&self, range: &std::ops::Range<usize>) -> usize {
        range.len() - self.vacant.range(range.clone()).count()
    }
    // Dynamic children are appended outside their parents' contiguous DFS subtrees.
    pub fn changed_subtrees(
        &self,
        moved: &[(usize, u64, Mat4)],
        ranges: &mut Vec<std::ops::Range<usize>>,
    ) {
        for &(slot, _, _) in moved {
            if ranges.last().is_none_or(|r| slot >= r.end) {
                ranges.push(slot..self.rows[slot].end);
            }
        }
        if !self.attached.is_empty() && !ranges.is_empty() {
            let mut seen: BTreeSet<_> = ranges.iter().map(|r| r.start).collect();
            let mut cursor = 0;
            while cursor < ranges.len() {
                for (_, children) in self.attached.range(ranges[cursor].clone()) {
                    for &child in children {
                        if seen.insert(child) {
                            ranges.push(child..child + 1);
                        }
                    }
                }
                cursor += 1;
            }
            ranges.sort_unstable_by_key(|r| r.start);
        }
    }
    pub fn resolve(&mut self, slot: usize) -> bool {
        let t = &self.rows[slot];
        let model = t
            .parent_slot
            .map_or(t.local, |s| self.world_matrix(s) * t.local);
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
