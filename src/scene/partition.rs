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
const FILE_BYTES: u64 = 1024 * 1024;
const IO_LIMIT: usize = 2;
mod cell;
use cell::{Actor, Checkpoint, Rows, checkpoint, read_bounded, validate_rows};
pub use cell::{Cell, Layout};
enum Job {
    Read(usize, PathBuf),
    Save(usize, Rows),
}
enum Reply {
    Read(usize, Result<Rows>),
    Saved(usize, Rows, Result<tempfile::TempPath>),
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
    pending: BTreeSet<usize>,
    writing: BTreeSet<usize>,
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
            let n = (c.first..c.first + c.count as u64)
                .filter(|id| world.entities.contains_key(id))
                .count();
            ensure!(n == 0 || n == c.count, "partially restored cell");
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
            grid,
            writing: Default::default(),
            loaded: Default::default(),
            active,
            pending: Default::default(),
            jobs,
            results,
        }))
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
                Reply::Read(i, v) => {
                    self.pending.remove(&i);
                    let c = &layout.cells[i];
                    if distance(c) <= layout.unload.powi(2) {
                        let rows = v?;
                        validate_rows(&rows, c, world)?;
                        self.loaded.insert(i, rows);
                    }
                }
                Reply::Saved(i, rows, result) => {
                    self.writing.remove(&i);
                    let file = result?;
                    let id = layout.cells[i].first;
                    if world.partition.as_ref().is_some_and(|l|matches!(l.overrides.get(&id),Some(Checkpoint::Memory(current)) if Arc::ptr_eq(current,&rows))){
                    let l=Arc::make_mut(Arc::make_mut(world).partition.as_mut().unwrap());l.overrides.insert(id,Checkpoint::File(Arc::new(file),rows.len()));
                }
                }
            }
        }
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
        for i in ordered {
            let c = &layout.cells[i];
            let d = distance(c);
            if self.active.contains(&i) && d > layout.preload.powi(2) && remaining >= c.count {
                if c.save_state
                    && (self.writing.contains(&i)
                        || self.pending.len() + self.writing.len() >= IO_LIMIT)
                {
                    continue;
                }
                let w = Arc::make_mut(world);
                ensure!(
                    (c.first..c.first + c.count as u64).all(|id| w.entities.contains_key(&id)),
                    "active cell actor removed externally"
                );
                for id in c.first..c.first + c.count as u64 {
                    let e = &w.entities[&id];
                    ensure!(
                        e.parent.is_none()
                            && e.mesh.is_none()
                            && e.material.is_none()
                            && e.light.is_none()
                            && e.skin.is_none()
                            && e.skin_binding.is_none()
                            && e.texture.is_none()
                            && e.animation.is_none()
                            && e.plant.is_none()
                            && e.geometry_assets.is_none()
                            && e.gaussian_payload.is_none()
                            && e.meshlets.is_none()
                            && e.hierarchy.is_none()
                            && e.topology.is_none(),
                        "cannot checkpoint actor {id} after a change to its cell schema"
                    );
                }
                let rows: Vec<Actor> = (c.first..c.first + c.count as u64)
                    .map(|id| w.entities.remove(&id).unwrap().into())
                    .collect();
                let rows = Arc::new(rows);
                if c.save_state {
                    Arc::make_mut(w.partition.as_mut().unwrap())
                        .overrides
                        .insert(c.first, Checkpoint::Memory(rows.clone()));
                    self.jobs
                        .try_send(Job::Save(i, rows.clone()))
                        .map_err(|_| anyhow::anyhow!("world checkpoint worker stopped"))?;
                    self.writing.insert(i);
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
                && !self.pending.contains(&i)
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
                    if self.jobs.try_send(Job::Read(i, path)).is_ok() {
                        self.pending.insert(i);
                    }
                }
            }
            if d <= layout.activate.powi(2)
                && remaining >= c.count
                && let Some(rows) = self.loaded.get(&i)
            {
                validate_rows(rows, c, world)?;
                ensure!(
                    (c.first..c.first + c.count as u64).all(|id| !world.entities.contains_key(&id)),
                    "cell ID collision"
                );
                let rows = self.loaded.remove(&i).unwrap();
                let w = Arc::make_mut(world);
                for (id, row) in (c.first..).zip(Arc::unwrap_or_clone(rows)) {
                    w.entities.insert(id, row.into());
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
mod tests {
    use super::*;
    use crate::{engine::Schedule, experiments::movement_lab, movement::linear};
    #[test]
    fn preload_activate_simulate_unload_save_and_resume() -> Result<()> {
        let mut w = Arc::new(movement_lab::partitioned()?);
        let mut r = Runtime::new(&w)?.unwrap();
        let far = Vec3::new(0., 0., 12.);
        let start = std::time::Instant::now();
        loop {
            r.update(&mut w, &[far])?;
            if !r.loaded.is_empty() {
                break;
            }
            ensure!(start.elapsed().as_secs() < 4, "cell read timeout");
            std::thread::yield_now();
        }
        assert!(!w.entities.contains_key(&1000));
        assert!(r.active.is_empty());
        let mut extractor = crate::render::SceneExtractor::default();
        extractor.snapshot(&w, Default::default(), [64, 64], Default::default())?;
        r.update(&mut w, &[Vec3::ZERO])?;
        let rendered = extractor.snapshot(&w, Default::default(), [64, 64], Default::default())?;
        assert!(!rendered.scene.stats.full_rebuild);
        assert_eq!(rendered.scene.stats.instances_updated, 6);
        let authored = w.entities[&1000].local;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        let mut sim = Schedule::default();
        linear::register(&mut sim)?;
        sim.tick(&mut w, 1. / 30., &pool)?;
        let moved = w.entities[&1000].local;
        let momentum = w.linear_body(1000)?.unwrap().momentum;
        assert_ne!(moved, authored);
        let dir = tempfile::tempdir()?;
        let save = dir.path().join("scene.json");
        w.save(&save)?;
        let live = World::load(&save)?;
        assert_eq!(live.entities[&1000].local, moved);
        assert_eq!(Runtime::new(&live)?.unwrap().active.len(), 1);
        r.update(&mut w, &[Vec3::splat(1000.)])?;
        assert!(!w.entities.contains_key(&1000));
        while !r.writing.is_empty() {
            ensure!(start.elapsed().as_secs() < 5, "cold checkpoint timeout");
            r.update(&mut w, &[Vec3::splat(1000.)])?;
            std::thread::yield_now();
        }
        assert!(matches!(
            w.partition.as_ref().unwrap().overrides.get(&1000),
            Some(Checkpoint::File(..))
        ));
        w.save(&save)?;
        let mut reloaded = Arc::new(World::load(&save)?);
        let mut r = Runtime::new(&reloaded)?.unwrap();
        while !reloaded.entities.contains_key(&1000) {
            ensure!(start.elapsed().as_secs() < 5, "restore timeout");
            r.update(&mut reloaded, &[Vec3::ZERO])?;
            std::thread::yield_now();
        }
        assert_eq!(reloaded.entities[&1000].local, moved);
        assert_eq!(reloaded.linear_body(1000)?.unwrap().momentum, momentum);
        Ok(())
    }
}
