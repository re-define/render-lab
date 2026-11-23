//! Attribute bindings to their physical owner and driver blocks to the request that opened them.
use super::{AllocationActivity, memory::Class};
use std::{collections::HashMap, sync::Arc};

#[derive(Clone, Copy)]
pub(super) struct Purpose<'a> {
    pub name: &'a str,
    pub class: Class,
    pub owner: Option<u64>,
    pub slot: Option<usize>,
}

impl Class {
    pub(super) fn named(self, name: &str) -> Purpose<'_> {
        Purpose {
            name,
            class: self,
            owner: None,
            slot: None,
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct PurposeMemory {
    pub name: Arc<str>,
    pub heap_index: u32,
    pub device_local: bool,
    pub class: Class,
    pub owner: Option<u64>,
    pub slot: Option<usize>,
    pub capacity_bytes: u64,
    pub used_bytes: u64,
    pub unused_bytes: u64,
    pub last_growth_submission: Option<u64>,
    pub bindings: AllocationActivity,
    pub driver: AllocationActivity,
    pub frame_bindings: AllocationActivity,
    pub frame_driver: AllocationActivity,
}

#[derive(Default)]
pub(super) struct Tracker {
    names: HashMap<Arc<str>, Vec<usize>>,
    rows: Vec<Entry>,
    published: Arc<[PurposeMemory]>,
    changed: bool,
}

struct Entry {
    report: PurposeMemory,
    submitted_capacity: u64,
}

impl Tracker {
    pub fn identify(&mut self, tag: Purpose<'_>, heap_index: u32, device_local: bool) -> usize {
        if let Some(group) = self.names.get(tag.name)
            && let Some(&id) = group.iter().find(|&&id| {
                let row = &self.rows[id].report;
                (row.class, row.owner, row.slot, row.heap_index)
                    == (tag.class, tag.owner, tag.slot, heap_index)
            })
        {
            return id;
        }
        let name: Arc<str> = tag.name.into();
        let id = self.rows.len();
        self.names.entry(name.clone()).or_default().push(id);
        self.rows.push(Entry {
            report: PurposeMemory {
                name,
                heap_index,
                device_local,
                class: tag.class,
                owner: tag.owner,
                slot: tag.slot,
                ..Default::default()
            },
            submitted_capacity: 0,
        });
        id
    }
    pub fn row(&mut self, id: usize) -> &mut PurposeMemory {
        self.changed = true;
        &mut self.rows[id].report
    }
    pub fn snapshot(&mut self, submission: u64) -> Arc<[PurposeMemory]> {
        if !std::mem::take(&mut self.changed) {
            return self.published.clone();
        }
        self.published = self
            .rows
            .iter_mut()
            .filter(|entry| {
                entry.report.bindings.allocations != 0 || entry.report.driver.allocations != 0
            })
            .map(|entry| {
                let r = &mut entry.report;
                if r.capacity_bytes > entry.submitted_capacity {
                    r.last_growth_submission = Some(submission);
                }
                entry.submitted_capacity = r.capacity_bytes;
                r.unused_bytes = r.capacity_bytes - r.used_bytes;
                let snapshot = r.clone();
                // Publish zero per-submission deltas once after the last event, then share the snapshot.
                self.changed |= !r.frame_bindings.is_empty() || !r.frame_driver.is_empty();
                r.frame_bindings = Default::default();
                r.frame_driver = Default::default();
                snapshot
            })
            .collect();
        self.published.clone()
    }
}

impl PurposeMemory {
    pub(super) fn bind(&mut self, bytes: u64, capacity: u64) {
        self.bindings.allocated(bytes);
        self.frame_bindings.allocated(bytes);
        self.capacity_bytes += capacity;
        self.used_bytes += capacity;
    }
    pub(super) fn unbind(&mut self, bytes: u64, capacity: u64, used: u64) {
        self.bindings.freed(bytes);
        self.frame_bindings.freed(bytes);
        self.capacity_bytes -= capacity;
        self.used_bytes -= used;
    }
    pub(super) fn driver_allocated(&mut self, bytes: u64) {
        self.driver.allocated(bytes);
        self.frame_driver.allocated(bytes);
    }
    pub(super) fn driver_freed(&mut self, bytes: u64) {
        self.driver.freed(bytes);
        self.frame_driver.freed(bytes);
    }
}
