//! Buffer-only Vulkan memory blocks. Ranges return here when their timeline-safe owners drop.
use super::*;
use std::{ops::Range, ptr::NonNull};

#[derive(Clone, Copy)]
pub(super) enum Target {
    Buffer(vk::Buffer, u64),
    Image(vk::Image),
}

impl Target {
    fn requirements(self, g: &Gpu) -> (vk::MemoryRequirements, bool) {
        let mut dedicated = vk::MemoryDedicatedRequirements::default();
        let mut result = vk::MemoryRequirements2::builder().push_next(&mut dedicated);
        unsafe {
            match self {
                Self::Buffer(b, _) => g.dev.get_buffer_memory_requirements2(
                    &vk::BufferMemoryRequirementsInfo2::builder().buffer(b),
                    &mut result,
                ),
                Self::Image(i) => g.dev.get_image_memory_requirements2(
                    &vk::ImageMemoryRequirementsInfo2::builder().image(i),
                    &mut result,
                ),
            }
        }
        (
            result.memory_requirements,
            dedicated.requires_dedicated_allocation != 0
                || dedicated.prefers_dedicated_allocation != 0,
        )
    }

    fn allocate_memory(
        self,
        g: &Gpu,
        ty: u32,
        bytes: u64,
        dedicated: bool,
    ) -> Result<vk::DeviceMemory, vk::ErrorCode> {
        let mut info = vk::MemoryAllocateInfo::builder()
            .allocation_size(bytes)
            .memory_type_index(ty);
        let mut addr =
            vk::MemoryAllocateFlagsInfo::builder().flags(vk::MemoryAllocateFlags::DEVICE_ADDRESS);
        if matches!(self, Self::Buffer(..)) {
            info = info.push_next(&mut addr);
        }
        let mut owner = match self {
            Self::Buffer(b, _) => vk::MemoryDedicatedAllocateInfo::builder().buffer(b),
            Self::Image(i) => vk::MemoryDedicatedAllocateInfo::builder().image(i),
        };
        if dedicated {
            info = info.push_next(&mut owner);
        }
        unsafe { g.dev.allocate_memory(&info, None) }
    }
}

#[derive(Clone, Copy)]
struct Mapping(NonNull<u8>);
// Each live Buffer owns a disjoint bound interval. Only &mut Buffer exposes its interval
// after the RHI's timeline retirement. The block stays mapped until its last interval returns.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

#[derive(Clone, Copy, Default)]
pub(super) struct Backing {
    pub memory: vk::DeviceMemory,
    mapping: Option<Mapping>,
    purpose: usize,
    capacity: u64,
    pub used: u64,
}

impl Backing {
    /// The caller must exclusively own the bound range and retire conflicting GPU accesses.
    pub unsafe fn host_slice(&mut self, offset: u64, size: usize) -> Option<&mut [u8]> {
        self.mapping.map(|base| unsafe {
            std::slice::from_raw_parts_mut(base.0.as_ptr().add(offset as usize), size)
        })
    }
}

struct FreeRanges(Vec<Range<u64>>);

impl FreeRanges {
    fn new(bytes: u64) -> Self {
        Self(std::iter::once(0..bytes).collect())
    }
    fn take(&mut self, bytes: u64, alignment: u64) -> Option<u64> {
        let (i, start, end) = self.0.iter().enumerate().find_map(|(i, r)| {
            let start = r.start.checked_next_multiple_of(alignment)?;
            let end = start.checked_add(bytes)?;
            (end <= r.end).then_some((i, start, end))
        })?;
        let old = self.0.remove(i);
        if end < old.end {
            self.0.insert(i, end..old.end);
        }
        if old.start < start {
            self.0.insert(i, old.start..start);
        }
        Some(start)
    }
    fn release(&mut self, mut r: Range<u64>) {
        let mut i = self.0.partition_point(|span| span.start < r.start);
        if i > 0 && self.0[i - 1].end == r.start {
            i -= 1;
            r.start = self.0.remove(i).start;
        }
        if i < self.0.len() && self.0[i].start == r.end {
            r.end = self.0.remove(i).end;
        }
        self.0.insert(i, r);
    }
}

struct Block {
    stream: bool,
    backing: Backing,
    ty: u32,
    bytes: u64,
    free: FreeRanges,
}

#[derive(Default)]
pub(super) struct Allocator {
    pub stats: ManagedMemory,
    blocks: Vec<Block>,
    tracker: purposes::Tracker,
}

impl Allocator {
    pub fn allocate(
        &mut self,
        g: &Gpu,
        target: Target,
        flags: vk::MemoryPropertyFlags,
        prefer_local: bool,
        tag: Purpose<'_>,
    ) -> Result<(Backing, AllocationInfo)> {
        let (mut backing, info) = self.bind(g, target, flags, prefer_local, tag)?;
        let capacity = match target {
            Target::Buffer(_, bytes) => bytes,
            Target::Image(_) => info.allocated_bytes,
        };
        self.tracker
            .row(backing.purpose)
            .bind(info.allocated_bytes, capacity);
        backing.capacity = capacity;
        backing.used = capacity;
        Ok((backing, info))
    }

    pub fn use_bytes(&mut self, backing: &mut Backing, used: u64) {
        debug_assert!(used <= backing.capacity);
        let row = self.tracker.row(backing.purpose);
        row.used_bytes = row.used_bytes - backing.used + used;
        backing.used = used;
    }

    pub fn snapshot(&mut self) -> ManagedMemory {
        self.stats.submission += 1;
        let mut report = self.stats.clone();
        report.purposes = self.tracker.snapshot(report.submission);
        report
    }

    fn bind(
        &mut self,
        g: &Gpu,
        target: Target,
        flags: vk::MemoryPropertyFlags,
        prefer_local: bool,
        tag: Purpose<'_>,
    ) -> Result<(Backing, AllocationInfo)> {
        let class = tag.class;
        let (req, dedicated) = target.requirements(g);
        let poolable = matches!(target, Target::Buffer(..))
            && !dedicated
            && matches!(
                class,
                Class::Transient | Class::History | Class::Asset | Class::StreamPage
            )
            && req.size <= 8 << 20;
        let compatible = (0..g.memory.memory_type_count).filter(|&ty| {
            req.memory_type_bits & (1 << ty) != 0
                && g.memory.memory_types[ty as usize]
                    .property_flags
                    .contains(flags)
        });
        let local_type = |ty: u32| {
            g.memory.memory_types[ty as usize]
                .property_flags
                .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
        };
        let mut oom = None;
        let mut over_budget = false;
        for ty in compatible
            .clone()
            .filter(|&ty| prefer_local && local_type(ty))
            .chain(compatible.filter(|&ty| !prefer_local || !local_type(ty)))
        {
            let ty_info = g.memory.memory_types[ty as usize];
            let heap_index = ty_info.heap_index;
            let local = g.memory.memory_heaps[heap_index as usize]
                .flags
                .contains(vk::MemoryHeapFlags::DEVICE_LOCAL);
            let id = self.tracker.identify(tag, heap_index, local);
            let mut binding = AllocationInfo {
                allocated_bytes: req.size,
                memory_type: ty,
                heap_index,
                device_local: local,
                ..Default::default()
            };
            if poolable
                && let Some((backing, offset)) = self.try_range(
                    g,
                    target,
                    req.alignment,
                    &binding,
                    (id, class == Class::StreamPage),
                )?
            {
                binding.pooled = true;
                binding.offset = offset;
                self.stats.range(class, local, req.size, true);
                return Ok((
                    Backing {
                        purpose: id,
                        ..backing
                    },
                    binding,
                ));
            }
            if class == Class::StreamPage && req.size > self.stats.stream_available() {
                let limit = self.stats.stream_budget_bytes.expect("limited stream pool");
                return Err(memory::BudgetExceeded {
                    live: limit - self.stats.stream_available(),
                    request: req.size,
                    limit,
                }
                .into());
            }
            // A block can exceed available budget or contiguous memory while the exact range fits.
            if !self.stats.allows(local, req.size) {
                over_budget = true;
                continue;
            }
            match target.allocate_memory(g, ty, req.size, true) {
                Ok(memory) => {
                    self.stats.allocate(class, local, req.size);
                    if class == Class::StreamPage {
                        self.stats.stream_dedicated_bytes += req.size;
                    }
                    self.tracker.row(id).driver_allocated(req.size);
                    return Ok((
                        Backing {
                            memory,
                            purpose: id,
                            ..Default::default()
                        },
                        binding,
                    ));
                }
                Err(e @ vk::ErrorCode::OUT_OF_DEVICE_MEMORY) => oom = Some(e),
                Err(e) => return Err(e.into()),
            }
        }
        if let Some(e) = oom {
            return Err(e.into());
        }
        if over_budget {
            return Err(memory::BudgetExceeded {
                live: self.stats.device_local.total(),
                request: req.size,
                limit: self.stats.budget_bytes.expect("explicit limit"),
            }
            .into());
        }
        anyhow::bail!("no compatible memory type")
    }

    fn try_range(
        &mut self,
        g: &Gpu,
        target: Target,
        alignment: u64,
        info: &AllocationInfo,
        (opener, stream): (usize, bool),
    ) -> Result<Option<(Backing, u64)>> {
        let size = info.allocated_bytes;
        let ty = info.memory_type;
        for block in self
            .blocks
            .iter_mut()
            .filter(|block| block.ty == ty && block.stream == stream)
        {
            if let Some(offset) = block.free.take(size, alignment) {
                return Ok(Some((block.backing, offset)));
            }
        }
        let bytes = if size <= 128 << 10 { 2 << 20 } else { 16 << 20 };
        if (stream && bytes > self.stats.stream_available())
            || !self.stats.allows(info.device_local, bytes)
        {
            return Ok(None);
        }
        match self.open_block(g, target, ty, bytes, info.device_local, (opener, stream)) {
            Ok(block) => {
                let offset = block
                    .free
                    .take(size, alignment)
                    .expect("new block fits first range");
                Ok(Some((block.backing, offset)))
            }
            Err(vk::ErrorCode::OUT_OF_DEVICE_MEMORY | vk::ErrorCode::MEMORY_MAP_FAILED) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn open_block(
        &mut self,
        g: &Gpu,
        target: Target,
        ty: u32,
        bytes: u64,
        local: bool,
        (opener, stream): (usize, bool),
    ) -> Result<&mut Block, vk::ErrorCode> {
        let memory = target.allocate_memory(g, ty, bytes, false)?;
        self.stats.allocate(Class::PoolFree, local, bytes);
        self.tracker.row(opener).driver_allocated(bytes);
        let coherent =
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let mapping = if g.memory.memory_types[ty as usize]
            .property_flags
            .contains(coherent)
        {
            match unsafe {
                g.dev
                    .map_memory(memory, 0, bytes, vk::MemoryMapFlags::empty())
            } {
                Ok(ptr) => Some(Mapping(
                    NonNull::new(ptr.cast()).expect("mapped Vulkan pointer"),
                )),
                Err(error) => {
                    unsafe {
                        g.dev.free_memory(memory, None);
                    }
                    self.stats.free(Class::PoolFree, local, bytes);
                    self.tracker.row(opener).driver_freed(bytes);
                    return Err(error);
                }
            }
        } else {
            None
        };
        self.blocks.push(Block {
            stream,
            backing: Backing {
                memory,
                mapping,
                purpose: opener,
                ..Default::default()
            },
            ty,
            bytes,
            free: FreeRanges::new(bytes),
        });
        self.stats.pool_blocks += 1;
        if stream {
            self.stats.stream_pool_bytes += bytes;
        }
        Ok(self.blocks.last_mut().unwrap())
    }

    pub fn release(&mut self, g: &Gpu, backing: Backing, info: &AllocationInfo) {
        if backing.memory.is_null() {
            return;
        }
        let row = self.tracker.row(backing.purpose);
        let class = row.class;
        row.unbind(info.allocated_bytes, backing.capacity, backing.used);
        if !info.pooled {
            if class == Class::StreamPage {
                self.stats.stream_dedicated_bytes -= info.allocated_bytes;
            }
            unsafe {
                g.dev.free_memory(backing.memory, None);
            }
            self.stats
                .free(class, info.device_local, info.allocated_bytes);
            self.tracker
                .row(backing.purpose)
                .driver_freed(info.allocated_bytes);
            return;
        }
        let i = self
            .blocks
            .iter()
            .position(|b| b.backing.memory == backing.memory)
            .expect("live buffer block");
        let block = &mut self.blocks[i];
        block
            .free
            .release(info.offset..info.offset + info.allocated_bytes);
        self.stats
            .range(class, info.device_local, info.allocated_bytes, false);
        if matches!(block.free.0.as_slice(), [range] if range.start == 0 && range.end == block.bytes)
        {
            let block = self.blocks.swap_remove(i);
            if block.stream {
                self.stats.stream_pool_bytes -= block.bytes;
            }
            unsafe {
                if backing.mapping.is_some() {
                    g.dev.unmap_memory(backing.memory);
                }
                g.dev.free_memory(backing.memory, None);
            }
            self.stats
                .free(Class::PoolFree, info.device_local, block.bytes);
            self.tracker
                .row(block.backing.purpose)
                .driver_freed(block.bytes);
            self.stats.pool_blocks -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_ranges_reuse_and_coalesce_across_both_neighbors() {
        let mut ranges = FreeRanges::new(128);
        assert_eq!(ranges.take(13, 1), Some(0));
        assert_eq!(ranges.take(32, 32), Some(32));
        assert_eq!(ranges.take(16, 8), Some(16));
        assert_eq!(ranges.take(65, 1), None);
        ranges.release(32..64);
        ranges.release(0..13);
        ranges.release(16..32);
        assert_eq!(ranges.take(128, 64), Some(0));
        assert!(ranges.0.is_empty());
    }

    #[test]
    #[ignore = "requires a Vulkan device"]
    fn buffers_share_aligned_blocks_and_return_ranges_after_drop() -> Result<()> {
        let gpu = Gpu::open(false)?;
        let make = |name, bytes| {
            Buffer::new(
                &gpu,
                bytes,
                vk::BufferUsageFlags::STORAGE_BUFFER,
                true,
                Class::Transient.named(name),
            )
        };
        let mut first = make("block opener", 300)?;
        let mut middle = make("middle", 512)?;
        let mut last = make("last", 700)?;
        let offset = middle.allocation.offset;
        assert!(first.allocation.pooled && offset > 0 && last.allocation.offset > offset);
        assert_eq!(gpu.allocator.lock().unwrap().stats.pool_blocks, 1);
        first.mapped(|bytes| {
            bytes.fill(7);
            Ok(())
        })?;
        middle.mapped(|bytes| {
            bytes.fill(8);
            Ok(())
        })?;
        last.mapped(|bytes| {
            bytes.fill(9);
            Ok(())
        })?;
        drop(middle);
        {
            let mut allocator = gpu.allocator.lock().unwrap();
            let committed = allocator.stats.device_local.total();
            allocator.stats.set_budget(Some(committed))?;
        }
        let mut replacement = make("replacement", 512)?;
        assert_eq!(replacement.allocation.offset, offset);
        replacement.mapped(|bytes| {
            bytes.fill(10);
            Ok(())
        })?;
        first.mapped(|bytes| {
            assert!(bytes.iter().all(|&b| b == 7));
            Ok(())
        })?;
        last.mapped(|bytes| {
            assert!(bytes.iter().all(|&b| b == 9));
            Ok(())
        })?;
        let stats = gpu.allocator.lock().unwrap().stats.clone();
        assert_eq!(
            (stats.activity.allocations, stats.suballocations.allocations),
            (1, 4)
        );
        drop((first, last, replacement));
        let mut allocator = gpu.allocator.lock().unwrap();
        assert_eq!(allocator.stats.pool_blocks, 0);
        assert_eq!(allocator.stats.activity.frees, 1);
        assert_eq!(
            allocator.stats.device_local.total() + allocator.stats.nonlocal.total(),
            0
        );
        let snapshot = allocator.snapshot();
        let opener = snapshot
            .purposes
            .iter()
            .find(|p| p.name.as_ref() == "block opener")
            .unwrap();
        assert_eq!((opener.driver.allocations, opener.driver.frees), (1, 1));
        assert!(snapshot.purposes.iter().all(|p| p.bindings.allocations == 1
            && p.bindings.frees == 1
            && p.capacity_bytes == 0));
        allocator.stats.set_budget(Some(1 << 20))?;
        drop(allocator);
        let dedicated = Buffer::device(
            &gpu,
            64 << 10,
            vk::BufferUsageFlags::STORAGE_BUFFER,
            Class::Transient.named("budget fallback"),
        )?;
        assert!(dedicated.allocation.device_local && !dedicated.allocation.pooled);
        assert_eq!(dedicated.allocation.offset, 0);
        Ok(())
    }
}
