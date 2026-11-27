use super::{Key, PAGE_BYTES, Part, feedback::PRIORITY_MASK};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Instant,
};

pub(super) struct Admission {
    pub levels: Vec<usize>,
    // A sorted list is also the request order used when a frame has limited copy capacity.
    pub requests: Vec<Key>,
    pub wanted: BTreeSet<Key>,
}
impl Admission {
    pub fn new(
        parts: &[Option<Part>],
        demand: &BTreeMap<usize, u32>,
        room: usize,
        waiting: &BTreeMap<usize, (usize, u64, Instant)>,
        frame: u64,
    ) -> Self {
        let mut ranked: Vec<_> = demand
            .iter()
            .filter_map(|(&i, &w)| {
                let part = parts.get(i)?.as_ref()?;
                let level = ((w >> 24) as usize).min(part.levels.len());
                let wait = waiting.get(&i).map_or(0, |v| frame.saturating_sub(v.1));
                Some((i, level, w & PRIORITY_MASK, wait))
            })
            .collect();
        // Reserve whole drawable representations. Break equal benefit by age then stable identity.
        ranked.sort_unstable_by_key(|&(unit, _, priority, age)| {
            (std::cmp::Reverse(priority), std::cmp::Reverse(age), unit)
        });
        let mut levels = vec![0; parts.len()];
        let mut requests = Vec::new();
        let mut free = room;
        for &(i, level, _, _) in &ranked {
            let p = parts[i].as_ref().unwrap();
            for l in (1..=level).rev() {
                let n = if p.root_offset(l).is_some() {
                    0
                } else {
                    p.levels[l - 1].bytes.div_ceil(PAGE_BYTES)
                };
                if n <= free {
                    levels[i] = l;
                    free -= n;
                    requests.extend((0..n).map(|j| (i, l, j)));
                    break;
                }
            }
        }
        // Spend spare slots on intermediate fallbacks. Load lower-cost fallbacks before
        // their own fine representation while preserving priority across independent units.
        for &(i, _, _, _) in &ranked {
            let p = parts[i].as_ref().unwrap();
            for l in 1..levels[i] {
                if p.root_offset(l).is_some() {
                    continue;
                }
                let n = p.levels[l - 1].bytes.div_ceil(PAGE_BYTES);
                if n <= free {
                    free -= n;
                    requests.extend((0..n).map(|j| (i, l, j)));
                }
            }
        }
        let order: BTreeMap<_, _> = ranked
            .iter()
            .enumerate()
            .map(|(rank, row)| (row.0, rank))
            .collect();
        requests.sort_unstable_by_key(|&(i, l, page)| (order[&i], l, page));
        let wanted = requests.iter().copied().collect();
        Self {
            levels,
            requests,
            wanted,
        }
    }
}

#[derive(Default, Clone, Copy)]
pub struct StreamStats {
    /// Accepted file reads, including requests which become stale while in flight.
    pub reads: u64,
    pub rereads: u64,
    pub reread_bytes: u64,
    pub requested_bytes: u64,
    /// Wall time from the first observed request for a unit/level to a binding selecting it.
    pub first_use_p95_ms: f64,
    pub latency_samples: u64,
    pub feedback_overflows: u64,
    pub deferred_units: usize,
}
#[derive(Default)]
pub(super) struct Metrics {
    pub stats: StreamStats,
    seen: BTreeSet<Key>,
    samples: VecDeque<f64>,
    changed: bool,
}
impl Metrics {
    pub fn read(&mut self, k: Key, bytes: usize) {
        self.stats.reads += 1;
        self.stats.requested_bytes += bytes as u64;
        if !self.seen.insert(k) {
            self.stats.rereads += 1;
            self.stats.reread_bytes += bytes as u64;
        }
    }
    pub fn available(&mut self, start: Instant) {
        if self.samples.len() == 512 {
            self.samples.pop_front();
        }
        self.samples
            .push_back(start.elapsed().as_secs_f64() * 1000.);
        self.stats.latency_samples += 1;
        self.changed = true;
    }
    pub fn finish(&mut self) {
        if !std::mem::take(&mut self.changed) {
            return;
        }
        let mut samples: Vec<_> = self.samples.iter().copied().collect();
        let index = (samples.len() * 95).div_ceil(100) - 1;
        self.stats.first_use_p95_ms = *samples
            .select_nth_unstable_by(index, |a, b| a.total_cmp(b))
            .1;
    }
}
