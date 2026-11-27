//! Device allocation accounting. Every successful allocation and free occurs on the RHI path.
use anyhow::{Result, ensure};
use serde::Serialize;

#[derive(Debug)]
pub(super) struct BudgetExceeded {
    pub live: u64,
    pub request: u64,
    pub limit: u64,
}
impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "device-local budget exceeded: {} bytes live, {} requested, limit {}",
            self.live, self.request, self.limit
        )
    }
}
impl std::error::Error for BudgetExceeded {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    #[default]
    Transient,
    History,
    Asset,
    StreamPage,
    Acceleration,
    BuildScratch,
    Infrastructure,
    PoolFree,
}

#[derive(Default, Debug, Clone, Serialize)]
pub struct MemoryBreakdown {
    pub transient: u64,
    pub history: u64,
    pub asset: u64,
    pub stream_page: u64,
    pub acceleration: u64,
    pub build_scratch: u64,
    pub infrastructure: u64,
    /// Committed block space currently available for another buffer binding.
    pub pool_free: u64,
}
impl MemoryBreakdown {
    pub fn total(&self) -> u64 {
        self.transient
            + self.history
            + self.asset
            + self.stream_page
            + self.acceleration
            + self.build_scratch
            + self.infrastructure
            + self.pool_free
    }
    fn field(&mut self, class: Class) -> &mut u64 {
        match class {
            Class::Transient => &mut self.transient,
            Class::History => &mut self.history,
            Class::StreamPage => &mut self.stream_page,
            Class::Asset => &mut self.asset,
            Class::Acceleration => &mut self.acceleration,
            Class::BuildScratch => &mut self.build_scratch,
            Class::Infrastructure => &mut self.infrastructure,
            Class::PoolFree => &mut self.pool_free,
        }
    }
}

#[derive(Default, Debug, Clone, Serialize)]
pub struct AllocationActivity {
    pub allocations: u64,
    pub allocated_bytes: u64,
    pub frees: u64,
    pub freed_bytes: u64,
}

impl AllocationActivity {
    pub fn is_empty(&self) -> bool {
        self.allocations == 0 && self.frees == 0
    }
    /// Both samples must come from the same backend in submission order.
    pub fn since(&self, previous: &Self) -> Self {
        Self {
            allocations: self.allocations - previous.allocations,
            allocated_bytes: self.allocated_bytes - previous.allocated_bytes,
            frees: self.frees - previous.frees,
            freed_bytes: self.freed_bytes - previous.freed_bytes,
        }
    }
    pub(super) fn allocated(&mut self, bytes: u64) {
        self.allocations += 1;
        self.allocated_bytes += bytes;
    }
    pub(super) fn freed(&mut self, bytes: u64) {
        self.frees += 1;
        self.freed_bytes += bytes;
    }
}

#[derive(Default, Debug, Clone, Serialize)]
pub struct ManagedMemory {
    /// Live explicitly allocated Vulkan memory across all slots and caches at submission.
    pub device_local: MemoryBreakdown,
    pub nonlocal: MemoryBreakdown,
    /// Per-class device-local high-water marks since backend creation; peaks may occur at different times.
    pub class_peaks: MemoryBreakdown,
    pub peak_device_local_bytes: u64,
    pub budget_bytes: Option<u64>,
    /// Cumulative explicit Vulkan memory traffic on both heaps, sampled at submission.
    pub activity: AllocationActivity,
    /// Bound ranges allocated and returned within ordinary buffer blocks.
    pub suballocations: AllocationActivity,
    pub pool_blocks: usize,
    /// Committed Vulkan blocks dedicated to fixed-size streamed pages.
    pub stream_pool_bytes: u64,
    /// Device-wide allowance across stream owners. Pins have priority over refinements.
    pub stream_budget_bytes: Option<u64>,
    pub stream_dedicated_bytes: u64,
    pub stream_root_bytes: u64,
    /// Bound fine pages detached from the manager but held by plans or native frame slots.
    pub stream_retiring_bytes: u64,
    /// Committed external slabs plus portable source bytes in submitted/uploading graphs.
    pub stream_staging_bytes: u64,
    pub stream_native_staging_bytes: u64,
    /// Reserved part of the allowance for asynchronous D3D12 slabs; zero on portable devices.
    pub stream_native_reserve_bytes: u64,
    pub stream_eviction_requests: u64,
    /// Snapshot sequence, including submissions without a presented image.
    pub submission: u64,
    pub purposes: std::sync::Arc<[super::PurposeMemory]>,
}
impl ManagedMemory {
    pub(super) fn allows(&self, local: bool, bytes: u64) -> bool {
        !local
            || self.budget_bytes.is_none_or(|cap| {
                bytes
                    <= cap
                        .saturating_sub(self.device_local.total())
                        .saturating_sub(self.stream_native_staging_bytes)
            })
    }
    pub(super) fn stream_available(&self) -> u64 {
        let resident = self
            .stream_pool_bytes
            .saturating_add(self.stream_dedicated_bytes)
            .saturating_add(self.stream_root_bytes)
            .saturating_add(
                self.stream_staging_bytes
                    .saturating_sub(self.stream_native_staging_bytes),
            )
            .saturating_add(
                self.stream_native_reserve_bytes
                    .max(self.stream_native_staging_bytes),
            );
        self.stream_budget_bytes
            .unwrap_or(u64::MAX)
            .saturating_sub(resident)
    }
    pub(super) fn set_budget(&mut self, bytes: Option<u64>) -> Result<()> {
        ensure!(
            bytes.is_none_or(|cap| cap >= self.device_local.total()),
            "budget is below current live device-local allocations"
        );
        self.budget_bytes = bytes;
        Ok(())
    }
    pub(super) fn allocate(&mut self, class: Class, local: bool, bytes: u64) {
        self.activity.allocated(bytes);
        if local {
            let entry = self.device_local.field(class);
            *entry += bytes;
            let peak = self.class_peaks.field(class);
            *peak = (*peak).max(*entry);
            self.peak_device_local_bytes =
                self.peak_device_local_bytes.max(self.device_local.total());
        } else {
            *self.nonlocal.field(class) += bytes;
        }
    }
    pub(super) fn free(&mut self, class: Class, local: bool, bytes: u64) {
        // Partially constructed Vulkan objects may be dropped before memory was allocated.
        if bytes == 0 {
            return;
        }
        self.activity.freed(bytes);
        let usage = if local {
            &mut self.device_local
        } else {
            &mut self.nonlocal
        };
        *usage.field(class) -= bytes;
    }

    pub(super) fn range(&mut self, class: Class, local: bool, bytes: u64, bind: bool) {
        let (from, to) = if bind {
            self.suballocations.allocated(bytes);
            (Class::PoolFree, class)
        } else {
            self.suballocations.freed(bytes);
            (class, Class::PoolFree)
        };
        let usage = if local {
            &mut self.device_local
        } else {
            &mut self.nonlocal
        };
        *usage.field(from) -= bytes;
        *usage.field(to) += bytes;
        if local {
            let peak = self.class_peaks.field(to);
            *peak = (*peak).max(*usage.field(to));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budget_and_peak_cover_overlapping_slots_and_freed_allocations() -> Result<()> {
        let mut stats = ManagedMemory::default();
        stats.set_budget(Some(80))?;
        stats.allocate(Class::Asset, true, 16);
        stats.allocate(Class::Transient, true, 32);
        stats.allocate(Class::Transient, true, 32);
        assert!(!stats.allows(true, 1));
        assert!(stats.allows(false, 256));
        stats.free(Class::Transient, true, 32);
        stats.free(Class::Transient, false, 0);
        stats.allocate(Class::Acceleration, true, 8);
        assert_eq!(stats.device_local.total(), 56);
        assert_eq!(stats.peak_device_local_bytes, 80);
        assert_eq!(stats.class_peaks.transient, 64);
        assert_eq!(
            stats.activity.allocated_bytes - stats.activity.freed_bytes,
            56
        );
        assert_eq!((stats.activity.allocations, stats.activity.frees), (4, 1));
        assert!(stats.set_budget(Some(40)).is_err());
        Ok(())
    }
}
