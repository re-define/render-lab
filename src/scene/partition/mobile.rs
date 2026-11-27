//! A roaming leaf keeps its authored ID. Its new cell owns its cold checkpoint.
use super::*;
use crate::scene::{Mobility, MotionPolicy, changes::Write};

#[derive(Default)]
pub(super) struct MobileIndex {
    pub by_first: BTreeMap<Id, usize>,
    members: BTreeMap<usize, BTreeSet<Id>>,
    live: BTreeSet<Id>,
    pub loaded: BTreeMap<Id, Rows>,
}
impl MobileIndex {
    pub fn restore(world: &World, layout: &Layout) -> Result<Self> {
        let mut index = Self {
            by_first: layout
                .cells
                .iter()
                .enumerate()
                .map(|(i, c)| (c.first, i))
                .collect(),
            ..Default::default()
        };
        for (&id, m) in &layout.roaming {
            index
                .members
                .entry(index.by_first[&m.cell])
                .or_default()
                .insert(id);
            if world.entities.contains_key(&id) {
                index.live.insert(id);
            } else {
                ensure!(
                    m.file.is_some() || m.checkpoint.is_some(),
                    "missing checkpoint for mobile actor {id}"
                );
            }
        }
        Ok(index)
    }
    fn birth(&self, id: Id, layout: &Layout) -> Option<usize> {
        let (&first, &i) = self.by_first.range(..=id).next_back()?;
        (id - first < layout.cells[i].count as u64).then_some(i)
    }
}
impl Runtime {
    pub(super) fn migrate_actors(&mut self, world: &mut Arc<World>) -> Result<()> {
        let layout = world.partition.as_ref().unwrap().clone();
        let candidates: BTreeSet<Id> =
            if let Some(edits) = self.cursor.as_ref().and_then(|c| world.entities.changes(c)) {
                edits
                    .writes()
                    .filter(|(_, w)| matches!(w, Write::Local | Write::Inspect | Write::Structure))
                    .map(|(id, _)| id)
                    .collect()
            } else {
                self.active
                    .iter()
                    .flat_map(|&i| {
                        let c = &layout.cells[i];
                        c.first..c.first + c.count as u64
                    })
                    .chain(self.mobile.live.iter().copied())
                    .collect()
            };
        self.cursor = Some(world.entities.cursor());
        for id in candidates {
            let Some(entity) = world.entities.get(&id) else {
                continue;
            };
            let Some(birth) = self.mobile.birth(id, &layout) else {
                continue;
            };
            let old = layout
                .roaming
                .get(&id)
                .map_or(birth, |m| self.mobile.by_first[&m.cell]);
            if entity.parent.is_some()
                || !self.active.contains(&old) && !self.mobile.live.contains(&id)
            {
                continue;
            }
            if world.motion_policy(id, MotionPolicy::STATIC)?.mobility != Mobility::Movable {
                continue;
            }
            let pos = entity.local.w_axis.truncate();
            let distance = |i: usize| layout.cells[i].center.distance_squared(pos);
            let mut closest = old;
            let mut best = distance(old);
            let (x, y, z) = tile(pos, layout.unload);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if let Some(batch) = self.grid.get(&(
                            x.saturating_add(dx),
                            y.saturating_add(dy),
                            z.saturating_add(dz),
                        )) {
                            for &i in batch {
                                let d = distance(i);
                                if d < best && d <= layout.preload.powi(2) {
                                    best = d;
                                    closest = i;
                                }
                            }
                        }
                    }
                }
            }
            if closest == old {
                continue;
            }
            cell::validate_leaf(id, entity)?;
            let l = Arc::make_mut(Arc::make_mut(world).partition.as_mut().unwrap());
            l.roaming.insert(
                id,
                cell::Mobile {
                    cell: layout.cells[closest].first,
                    file: None,
                    checkpoint: None,
                },
            );
            if let Some(m) = self.mobile.members.get_mut(&old) {
                m.remove(&id);
            }
            self.mobile.members.entry(closest).or_default().insert(id);
            self.mobile.live.insert(id);
        }
        Ok(())
    }
    pub(super) fn stream_mobiles(
        &mut self,
        world: &mut Arc<World>,
        ordered: &[usize],
        sources: &[Vec3],
        remaining: &mut usize,
        started: std::time::Instant,
    ) -> Result<()> {
        let l = world.partition.as_ref().unwrap().clone();
        let distance = |id: Id| {
            let cell = &l.cells[self.mobile.by_first[&l.roaming[&id].cell]];
            sources
                .iter()
                .map(|s| s.distance_squared(cell.center))
                .fold(f32::INFINITY, f32::min)
        };
        self.mobile
            .loaded
            .retain(|id, _| distance(*id) <= l.unload.powi(2));
        let mut wanted = self.mobile.live.clone();
        for i in ordered {
            if let Some(m) = self.mobile.members.get(i) {
                wanted.extend(m);
            }
        }
        for id in wanted {
            if *remaining == 0 || *remaining < FRAME_ROWS && started.elapsed() >= ASSEMBLY_TIME {
                break;
            }
            let item = Item::Mobile(id);
            let d = distance(id);
            if self.mobile.live.contains(&id) {
                if d <= l.preload.powi(2)
                    || self.writing.contains(&item)
                    || self.writing.len() + self.pending.len() >= IO_LIMIT
                {
                    continue;
                }
                let w = Arc::make_mut(world);
                cell::validate_leaf(
                    id,
                    w.entities
                        .get(&id)
                        .context("live mobile actor removed externally")?,
                )?;
                let rows = Arc::new(vec![w.entities.remove(&id).unwrap().into()]);
                let m = Arc::make_mut(w.partition.as_mut().unwrap())
                    .roaming
                    .get_mut(&id)
                    .unwrap();
                m.checkpoint = Some(Checkpoint::Memory(rows.clone()));
                m.file = None;
                self.jobs
                    .try_send(Job::Save(item, rows.clone()))
                    .map_err(|_| anyhow::anyhow!("mobile checkpoint worker stopped"))?;
                self.writing.insert(item);
                self.mobile.live.remove(&id);
                if d <= l.unload.powi(2) {
                    self.mobile.loaded.insert(id, rows);
                }
                *remaining -= 1;
                continue;
            }
            if d > l.preload.powi(2) {
                continue;
            }
            let mobile = &l.roaming[&id];
            if !self.mobile.loaded.contains_key(&id) && !self.pending.contains(&item) {
                if let Some(Checkpoint::Memory(rows)) = &mobile.checkpoint {
                    self.mobile.loaded.insert(id, rows.clone());
                } else if self.pending.len() + self.writing.len() < IO_LIMIT {
                    let path = match &mobile.checkpoint {
                        Some(Checkpoint::File(path, _)) => path.to_path_buf(),
                        _ => l.base.join(
                            mobile
                                .file
                                .as_deref()
                                .context("missing saved mobile actor")?,
                        ),
                    };
                    if self.jobs.try_send(Job::Read(item, path)).is_ok() {
                        self.pending.insert(item);
                    }
                }
            }
            let c = &l.cells[self.mobile.by_first[&mobile.cell]];
            let ready = self.ready.satisfies(c);
            if d <= l.activate.powi(2)
                && ready
                && let Some(rows) = self.mobile.loaded.remove(&id)
            {
                ensure!(
                    !world.entities.contains_key(&id) && rows.len() == 1,
                    "mobile actor collision or invalid checkpoint"
                );
                cell::validate_actors(&rows, world)?;
                let w = Arc::make_mut(world);
                w.entities
                    .insert(id, Arc::unwrap_or_clone(rows).pop().unwrap().into());
                let m = Arc::make_mut(w.partition.as_mut().unwrap())
                    .roaming
                    .get_mut(&id)
                    .unwrap();
                m.file = None;
                m.checkpoint = None;
                self.mobile.live.insert(id);
                *remaining -= 1;
            }
        }
        Ok(())
    }
}
