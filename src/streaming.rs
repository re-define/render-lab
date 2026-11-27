//! Shader feedback selects detail. A file worker and immutable frame leases decouple I/O from drawing.
#[cfg(lab_directstorage)]
pub(crate) mod directstorage;
mod disk;
mod feedback;
mod policy;
pub(crate) use feedback::FeedbackSink;
use feedback::{PRIORITY_MASK, Reports};
pub use policy::StreamStats;
use policy::{Admission, Metrics};
mod package;
use crate::rdg::{Graph, Op, Resource};
use anyhow::{Result, ensure};
pub(crate) use disk::Upload;
pub use package::{Level, Package, Part};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Instant,
};

pub const PAGE_BYTES: usize = 64 * 1024;
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub requests: usize,
    pub upload_pages: usize,
    pub in_flight: usize,
    pub pool_pages: usize,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            requests: 8,
            upload_pages: 4,
            in_flight: 16,
            pool_pages: 32,
        }
    }
}
type Key = (usize, usize, usize); // part, detail level, page within level
pub(crate) enum UploadFailure {
    #[cfg(lab_directstorage)]
    Import(String),
    Capacity(String),
}
pub(crate) struct Lease {
    pub failure: std::sync::OnceLock<UploadFailure>,
    pub pool: u64,
    pub retirement: std::sync::OnceLock<mpsc::Sender<(u64, usize)>>,
    pub slot: usize,
    pub complete: AtomicBool,
    pub cold: AtomicBool,
    pub evict: AtomicBool,
    pub detached: AtomicBool,
    pub ready: std::sync::OnceLock<(u64, usize, u64)>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(events) = self.retirement.get() {
            let _ = events.send((self.pool, self.slot));
        }
    }
}
struct Page {
    lease: Arc<Lease>,
    stage: Option<Staged>,
}
impl Drop for Page {
    fn drop(&mut self) {
        self.lease.detached.store(true, Ordering::Release);
    }
}
/// One owner reference per admitted page, including evicted uploads still held by a GPU frame.
struct Permit {
    _owner: Arc<()>,
}
struct Staged {
    upload: Upload,
    permit: Arc<Permit>,
}
type ReadJob = (Key, Level, Option<[u8; 8]>, Arc<Permit>);
pub(crate) struct CopyTicket {
    pub lease: Arc<Lease>,
    _permit: Arc<Permit>,
}
struct Pending {
    page: Page,
    ticket: Option<Weak<CopyTicket>>,
    last_used: u64,
}
#[derive(Clone, Copy)]
pub struct Binding {
    pub level: usize,
    pub first_page: u32,
    pub word_offset: u32,
}
pub struct View {
    pub table: Resource,
    pub parts: Vec<Option<Binding>>,
    pub uploads: usize,
    pub stats: StreamStats,
}
/// Consumers use a streaming unit as the smallest representation they can publish
/// atomically (e.g. one texture tile or all payload pages for one cluster group).
pub struct Streamer {
    parts: Vec<Option<Part>>,
    pool: u64,
    budget: Budget,
    slots: Vec<Weak<Lease>>,
    pages: BTreeMap<Key, Pending>,
    pending: std::collections::BTreeSet<Key>,
    cache_frames: u64,
    frame_budget_ms: f64,
    metrics: Metrics,
    waiting: BTreeMap<usize, (usize, u64, Instant)>,
    last_visible: Vec<usize>,
    minimum: BTreeMap<usize, u32>,
    permits: Arc<()>,
    jobs: mpsc::SyncSender<Vec<ReadJob>>,
    results: mpsc::Receiver<(Key, Result<Staged, String>)>,
    host_only: bool,
    retry_after: BTreeMap<Key, u64>,
    frame: u64,
    cancelled: Arc<AtomicBool>,
    feedback: Arc<Mutex<Reports>>,
    admission: Option<(BTreeMap<usize, u32>, Admission)>,
    pub last_error: Option<String>,
}
impl Streamer {
    pub fn new(package: Package, budget: Budget) -> Result<Self> {
        Self::from_packages(vec![package], budget)
    }
    /// A scene admits all registered packages against one pool and per-frame budget.
    pub fn from_packages(packages: Vec<Package>, budget: Budget) -> Result<Self> {
        let mut paths = Vec::new();
        let mut files = Vec::new();
        let mut parts = Vec::new();
        let mut roots = std::collections::HashSet::<Arc<[u8]>>::new();
        for mut package in packages {
            for part in &mut package.parts {
                if let Some(shared) = roots.get(&part.root) {
                    part.root = shared.clone();
                } else {
                    roots.insert(part.root.clone());
                }
            }
            files.extend(std::iter::repeat_n(paths.len(), package.parts.len()));
            paths.push(package.path);
            parts.extend(package.parts.into_iter().map(Some));
        }
        ensure!(
            budget.requests > 0
                && budget.upload_pages > 0
                && budget.in_flight > 0
                && budget.pool_pages > 0
                && budget.pool_pages <= 16384
                && (1..=feedback::MAX_UNITS).contains(&parts.len()),
            "invalid stream budget or part count"
        );
        let (jobs, work) = mpsc::sync_channel::<Vec<ReadJob>>(budget.in_flight);
        let (send, results) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = cancelled.clone();
        thread::Builder::new()
            .name("lab-page-io".into())
            .spawn(move || disk::run(paths, files, work, send, stop))?;
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Ok(Self {
            permits: Default::default(),
            host_only: false,
            retry_after: Default::default(),
            frame: 0,
            cancelled,
            last_visible: vec![0; parts.len()],
            parts,
            admission: None,
            pool: NEXT.fetch_add(1, Ordering::Relaxed),
            slots: (0..budget.pool_pages).map(|_| Weak::new()).collect(),
            budget,
            pages: Default::default(),
            pending: Default::default(),
            cache_frames: 8,
            frame_budget_ms: 1000. / 60.,
            metrics: Default::default(),
            waiting: Default::default(),
            minimum: Default::default(),
            jobs,
            results,
            feedback: Default::default(),
            last_error: None,
        })
    }
    pub fn bind(&mut self, g: &mut Graph, adapter: Option<[u8; 8]>) -> Result<View> {
        self.bind_view(g, adapter, true)
    }
    pub(crate) fn bind_view(
        &mut self,
        g: &mut Graph,
        adapter: Option<[u8; 8]>,
        dispatch: bool,
    ) -> Result<View> {
        if dispatch {
            self.frame = self.frame.saturating_add(1);
        }
        self.pages
            .retain(|key, p| match p.page.lease.failure.get() {
                #[cfg(lab_directstorage)]
                Some(UploadFailure::Import(e)) => {
                    self.host_only = true;
                    self.last_error = Some(e.clone());
                    false
                }
                Some(UploadFailure::Capacity(e)) => {
                    self.last_error = Some(e.clone());
                    self.retry_after.insert(*key, self.frame.saturating_add(60));
                    false
                }
                None => true,
            });
        let adapter = if self.host_only { None } else { adapter };
        let mut demand = self.minimum.clone();
        let reports = self.feedback.lock().unwrap();
        reports.merge(&mut demand);
        self.metrics.stats.feedback_overflows = reports.overflows;
        let slow = reports.gpu_ms.is_some_and(|ms| ms > self.frame_budget_ms);
        drop(reports);
        for (&unit, &word) in &demand {
            let desired = (word >> 24) as usize;
            if self.parts.get(unit).is_some_and(Option::is_some)
                && self.last_visible[unit] < desired
                && self.waiting.get(&unit).is_none_or(|w| w.0 != desired)
            {
                self.waiting
                    .insert(unit, (desired, self.frame, Instant::now()));
            }
        }
        self.waiting.retain(|id, _| demand.contains_key(id));
        if self
            .admission
            .as_ref()
            .is_none_or(|(last, _)| last != &demand)
        {
            let selection = Admission::new(
                &self.parts,
                &demand,
                self.budget.pool_pages,
                &self.waiting,
                self.frame,
            );
            self.admission = Some((demand, selection));
        }
        let admission = &self.admission.as_ref().unwrap().1;
        let wanted = &admission.wanted;
        self.retry_after.retain(|k, _| wanted.contains(k));
        // Drop unwanted pending uploads immediately. Keep completed cold pages briefly while
        // spare capacity exists. Immutable old frame tables independently retain their leases.
        self.pages.retain(|key, p| {
            let cold = !wanted.contains(key);
            p.page.lease.cold.store(cold, Ordering::Release);
            if p.page.lease.evict.swap(false, Ordering::AcqRel) && cold {
                return false;
            }
            if !cold {
                p.last_used = self.frame;
                return true;
            }
            p.page.lease.complete.load(Ordering::Acquire)
                && self.frame.saturating_sub(p.last_used) <= self.cache_frames
        });
        let mut cold: Vec<_> = self
            .pages
            .iter()
            .filter(|(k, _)| !wanted.contains(k))
            .map(|(k, p)| (p.last_used, *k))
            .collect();
        let discard = cold
            .len()
            .saturating_sub(self.budget.pool_pages.saturating_sub(wanted.len()));
        if discard > 0 {
            cold.sort_unstable();
            for (_, key) in cold.into_iter().take(discard) {
                self.pages.remove(&key);
            }
        }
        let allowed = &admission.levels;
        let upload_limit = if slow { 1 } else { self.budget.upload_pages };
        // Completed copies release CPU staging immediately; failed/dropped plans may retry.
        for p in self.pages.values_mut() {
            if p.page.lease.complete.load(Ordering::Acquire) {
                p.page.stage = None;
            }
        }
        let mut uploaded = 0;
        if dispatch {
            while let Some(slot) = self.slots.iter().position(|x| x.strong_count() == 0) {
                let Ok((key, result)) = self.results.try_recv() else {
                    break;
                };
                self.pending.remove(&key);
                if !wanted.contains(&key) {
                    continue;
                }
                let stage = match result {
                    Ok(v) => v,
                    Err(e) => {
                        self.retry_after.insert(key, self.frame.saturating_add(60));
                        self.last_error = Some(e);
                        continue;
                    }
                };
                let lease = Arc::new(Lease {
                    pool: self.pool,
                    retirement: Default::default(),
                    slot,
                    failure: Default::default(),
                    complete: AtomicBool::new(false),
                    cold: Default::default(),
                    evict: Default::default(),
                    detached: Default::default(),
                    ready: Default::default(),
                });
                self.slots[slot] = Arc::downgrade(&lease);
                self.pages.insert(
                    key,
                    Pending {
                        page: Page {
                            lease,
                            stage: Some(stage),
                        },
                        ticket: None,
                        last_used: self.frame,
                    },
                );
            }
        }
        let mut jobs = Vec::new();
        for &k in &admission.requests {
            if !dispatch
                || jobs.len() >= self.budget.requests
                || Arc::strong_count(&self.permits) > self.budget.in_flight
                || self.pending.len() + jobs.len() >= self.budget.in_flight
            {
                break;
            }
            if self
                .retry_after
                .get(&k)
                .is_some_and(|frame| *frame > self.frame)
                || self.pending.contains(&k)
                || self.pages.contains_key(&k)
            {
                continue;
            }
            let (i, l, n) = k;
            let level = self.parts[i].as_ref().unwrap().levels[l - 1];
            let start = n * PAGE_BYTES;
            let range = Level {
                offset: level.offset + start as u64,
                bytes: (level.bytes - start).min(PAGE_BYTES),
            };
            jobs.push((
                k,
                range,
                adapter,
                Arc::new(Permit {
                    _owner: self.permits.clone(),
                }),
            ));
        }
        if !jobs.is_empty() {
            let submitted: Vec<_> = jobs.iter().map(|(k, l, _, _)| (*k, l.bytes)).collect();
            if self.jobs.try_send(jobs).is_ok() {
                for (k, n) in submitted {
                    self.pending.insert(k);
                    self.metrics.read(k, n);
                }
            }
        }
        for key in &admission.requests {
            if !dispatch || uploaded >= upload_limit {
                break;
            }
            let Some(pending) = self.pages.get_mut(key) else {
                continue;
            };
            let page = &pending.page;
            let Some(stage) = page.stage.as_ref() else {
                continue;
            };
            if pending.ticket.as_ref().and_then(Weak::upgrade).is_some() {
                continue;
            }
            let dst = g.physical_page(page.lease.clone());
            let src = g.upload_page(stage.upload.clone(), page.lease.clone());
            let idx = g.pass(
                "Upload streamed page",
                Op::CopyStreamPage {
                    src,
                    dst,
                    bytes: stage.upload.bytes(),
                },
            );
            g.mark_external_effect(idx)?;
            let t = Arc::new(CopyTicket {
                lease: page.lease.clone(),
                _permit: stage.permit.clone(),
            });
            pending.ticket = Some(Arc::downgrade(&t));
            g.page_copies.push((idx, t));
            uploaded += 1;
        }
        let mut parts = vec![];
        let mut bindings = vec![];
        let mut roots = BTreeMap::new();
        for (i, p) in self.parts.iter().enumerate() {
            let Some(p) = p else {
                parts.push(None);
                continue;
            };
            let mut selected = allowed[i];
            while p.root_offset(selected).is_none() {
                let n = p.levels[selected - 1].bytes.div_ceil(PAGE_BYTES);
                if (0..n).all(|j| {
                    self.pages
                        .get(&(i, selected, j))
                        .is_some_and(|p| p.page.lease.complete.load(Ordering::Acquire))
                }) {
                    break;
                }
                selected -= 1;
            }
            parts.push(Some(Binding {
                level: selected,
                first_page: bindings.len() as u32,
                word_offset: p.root_offset(selected).unwrap_or(0) as u32,
            }));
            if dispatch {
                self.last_visible[i] = selected;
                if self.waiting.get(&i).is_some_and(|v| v.0 <= selected)
                    && let Some((_, _, start)) = self.waiting.remove(&i)
                {
                    self.metrics.available(start);
                }
            }
            if p.root_offset(selected).is_some() {
                let root = *roots.entry(p.root.as_ptr()).or_insert_with(|| {
                    let root = g.resident_input("Pinned stream root", p.root.clone());
                    g.stream_roots.insert(root);
                    root
                });
                bindings.push(root)
            } else {
                for j in 0..p.levels[selected - 1].bytes.div_ceil(PAGE_BYTES) {
                    bindings.push(g.physical_page(self.pages[&(i, selected, j)].page.lease.clone()))
                }
            }
        }
        let table = g.address_table(bindings);
        self.metrics.stats.deferred_units = self.waiting.len();
        self.metrics.finish();
        Ok(View {
            table,
            parts,
            uploads: uploaded,
            stats: self.metrics.stats,
        })
    }
    /// Detach an asset now. Existing frame graphs keep their immutable binding and GPU leases.
    pub fn unload_part(&mut self, part: usize) -> Result<()> {
        let entry = self
            .parts
            .get_mut(part)
            .ok_or_else(|| anyhow::anyhow!("unknown stream part"))?;
        entry.take();
        self.minimum.remove(&part);
        self.waiting.remove(&part);
        self.pages.retain(|key, _| key.0 != part);
        self.admission = None;
        Ok(())
    }
    /// Allocate one feedback target for all contributing passes in this view.
    pub fn request_buffer(&self, g: &mut Graph) -> Resource {
        g.input(
            "Visible streaming units",
            feedback::buffer(self.parts.len()),
        )
    }
    /// Call after the last writer; the RHI publishes this immutable view's feedback upon retirement.
    pub fn observe_requests(&self, g: &mut Graph, buffer: Resource, secondary: bool) {
        g.pass(
            "Read visible streaming units",
            Op::Counters {
                buffer,
                first: 0,
                labels: vec![],
            },
        );
        g.stream_feedback.insert(
            buffer,
            FeedbackSink {
                reports: self.feedback.clone(),
                frame: self.frame,
                secondary,
                units: self.parts.len(),
            },
        );
    }
    pub fn set_cache_frames(&mut self, frames: u64) {
        self.cache_frames = frames;
    }
    /// A world/physics consumer can pin the minimum detail of a unit. Zero removes its vote.
    pub fn set_minimum(&mut self, unit: usize, level: usize) -> Result<()> {
        let part = self
            .parts
            .get(unit)
            .and_then(Option::as_ref)
            .ok_or_else(|| anyhow::anyhow!("unknown streaming unit"))?;
        ensure!(level <= part.levels.len(), "invalid minimum stream level");
        if level == 0 {
            self.minimum.remove(&unit);
        } else {
            self.minimum
                .insert(unit, ((level as u32) << 24) | PRIORITY_MASK);
        }
        Ok(())
    }
    pub fn set_target_frame_ms(&mut self, ms: f64) -> Result<()> {
        ensure!(ms.is_finite() && ms > 0., "invalid frame-time target");
        self.frame_budget_ms = ms;
        Ok(())
    }
}

impl Drop for Streamer {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn make(path: &std::path::Path, byte: u8) -> Result<Package> {
        Package::cook(path, vec![vec![vec![byte; 4], vec![byte; PAGE_BYTES]]])
    }
    #[test]
    fn independent_packages_share_budget_and_explicit_unload_discards_late_io() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let p = dir.path().join("a.pages");
        let a = make(&p, 1)?;
        assert!(make(&p, 9).is_err());
        assert_eq!(Package::open(&p)?.parts[0].root[0], 1);
        let b = make(&dir.path().join("b.pages"), 2)?;
        let mut s = Streamer::from_packages(
            vec![a, b],
            Budget {
                requests: 1,
                upload_pages: 1,
                in_flight: 1,
                pool_pages: 1,
            },
        )?;
        s.set_minimum(0, 1)?;
        s.set_minimum(1, 1)?;
        let mut old = Graph::new([1, 1]);
        let v = s.bind(&mut old, None)?;
        assert_eq!(s.pending.len(), 1);
        assert!(v.parts[0].is_some());
        s.unload_part(0)?;
        let start = std::time::Instant::now();
        loop {
            let mut g = Graph::new([1, 1]);
            let v = s.bind(&mut g, None)?;
            assert!(v.parts[0].is_none());
            assert!(v.uploads <= 1 && s.pending.len() <= 1);
            if let Some((_, ticket)) = g.page_copies.first() {
                assert_eq!(s.pages.keys().next().unwrap().0, 1);
                ticket.lease.complete.store(true, Ordering::Release);
                break;
            }
            ensure!(start.elapsed().as_secs() < 4, "no second package upload");
            std::thread::yield_now();
        }
        let mut g = Graph::new([1, 1]);
        assert_eq!(s.bind(&mut g, None)?.parts[1].unwrap().level, 1);
        s.unload_part(1)?;
        let mut next = Graph::new([1, 1]);
        assert!(s.bind(&mut next, None)?.parts.iter().all(Option::is_none));
        assert_eq!(g.physical_pages.len(), 1);
        assert!(next.physical_pages.is_empty());
        Ok(())
    }

    #[test]
    fn admission_reserves_high_benefit_complete_groups_and_keeps_a_reachable_fallback() -> Result<()>
    {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("groups.pages");
        let mut payloads = vec![vec![
            vec![0; 4],
            vec![1; PAGE_BYTES],
            vec![2; PAGE_BYTES * 4],
        ]];
        payloads.push(vec![vec![0; 4], vec![5; PAGE_BYTES * 2]]);
        let p = Package::cook(&path, payloads)?;
        let parts = p.parts.into_iter().map(Some).collect::<Vec<_>>();
        let demand = BTreeMap::from([(0, 2 << 24 | 4), (1, 1 << 24 | 15)]);
        let selection = Admission::new(&parts, &demand, 3, &Default::default(), 33);
        assert_eq!(selection.levels, [1, 1]);
        assert_eq!(selection.requests, [(1, 1, 0), (1, 1, 1), (0, 1, 0)]);
        let small = Admission::new(&parts, &demand, 1, &Default::default(), 34);
        assert_eq!(small.levels, [1, 0]); // Never reserve an undrawable half group.
        Ok(())
    }
    #[test]
    fn packed_levels_keep_word_offsets_and_independent_units_exceed_eight() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("joint.pages");
        let data = (0..40)
            .map(|_| vec![vec![0; 4], vec![3; 16], vec![7; 64]])
            .collect();
        Package::cook(&path, data)?;
        assert_eq!(path.metadata()?.len(), 0);
        let package = Package::open(&path)?;
        assert_eq!(&package.parts[39].root[20..24], &[7; 4]);
        let mut s = Streamer::new(package, Default::default())?;
        s.set_minimum(39, 2)?;
        let mut g = Graph::new([1, 1]);
        let view = s.bind(&mut g, None)?;
        assert_eq!(view.parts[39].unwrap().word_offset, 5);
        assert_eq!(view.parts[0].unwrap().level, 0);
        assert_eq!(view.stats.reads, 0);
        assert_eq!(g.stream_roots.len(), 1);
        Ok(())
    }
    #[test]
    fn cached_pages_survive_a_short_reversal_and_retry_after_an_io_error() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("cold.pages");
        let mut s = Streamer::new(make(&path, 3)?, Default::default())?;
        s.set_cache_frames(3);
        s.set_minimum(0, 1)?;
        std::fs::File::create(&path)?;
        let started = Instant::now();
        loop {
            let mut g = Graph::new([1, 1]);
            s.bind(&mut g, None)?;
            if s.last_error.is_some() {
                break;
            }
            ensure!(started.elapsed().as_secs() < 5, "missing failure");
            std::thread::yield_now();
        }
        std::fs::write(&path, vec![3; PAGE_BYTES])?;
        loop {
            let mut g = Graph::new([1, 1]);
            s.bind(&mut g, None)?;
            if let Some((_, ticket)) = g.page_copies.first() {
                ticket.lease.complete.store(true, Ordering::Release);
                break;
            }
            ensure!(started.elapsed().as_secs() < 5, "retry did not recover");
            std::thread::yield_now();
        }
        let mut g = Graph::new([1, 1]);
        let read = s.bind(&mut g, None)?.stats.reads;
        s.set_minimum(0, 0)?;
        let mut g = Graph::new([1, 1]);
        assert_eq!(s.bind(&mut g, None)?.parts[0].unwrap().level, 0);
        s.set_minimum(0, 1)?;
        let mut g = Graph::new([1, 1]);
        let v = s.bind(&mut g, None)?;
        assert_eq!(v.parts[0].unwrap().level, 1);
        assert_eq!(v.stats.reads, read);
        s.set_minimum(0, 0)?;
        for _ in 0..5 {
            s.bind(&mut Graph::new([1, 1]), None)?;
        }
        assert!(s.pages.is_empty());
        Ok(())
    }
    #[test]
    fn short_disk_read_keeps_rendering_root() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let p = dir.path().join("missing.pages");
        let package = make(&p, 3)?;
        std::fs::File::create(p)?;
        let mut s = Streamer::new(package, Default::default())?;
        s.set_minimum(0, 1)?;
        let start = std::time::Instant::now();
        loop {
            let mut g = Graph::new([1, 1]);
            let v = s.bind(&mut g, None)?;
            assert_eq!(v.parts[0].unwrap().level, 0);
            assert_eq!(v.uploads, 0);
            if s.last_error.is_some() {
                break;
            }
            ensure!(start.elapsed().as_secs() < 4, "no I/O error");
            std::thread::yield_now();
        }
        Ok(())
    }
}
