//! Per-device accounting and eviction requests. Frame plans retain immutable bindings.
use super::*;
use std::sync::atomic::Ordering;

impl Backend {
    /// Soft cap for mandatory roots and hard admission limit for new fine-page allocations.
    pub fn set_stream_budget(&mut self, bytes: u64) {
        self.gpu.allocator.lock().unwrap().stats.stream_budget_bytes = Some(bytes);
    }

    pub(super) fn update_stream_pressure(&self, graph: &Graph) {
        let page_size = crate::streaming::PAGE_BYTES as u64;
        let mut alloc = self.gpu.allocator.lock().unwrap();
        let stats = &mut alloc.stats;
        let cap = stats.stream_budget_bytes.unwrap_or(u64::MAX);
        #[cfg(lab_directstorage)]
        if let Some(adapter) = self.gpu.stream_adapter {
            stats.stream_native_reserve_bytes = cap / 4;
            stats.stream_native_staging_bytes =
                crate::streaming::directstorage::staging_budget(adapter, cap / 4);
        }
        if self.stream_pages.is_empty()
            && self.stream_roots.is_empty()
            && graph.upload_sources.is_empty()
            && graph.stream_roots.is_empty()
            && stats.stream_staging_bytes == 0
            && stats.stream_root_bytes == 0
            && stats.stream_retiring_bytes == 0
        {
            stats.stream_staging_bytes = stats.stream_native_staging_bytes;
            return;
        }
        let mut roots = std::collections::HashSet::new();
        stats.stream_root_bytes = 0;
        for key in &self.stream_roots {
            if let Some(r) = self.resident.get(key) {
                roots.insert(key.0);
                stats.stream_root_bytes += r.buffer.allocation.allocated_bytes;
            }
        }
        // Include newly introduced roots in admission decisions for this same plan.
        for r in &graph.stream_roots {
            if let Storage::Buffer {
                initial: Some(data),
                bytes,
            } = &graph.resources[r.0].1
                && roots.insert(data.as_ptr() as usize)
            {
                stats.stream_root_bytes += *bytes as u64;
            }
        }
        let mut host = std::collections::HashSet::new();
        stats.stream_staging_bytes = stats.stream_native_staging_bytes;
        for g in self
            .pending
            .iter()
            .map(|p| &p.plan.graph)
            .chain(std::iter::once(graph))
        {
            for (source, _) in g.upload_sources.values() {
                match source {
                    crate::streaming::Upload::Host(data) => {
                        if host.insert(data.as_ptr() as usize) {
                            stats.stream_staging_bytes += data.len() as u64;
                        }
                    }
                    #[cfg(lab_directstorage)]
                    crate::streaming::Upload::Direct(_) => {}
                }
            }
        }
        let usage = stats
            .device_local
            .stream_page
            .saturating_add(stats.stream_root_bytes)
            .saturating_add(
                stats
                    .stream_staging_bytes
                    .saturating_sub(stats.stream_native_staging_bytes),
            )
            .saturating_add(
                stats
                    .stream_native_reserve_bytes
                    .max(stats.stream_native_staging_bytes),
            )
            .saturating_add(page_size.saturating_mul(graph.upload_sources.len() as u64));
        let stream_pressure = usage >= cap - cap / 10;
        let device_pressure = stats.budget_bytes.is_some_and(|limit| {
            stats
                .device_local
                .total()
                .saturating_add(stats.stream_native_staging_bytes)
                >= limit - limit / 10
        });
        let mut blocks = std::collections::BTreeMap::<_, (usize, Vec<_>)>::new();
        stats.stream_retiring_bytes = 0;
        for (lease, buffer) in self.stream_pages.values() {
            let Some(lease) = lease.upgrade() else {
                stats.stream_retiring_bytes += buffer.size;
                continue;
            };
            if lease.detached.load(Ordering::Acquire) {
                stats.stream_retiring_bytes += buffer.size;
                continue;
            }
            if stream_pressure || device_pressure {
                let (hot, cold) = blocks.entry(buffer.memory_block().as_raw()).or_default();
                if lease.cold.load(Ordering::Acquire) && !lease.evict.load(Ordering::Acquire) {
                    cold.push(lease);
                } else {
                    *hot += 1;
                }
            }
        }
        // Reclaim nearly empty blocks first; each owner consumes the flag at its next bind.
        let mut blocks: Vec<_> = blocks.into_values().collect();
        blocks.sort_by_key(|(hot, cold)| (*hot, cold.len()));
        let target = if device_pressure {
            page_size.saturating_mul(8)
        } else {
            usage.saturating_sub(cap - cap / 4)
        };
        let mut requested = stats.stream_retiring_bytes;
        for (_, cold) in blocks {
            for lease in cold {
                if requested >= target {
                    return;
                }
                lease.evict.store(true, Ordering::Release);
                stats.stream_eviction_requests += 1;
                requested += page_size;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        render::FrameGraph,
        streaming::{Budget, PAGE_BYTES, Package, Streamer},
    };
    #[test]
    #[ignore = "requires Vulkan 1.4 descriptor heaps"]
    fn publishes_batched_copies_before_retiring_the_render_frame() -> Result<()> {
        let mut gpu = Backend::open()?;
        let adapter = gpu.stream_adapter();
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("tiles.pages");
        let fine = vec![91; PAGE_BYTES * 3];
        let package = Package::cook(&file, vec![vec![vec![0; 4], fine]])?;
        let mut manager = Streamer::new(
            package,
            Budget {
                requests: 4,
                upload_pages: 4,
                in_flight: 4,
                pool_pages: 4,
            },
        )?;
        manager.set_minimum(0, 1)?;
        let workers = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        let deadline = Instant::now() + std::time::Duration::from_secs(15);
        let (plan, leases) = loop {
            let mut g = FrameGraph::new([1, 1]);
            manager.bind(&mut g, adapter)?;
            if g.page_copies.len() == 3 {
                let leases: Vec<_> = g.page_copies.iter().map(|(_, t)| t.lease.clone()).collect();
                let output = g.input("readback", vec![0; 4]);
                g.set_output(output);
                break (g.compile()?, leases);
            }
            ensure!(Instant::now() < deadline, "timed out reading batch");
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        assert!(gpu.enqueue(plan, &workers, None)?.is_none());
        while leases.iter().any(|l| !l.complete.load(Ordering::Acquire)) {
            gpu.timelines.publish_stream_copies(&gpu.pending)?;
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for copy point"
            );
            std::thread::yield_now();
        }
        assert_eq!(
            gpu.pending.len(),
            1,
            "early publication must not consume a render slot"
        );
        let frame = gpu.retire_next().unwrap()?;
        if std::env::var_os("RENDER_LAB_REQUIRE_DIRECTSTORAGE").is_some() {
            assert_eq!(frame.counters["stream DirectStorage uploads"], 3);
            assert_eq!(frame.counters["stream DirectStorage source batches"], 1);
        }
        drop(leases);
        drop(manager);
        let _ = gpu.retire_ready();
        assert!(!gpu.has_stream_allocations());
        Ok(())
    }

    #[test]
    #[ignore = "requires Vulkan 1.4 descriptor heaps"]
    fn device_pressure_reclaims_a_cold_owner_without_releasing_a_queued_reference() -> Result<()> {
        let mut gpu = Backend::open()?;
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("one.pages");
        Package::cook(&file, vec![vec![vec![0; 4], vec![17; PAGE_BYTES]]])?;
        let mut owners: Vec<_> = (0..2)
            .map(|_| Streamer::new(Package::open(&file)?, Budget::default()))
            .collect::<Result<_>>()?;
        let workers = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        let bind = |s: &mut Streamer| -> Result<(usize, FramePlan)> {
            let mut g = FrameGraph::new([1, 1]);
            let v = s.bind(&mut g, None)?;
            let level = v.parts[0].unwrap().level;
            let output = g.input("readback", vec![0; 4]);
            g.set_output(output);
            Ok((level, g.compile()?))
        };
        let deadline = Instant::now() + std::time::Duration::from_secs(15);
        for owner in &mut owners {
            owner.set_cache_frames(120);
            owner.set_minimum(0, 1)?;
            loop {
                let (level, p) = bind(owner)?;
                gpu.execute(p, &workers)?;
                if level == 1 {
                    break;
                }
                ensure!(Instant::now() < deadline, "owner never refined");
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        let (level, held) = bind(&mut owners[0])?;
        assert_eq!(level, 1);
        owners[0].set_minimum(0, 0)?;
        gpu.set_stream_budget(PAGE_BYTES as u64);
        let (level, p) = bind(&mut owners[0])?;
        assert_eq!(level, 0);
        gpu.execute(p, &workers)?;
        let (_, p) = bind(&mut owners[0])?;
        let pressured = gpu.execute(p, &workers)?;
        assert_eq!(
            pressured.memory.managed.stream_retiring_bytes,
            PAGE_BYTES as u64
        );
        assert_eq!(
            pressured.memory.managed.device_local.stream_page,
            (2 * PAGE_BYTES) as u64
        );
        drop(held);
        let (level, p) = bind(&mut owners[1])?;
        assert_eq!(level, 1, "pressure must preserve the hot owner");
        let f = gpu.execute(p, &workers)?;
        assert_eq!(f.memory.managed.device_local.stream_page, PAGE_BYTES as u64);
        assert_eq!(f.memory.managed.stream_retiring_bytes, 0);
        assert!(f.memory.managed.stream_eviction_requests >= 1);
        Ok(())
    }

    #[test]
    fn global_stream_allowance_accounts_for_slab_reserve_and_retirement() {
        let mut memory = ManagedMemory {
            stream_budget_bytes: Some(4 << 20),
            stream_root_bytes: 64 << 10,
            stream_pool_bytes: 2 << 20,
            stream_dedicated_bytes: 64 << 10,
            stream_native_reserve_bytes: 1 << 20,
            stream_staging_bytes: 320 << 10,
            stream_native_staging_bytes: 256 << 10,
            ..Default::default()
        };
        let remaining = memory.stream_available();
        assert_eq!(remaining, (1 << 20) - 3 * (64 << 10));
        // A logically evicted page is still inside the already charged Vulkan block.
        memory.stream_retiring_bytes = 64 << 10;
        assert_eq!(memory.stream_available(), remaining);
        memory.stream_budget_bytes = Some(1 << 20);
        assert_eq!(memory.stream_available(), 0);
    }
}
