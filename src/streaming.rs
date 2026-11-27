//! Shader feedback selects detail. A file worker and immutable frame leases decouple I/O from drawing.
#[cfg(lab_directstorage)]
pub(crate) mod directstorage;
mod disk;
mod package;
use crate::rdg::{Graph, Op, Resource};
use anyhow::{Result, ensure};
pub(crate) use disk::Upload;
pub use package::{Level, Package, Part};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    thread,
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
            requests: 4,
            upload_pages: 2,
            in_flight: 8,
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
}
#[derive(Clone, Copy)]
pub struct Binding {
    pub level: usize,
    pub first_page: u32,
}
pub struct View {
    pub table: Resource,
    pub parts: Vec<Option<Binding>>,
    pub uploads: usize,
}
pub struct Streamer {
    parts: Vec<Option<Part>>,
    pool: u64,
    budget: Budget,
    slots: Vec<Weak<Lease>>,
    pages: BTreeMap<Key, Pending>,
    pending: std::collections::BTreeSet<Key>,
    failed: std::collections::BTreeSet<Key>,
    permits: Arc<()>,
    jobs: mpsc::SyncSender<ReadJob>,
    results: mpsc::Receiver<(Key, Result<Staged, String>)>,
    host_only: bool,
    retry_after: BTreeMap<Key, u64>,
    frame: u64,
    cancelled: Arc<AtomicBool>,
    feedback: Arc<AtomicU32>,
    admission: Option<(u32, Vec<usize>, std::collections::BTreeSet<Key>)>,
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
        for package in packages {
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
                && (1..=8).contains(&parts.len()),
            "invalid stream budget or part count"
        );
        for p in parts.iter().flatten() {
            ensure!(p.levels.len() <= 4, "feedback has four bits per part");
        }
        let (jobs, work) = mpsc::sync_channel::<ReadJob>(budget.in_flight);
        let (send, results) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = cancelled.clone();
        thread::Builder::new()
            .name("lab-page-io".into())
            .spawn(move || {
                let mut readers: Vec<_> = paths.iter().map(|_| None).collect();
                for (key, range, adapter, permit) in work {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let file = files[key.0];
                    let path = &paths[file];
                    let result = readers[file]
                        .get_or_insert_with(|| disk::Reader::new(path, adapter))
                        .read(path, range, adapter.is_some())
                        .map(|upload| Staged { upload, permit })
                        .map_err(|e| format!("{e:#}"));
                    if send.send((key, result)).is_err() {
                        break;
                    }
                }
            })?;
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Ok(Self {
            permits: Default::default(),
            host_only: false,
            retry_after: Default::default(),
            frame: 0,
            cancelled,
            parts,
            admission: None,
            pool: NEXT.fetch_add(1, Ordering::Relaxed),
            slots: (0..budget.pool_pages).map(|_| Weak::new()).collect(),
            budget,
            pages: Default::default(),
            pending: Default::default(),
            failed: Default::default(),
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
                    self.last_error = Some(e.clone());
                    self.host_only = true;
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
        let demand = self.feedback.load(Ordering::Acquire);
        if self
            .admission
            .as_ref()
            .is_none_or(|(last, _, _)| *last != demand)
        {
            let mut desired = Vec::new();
            let mut room = self.budget.pool_pages;
            // Always admit complete levels. Divide bounded capacity across independently requested parts.
            for (i, p) in self.parts.iter().enumerate() {
                let wanted = (((demand >> (4 * i)) & 15).count_ones() as usize)
                    .min(p.as_ref().map_or(0, |p| p.levels.len()));
                desired.push(wanted);
            }
            // Reserve capacity coarse-to-fine, round robin across kinds. Keep a reachable fallback.
            let mut wanted = std::collections::BTreeSet::new();
            let mut allowed = vec![0; self.parts.len()];
            for level in 1..=desired.iter().copied().max().unwrap_or(0) {
                for (i, p) in self.parts.iter().enumerate() {
                    if desired[i] < level || allowed[i] + 1 != level {
                        continue;
                    }
                    let n = p.as_ref().unwrap().levels[level - 1]
                        .bytes
                        .div_ceil(PAGE_BYTES);
                    if n <= room {
                        room -= n;
                        allowed[i] = level;
                        wanted.extend((0..n).map(|j| (i, level, j)));
                    }
                }
            }
            self.pages.retain(|k, _| wanted.contains(k));
            self.failed.retain(|k| wanted.contains(k));
            self.retry_after.retain(|k, _| wanted.contains(k));
            self.admission = Some((demand, allowed, wanted));
        }
        let (_, allowed, wanted) = self.admission.as_ref().unwrap();
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
                        self.failed.insert(key);
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
                    },
                );
            }
        }
        let mut requested = 0;
        for &k in wanted {
            if !dispatch
                || requested >= self.budget.requests
                || Arc::strong_count(&self.permits) > self.budget.in_flight
                || self.pending.len() >= self.budget.in_flight
            {
                break;
            }
            if self
                .retry_after
                .get(&k)
                .is_some_and(|frame| *frame > self.frame)
                || self.pending.contains(&k)
                || self.pages.contains_key(&k)
                || self.failed.contains(&k)
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
            if self
                .jobs
                .try_send((
                    k,
                    range,
                    adapter,
                    Arc::new(Permit {
                        _owner: self.permits.clone(),
                    }),
                ))
                .is_ok()
            {
                self.pending.insert(k);
                requested += 1
            }
        }
        for pending in self.pages.values_mut() {
            if !dispatch || uploaded >= self.budget.upload_pages {
                break;
            }
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
        for (i, p) in self.parts.iter().enumerate() {
            let Some(p) = p else {
                parts.push(None);
                continue;
            };
            let mut selected = allowed[i];
            while selected > 0 {
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
            }));
            if selected == 0 {
                let root = g.resident_input("Pinned stream root", p.root.clone());
                g.stream_roots.insert(root);
                bindings.push(root)
            } else {
                for j in 0..p.levels[selected - 1].bytes.div_ceil(PAGE_BYTES) {
                    bindings.push(g.physical_page(self.pages[&(i, selected, j)].page.lease.clone()))
                }
            }
        }
        let table = g.address_table(bindings);
        Ok(View {
            table,
            parts,
            uploads: uploaded,
        })
    }
    /// Detach an asset now. Existing frame graphs keep their immutable binding and GPU leases.
    pub fn unload_part(&mut self, part: usize) -> Result<()> {
        let entry = self
            .parts
            .get_mut(part)
            .ok_or_else(|| anyhow::anyhow!("unknown stream part"))?;
        entry.take();
        self.pages.retain(|key, _| key.0 != part);
        self.admission = None;
        Ok(())
    }
    pub fn observe_demand(&self, g: &mut Graph, buffer: Resource) {
        g.pass(
            "Read completed demand",
            Op::Counters {
                buffer,
                first: 0,
                labels: vec!["visible page demand"],
            },
        );
        g.stream_feedback.insert(buffer, self.feedback.clone());
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
        s.feedback.store(0x11, Ordering::Release);
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
    fn short_disk_read_keeps_rendering_root() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let p = dir.path().join("missing.pages");
        let package = make(&p, 3)?;
        std::fs::File::create(p)?;
        let mut s = Streamer::new(package, Default::default())?;
        s.feedback.store(1, Ordering::Release);
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
