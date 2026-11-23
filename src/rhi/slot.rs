//! Physical allocations owned by one timeline-retired frame slot.
use super::*;

#[derive(Hash, PartialEq, Eq)]
struct BindingKey {
    label: String,
    owner: Option<u64>,
    occurrence: usize,
}

pub(super) struct FrameSlot {
    // Drop command buffers before releasing the descriptor heap address.
    pub recorders: Vec<Recorder>,
    pub heap: Heap,
    pub resources: Vec<Physical>,
    layout: Vec<BindingKey>,
    // Only opted-in work buffers survive a missing producer, for a bounded number of slot uses.
    idle: HashMap<BindingKey, (Physical, u64)>,
    epoch: u64,
}

impl FrameSlot {
    pub fn new(g: &Arc<Gpu>, index: usize) -> Result<Self> {
        Ok(Self {
            heap: Heap::new(g, 16, index)?,
            resources: Vec::new(),
            recorders: Vec::new(),
            layout: Vec::new(),
            idle: HashMap::new(),
            epoch: 0,
        })
    }

    pub fn clear(&mut self) {
        self.resources.clear();
        self.layout.clear();
        self.idle.clear();
        self.heap.slots.fill(None);
    }

    pub fn bind_layout(&mut self, graph: &Graph) {
        self.epoch += 1;
        self.idle.retain(|_, (_, last)| self.epoch - *last <= 120);
        let bindings = graph.resources.iter().enumerate().map(|(i, (name, _))| {
            (
                name.as_str(),
                graph.scratch_owners.get(&Resource(i)).copied(),
            )
        });
        if self.layout.len() == graph.resources.len()
            && self
                .layout
                .iter()
                .zip(bindings.clone())
                .all(|(key, (name, owner))| key.label == name && key.owner == owner)
        {
            return;
        }

        // An inserted pass may shift every following descriptor index. Rebind by identity
        // before checking Vulkan usage, storage shape, and byte range in Backend::prepare.
        let mut available = std::mem::take(&mut self.idle);
        for (key, item) in self.layout.drain(..).zip(self.resources.drain(..)) {
            available.insert(key, (item, self.epoch));
        }
        let mut occurrences = HashMap::new();
        for (name, owner) in bindings {
            let next = occurrences.entry((name, owner)).or_insert(0);
            let key = BindingKey {
                label: name.into(),
                owner,
                occurrence: *next,
            };
            *next += 1;
            let item = available
                .remove(&key)
                .map_or(Physical::Vacant, |(item, _)| item);
            self.resources.push(item);
            self.layout.push(key);
        }
        available.retain(|key, (item, _)| {
            if key.owner.is_some()
                && let Physical::Buffer(buffer) = item
            {
                buffer.use_bytes(0);
                true
            } else {
                false
            }
        });
        self.idle = available;
        self.heap.slots.fill(None);
    }
}

// Alternating 1x and 1.5x power-of-two tiers cap growth slack at 50%.
fn tier(bytes: u64, limit: u64) -> u64 {
    let upper = bytes.next_power_of_two();
    let mid = upper - upper / 4;
    (if bytes <= mid { mid } else { upper })
        .min(limit)
        .max(bytes)
}

pub(super) fn allocate_scratch(
    g: &Arc<Gpu>,
    bytes: u64,
    usage: vk::BufferUsageFlags,
    tag: Purpose<'_>,
) -> Result<Buffer> {
    let limit = u64::from(g.limits.max_storage_buffer_range);
    ensure!(bytes <= limit, "storage buffer exceeds device limit");
    let headroom = {
        let allocator = g.allocator.lock().unwrap();
        let ledger = &allocator.stats;
        ledger.budget_bytes.map_or(limit, |cap| {
            limit.min(cap.saturating_sub(ledger.device_local.total()))
        })
    };
    let capacity = tier(bytes, headroom);
    let memory_pressure = |error: &anyhow::Error| {
        error.is::<memory::BudgetExceeded>()
            || error.downcast_ref::<vk::ErrorCode>() == Some(&vk::ErrorCode::OUT_OF_DEVICE_MEMORY)
    };
    if capacity > bytes {
        match Buffer::device(g, capacity, usage, tag) {
            Err(error) if memory_pressure(&error) => {}
            result => return result,
        }
    }
    match Buffer::device(g, bytes, usage, tag) {
        Err(error) if memory_pressure(&error) => Buffer::new(g, bytes, usage, true, tag),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::tier;

    #[test]
    fn capacity_tiers_respect_range_and_budget_without_truncating_requests() {
        for (requested, limit, expected) in [
            (1024, 4096, 1024),
            (1025, 4096, 1536),
            (1537, 4096, 2048),
            (1537, 1600, 1600),
            (1537, 0, 1537),
            (u32::MAX as u64, u32::MAX as u64, u32::MAX as u64),
        ] {
            assert_eq!(tier(requested, limit), expected);
        }
    }
}
