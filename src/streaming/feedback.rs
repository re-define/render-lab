use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

/// Unique unit IDs are appended here. Overflow can still be recovered from the priority words.
const LIST: usize = 512;
pub(super) const MAX_UNITS: usize = 16384;
pub(super) const PRIORITY_MASK: u32 = 0x00ff_ffff;
pub(super) fn buffer(units: usize) -> Vec<u8> {
    let mut data = vec![0; 4 * (2 + units.min(LIST) + units)];
    data[..4].copy_from_slice(&(units as u32).to_ne_bytes());
    data
}
#[derive(Default)]
pub(super) struct Reports {
    views: [(u64, BTreeMap<usize, u32>); 2],
    pub gpu_ms: Option<f64>,
    pub overflows: u64,
}
impl Reports {
    pub fn merge(&self, into: &mut BTreeMap<usize, u32>) {
        let main = self.views[0].0;
        for (frame, words) in &self.views {
            if frame.saturating_add(2) < main {
                continue;
            }
            for (&unit, &word) in words {
                let old = into.entry(unit).or_default();
                *old = (*old).max(word);
            }
        }
    }
}
#[derive(Clone)]
pub(crate) struct FeedbackSink {
    pub(super) reports: Arc<Mutex<Reports>>,
    pub(super) frame: u64,
    pub(super) secondary: bool,
    pub(super) units: usize,
}
impl FeedbackSink {
    pub fn publish(&self, data: &[u8]) {
        let units = self.units;
        let cap = units.min(LIST);
        let word = |i: usize| u32::from_ne_bytes(data[i * 4..i * 4 + 4].try_into().unwrap());
        if data.len() < 4 * (2 + cap + units) || word(0) != units as u32 {
            return;
        }
        let mut reports = self.reports.lock().unwrap();
        let view = usize::from(self.secondary);
        if reports.views[view].0 > self.frame {
            return;
        }
        let total = word(1) as usize;
        let mut words = BTreeMap::new();
        let mut read = |id: usize| {
            if id < units {
                let value = word(2 + cap + id);
                if value >> 24 != 0 {
                    words.insert(id, value);
                }
            }
        };
        if total > cap {
            reports.overflows += 1;
            for id in 0..units {
                read(id);
            }
        } else {
            for i in 0..total {
                read(word(2 + i) as usize);
            }
        }
        reports.views[view] = (self.frame, words);
    }
    pub fn report_gpu_ms(&self, ms: f64) {
        if self.secondary || !ms.is_finite() || ms <= 0. {
            return;
        }
        let mut state = self.reports.lock().unwrap();
        state.gpu_ms = Some(state.gpu_ms.map_or(ms, |old| old * 0.8 + ms * 0.2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn publish(target: &FeedbackSink, ids: &[usize]) {
        let units = target.units;
        let mut data = vec![0u32; 2 + units.min(LIST) + units];
        data[0] = units as u32;
        data[1] = ids.len() as u32;
        for (i, &id) in ids.iter().enumerate() {
            if i < LIST {
                data[2 + i] = id as u32;
            }
            data[2 + units.min(LIST) + id] = 1 << 24 | 100;
        }
        target.publish(bytemuck::cast_slice(&data));
    }
    #[test]
    fn overflow_preserves_sparse_requests_and_stale_secondary_views_expire() {
        let reports = Arc::new(Mutex::new(Reports::default()));
        let mut primary = FeedbackSink {
            reports: reports.clone(),
            units: 620,
            secondary: false,
            frame: 1,
        };
        let secondary = FeedbackSink {
            secondary: true,
            ..primary.clone()
        };
        publish(&primary, &(0..513).collect::<Vec<_>>());
        publish(&secondary, &[619]);
        let mut merged = BTreeMap::new();
        reports.lock().unwrap().merge(&mut merged);
        assert_eq!(merged.len(), 514);
        assert_eq!(reports.lock().unwrap().overflows, 1);
        primary.frame = 4;
        publish(&primary, &[610]);
        let older = FeedbackSink {
            frame: 2,
            ..primary.clone()
        };
        publish(&older, &[33]);
        merged.clear();
        reports.lock().unwrap().merge(&mut merged);
        assert_eq!(merged.keys().copied().collect::<Vec<_>>(), [610]);
    }
}
