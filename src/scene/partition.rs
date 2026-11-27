//! File-backed logical cells. Only active actors are members of the simulation World.
use super::{ComponentSet, Entity, Id, Instance, World};
use anyhow::{Context, Result, ensure};
use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::{Arc, mpsc},
};
const FRAME_ROWS: usize = 32;
const BATCH_ROWS: usize = 32;
const ASSEMBLY_TIME: std::time::Duration = std::time::Duration::from_micros(1000);
mod mobile;
use super::changes::Cursor;
use mobile::MobileIndex;
/// An interest epoch owns its feedback slots; retired snapshots cannot acknowledge a reloaded asset.
#[derive(Default, Clone)]
pub(crate) struct ReadyLevels(Arc<BTreeMap<String, Arc<std::sync::atomic::AtomicU8>>>);
impl ReadyLevels {
    pub(crate) fn record(&self, key: &str, level: u8) {
        if let Some(slot) = self.0.get(key) {
            slot.store(level, std::sync::atomic::Ordering::Release);
        }
    }
    fn satisfies(&self, c: &Cell) -> bool {
        c.assets.iter().all(|(key, &min)| {
            self.0
                .get(key)
                .map_or(0, |v| v.load(std::sync::atomic::Ordering::Acquire))
                >= min
        })
    }
    fn reconcile(&self, levels: &BTreeMap<String, u8>) -> Self {
        Self(Arc::new(
            levels
                .keys()
                .map(|k| (k.clone(), self.0.get(k).cloned().unwrap_or_default()))
                .collect(),
        ))
    }
}
const FILE_BYTES: u64 = 1024 * 1024;
const IO_LIMIT: usize = 2;
mod archive;
mod cell;
use cell::{Actor, Checkpoint, Rows, checkpoint, read_bounded, validate_rows};
pub use cell::{Cell, Layout};
#[derive(Clone, Copy, PartialOrd, Ord, PartialEq, Eq)]
enum Item {
    Batch(usize),
    Mobile(Id),
}
enum Job {
    Read(Item, PathBuf),
    Save(Item, Rows),
}
enum Reply {
    Read(Item, Result<Rows>),
    Saved(Item, Rows, Result<tempfile::TempPath>),
}
type Tile = (i32, i32, i32);
fn tile(p: Vec3, width: f32) -> Tile {
    let v = (p / width).floor().as_ivec3();
    (v.x, v.y, v.z)
}
pub struct Runtime {
    grid: BTreeMap<Tile, Vec<usize>>,
    loaded: BTreeMap<usize, Rows>,
    active: BTreeSet<usize>,
    pending: BTreeSet<Item>,
    writing: BTreeSet<Item>,
    mobile: MobileIndex,
    cursor: Option<Cursor>,
    asset_levels: Arc<BTreeMap<String, u8>>,
    ready: ReadyLevels,
    issuing: BTreeSet<usize>,
    jobs: mpsc::SyncSender<Job>,
    results: mpsc::Receiver<Reply>,
}
impl Runtime {
    pub fn new(world: &World) -> Result<Option<Self>> {
        let Some(l) = &world.partition else {
            return Ok(None);
        };
        l.validate()?;
        let (jobs, work) = mpsc::sync_channel::<Job>(IO_LIMIT);
        let (out, results) = mpsc::channel();
        let mut active = BTreeSet::new();
        for (i, c) in l.cells.iter().enumerate() {
            let authored =
                (c.first..c.first + c.count as u64).filter(|id| !l.roaming.contains_key(id));
            let n = authored
                .clone()
                .filter(|id| world.entities.contains_key(id))
                .count();
            ensure!(
                n == 0 || n == authored.count(),
                "partially restored cell batch"
            );
            if n > 0 {
                active.insert(i);
            }
        }
        std::thread::Builder::new()
            .name("lab-world-io".into())
            .spawn(move || {
                for job in work {
                    let reply = match job {
                        Job::Read(i, path) => Reply::Read(
                            i,
                            read_bounded(&path)
                                .and_then(|v| Ok(Arc::new(serde_json::from_slice(&v)?))),
                        ),
                        Job::Save(i, rows) => {
                            let result = checkpoint(&rows);
                            Reply::Saved(i, rows, result)
                        }
                    };
                    if out.send(reply).is_err() {
                        break;
                    }
                }
            })?;
        let mut grid = BTreeMap::<Tile, Vec<usize>>::new();
        for (i, c) in l.cells.iter().enumerate() {
            grid.entry(tile(c.center, l.unload)).or_default().push(i);
        }
        Ok(Some(Self {
            mobile: MobileIndex::restore(world, l)?,
            cursor: None,
            asset_levels: Default::default(),
            ready: Default::default(),
            issuing: Default::default(),
            grid,
            writing: Default::default(),
            loaded: Default::default(),
            active,
            pending: Default::default(),
            jobs,
            results,
        }))
    }
    /// Required levels are merged by stable asset key across all interested cells and sources.
    pub fn asset_levels(&self) -> &Arc<BTreeMap<String, u8>> {
        &self.asset_levels
    }
    /// The pinned root has level zero. Higher minimums gate activation until acknowledged.
    pub fn set_asset_ready(&self, key: &str, level: u8) {
        self.ready.record(key, level);
    }
    pub fn counts(&self) -> (usize, usize, usize) {
        (
            self.pending.len() + self.writing.len(),
            self.loaded.len(),
            self.active.len(),
        )
    }
    pub fn update(&mut self, world: &mut Arc<World>, sources: &[Vec3]) -> Result<()> {
        let layout = world.partition.clone().context("missing cell layout")?;
        ensure!(
            sources.iter().all(|p| p.is_finite()),
            "invalid world streaming source"
        );
        let distance = |c: &Cell| {
            sources
                .iter()
                .map(|p| p.distance_squared(c.center))
                .fold(f32::INFINITY, f32::min)
        };
        while let Ok(msg) = self.results.try_recv() {
            match msg {
                Reply::Read(item, v) => {
                    self.pending.remove(&item);
                    match item {
                        Item::Batch(i) if distance(&layout.cells[i]) <= layout.unload.powi(2) => {
                            let rows = v?;
                            validate_rows(&rows, &layout.cells[i], world)?;
                            self.loaded.insert(i, rows);
                        }
                        Item::Mobile(id) => {
                            let mobile = &layout.roaming[&id];
                            let i = self.mobile.by_first[&mobile.cell];
                            if distance(&layout.cells[i]) <= layout.unload.powi(2) {
                                let rows = v?;
                                ensure!(rows.len() == 1, "invalid mobile actor file");
                                cell::validate_actors(&rows, world)?;
                                self.mobile.loaded.insert(id, rows);
                            }
                        }
                        _ => {}
                    }
                }
                Reply::Saved(item, rows, result) => {
                    self.writing.remove(&item);
                    let file = result?;
                    let world_layout = world.partition.as_ref().unwrap();
                    let slot = match item {
                        Item::Batch(i) => world_layout.overrides.get(&layout.cells[i].first),
                        Item::Mobile(id) => world_layout
                            .roaming
                            .get(&id)
                            .and_then(|m| m.checkpoint.as_ref()),
                    };
                    if matches!(slot, Some(Checkpoint::Memory(current)) if Arc::ptr_eq(current,&rows))
                    {
                        let l = Arc::make_mut(Arc::make_mut(world).partition.as_mut().unwrap());
                        let saved = Checkpoint::File(Arc::new(file), rows.len());
                        match item {
                            Item::Batch(i) => {
                                l.overrides.insert(layout.cells[i].first, saved);
                            }
                            Item::Mobile(id) => {
                                l.roaming.get_mut(&id).unwrap().checkpoint = Some(saved)
                            }
                        }
                    }
                }
            }
        }
        self.migrate_actors(world)?;
        self.loaded
            .retain(|i, _| distance(&layout.cells[*i]) <= layout.unload.powi(2));
        let mut remaining = FRAME_ROWS;
        let mut nearby = self.active.clone();
        for &source in sources {
            let (x, y, z) = tile(source, layout.unload);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if let Some(ids) = self.grid.get(&(
                            x.saturating_add(dx),
                            y.saturating_add(dy),
                            z.saturating_add(dz),
                        )) {
                            nearby.extend(ids);
                        }
                    }
                }
            }
        }
        let mut ordered: Vec<_> = nearby.into_iter().collect();
        ordered.sort_unstable_by(|&a, &b| {
            distance(&layout.cells[a])
                .total_cmp(&distance(&layout.cells[b]))
                .then(a.cmp(&b))
        });
        let interested: BTreeSet<_> = ordered
            .iter()
            .copied()
            .filter(|&i| {
                let c = &layout.cells[i];
                !c.assets.is_empty()
                    && (distance(c) <= layout.preload.powi(2) || self.active.contains(&i))
            })
            .collect();
        if interested != self.issuing {
            let mut levels = BTreeMap::<String, u8>::new();
            for &i in &interested {
                for (key, &minimum) in &layout.cells[i].assets {
                    levels
                        .entry(key.clone())
                        .and_modify(|v| *v = (*v).max(minimum))
                        .or_insert(minimum);
                }
            }
            self.issuing = interested;
            if *self.asset_levels != levels {
                self.ready = self.ready.reconcile(&levels);
                self.asset_levels = Arc::new(levels);
                let w = Arc::make_mut(world);
                w.cell_asset_levels = self.asset_levels.clone();
                w.cell_asset_ready = self.ready.clone();
            }
        }
        let started = std::time::Instant::now();
        self.stream_mobiles(world, &ordered, sources, &mut remaining, started)?;
        for i in ordered {
            if remaining < FRAME_ROWS && started.elapsed() >= ASSEMBLY_TIME {
                break;
            }

            let c = &layout.cells[i];
            let d = distance(c);
            if self.active.contains(&i) && d > layout.preload.powi(2) && remaining >= c.count {
                if c.save_state
                    && (self.writing.contains(&Item::Batch(i))
                        || self.pending.len() + self.writing.len() >= IO_LIMIT)
                {
                    continue;
                }
                let w = Arc::make_mut(world);
                let ownership = w.partition.as_ref().unwrap().clone();
                let ids = (c.first..c.first + c.count as u64)
                    .filter(|id| !ownership.roaming.contains_key(id));
                ensure!(
                    ids.clone().all(|id| w.entities.contains_key(&id)),
                    "active cell actor removed externally"
                );
                for id in ids {
                    let e = &w.entities[&id];
                    cell::validate_leaf(id, e)?;
                }
                let rows: Vec<Actor> = (c.first..c.first + c.count as u64)
                    .map(|id| {
                        if ownership.roaming.contains_key(&id) {
                            Actor::default()
                        } else {
                            w.entities.remove(&id).unwrap().into()
                        }
                    })
                    .collect();
                let rows = Arc::new(rows);
                if c.save_state {
                    Arc::make_mut(w.partition.as_mut().unwrap())
                        .overrides
                        .insert(c.first, Checkpoint::Memory(rows.clone()));
                    self.jobs
                        .try_send(Job::Save(Item::Batch(i), rows.clone()))
                        .map_err(|_| anyhow::anyhow!("world checkpoint worker stopped"))?;
                    self.writing.insert(Item::Batch(i));
                }
                if d <= layout.unload.powi(2) {
                    self.loaded.insert(i, rows);
                }
                self.active.remove(&i);
                remaining -= c.count;
            }
            if self.active.contains(&i) {
                continue;
            }
            if d <= layout.preload.powi(2)
                && !self.loaded.contains_key(&i)
                && !self.pending.contains(&Item::Batch(i))
            {
                let saved = world
                    .partition
                    .as_ref()
                    .and_then(|l| l.overrides.get(&c.first));
                if let Some(Checkpoint::Memory(rows)) = saved {
                    self.loaded.insert(i, rows.clone());
                } else if self.pending.len() + self.writing.len() < IO_LIMIT {
                    let path = match saved {
                        Some(Checkpoint::File(p, _)) => p.to_path_buf(),
                        _ => layout.base.join(&c.file),
                    };
                    if self.jobs.try_send(Job::Read(Item::Batch(i), path)).is_ok() {
                        self.pending.insert(Item::Batch(i));
                    }
                }
            }
            if d <= layout.activate.powi(2)
                && remaining >= c.count
                && self.ready.satisfies(c)
                && let Some(rows) = self.loaded.get(&i)
            {
                validate_rows(rows, c, world)?;
                ensure!(
                    (c.first..c.first + c.count as u64)
                        .filter(|id| !world.partition.as_ref().unwrap().roaming.contains_key(id))
                        .all(|id| !world.entities.contains_key(&id)),
                    "cell ID collision"
                );
                let rows = self.loaded.remove(&i).unwrap();
                let w = Arc::make_mut(world);
                for (id, row) in (c.first..).zip(Arc::unwrap_or_clone(rows)) {
                    if !w.partition.as_ref().unwrap().roaming.contains_key(&id) {
                        w.entities.insert(id, row.into());
                    }
                }
                Arc::make_mut(w.partition.as_mut().unwrap())
                    .overrides
                    .remove(&c.first);
                self.active.insert(i);
                remaining -= c.count;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
