//! Completed-frame data shared with telemetry, capture, and the UI.
use crate::rdg::Queue;
use std::{sync::Arc, time::Instant};

#[derive(Default, Clone, serde::Serialize)]
pub struct AllocationInfo {
    /// Driver-required binding range, excluding other ranges and unassigned block space.
    pub allocated_bytes: u64,
    pub offset: u64,
    pub pooled: bool,
    pub memory_type: u32,
    pub heap_index: u32,
    pub device_local: bool,
}

#[derive(Default, Clone, serde::Serialize)]
pub struct FrameMemory {
    pub managed: super::ManagedMemory,
    pub atlas_buffer_bytes: u64,
    pub atlas: Option<AllocationInfo>,
    /// Binding ranges for live RDG resources and the descriptor heap in this frame slot.
    pub device_local_bytes: u64,
    pub nonlocal_bytes: u64,
}

#[derive(Default, Clone, serde::Serialize)]
pub struct FrameProfile {
    pub frames_in_flight: usize,
    pub resident_upload_bytes: u64,
    /// Logical temporary buffer payloads served by an earlier physical allocation in this frame.
    pub transient_reuse_bytes: u64,
    /// Payload bytes written through mapped buffers, excluding resident assets and zero padding.
    pub dynamic_upload_bytes: u64,
    pub scene_table_upload_bytes: u64,
    /// CPU visits to instances during geometry routing and packet preparation.
    pub geometry_instance_visits: usize,
    pub extracted_entities: usize,
    pub updated_instances: usize,
    pub acceleration_builds: usize,
    pub acceleration_updates: usize,
    pub main_ms: f64,
    pub extract_ms: f64,
    pub build_ms: f64,
    pub compile_ms: f64,
    pub rhi_prepare_ms: f64,
    pub acquire_ms: f64,
    pub record_ms: f64,
    pub submit_ms: f64,
    pub present_ms: f64,
    pub retire_ms: f64,
    pub rhi_ms: f64,
    pub gpu_span_ms: Option<f64>,
    pub task_workers: usize,
    pub recording_workers: usize,
    pub overlapping_recordings: usize,
    pub dependency_edges: usize,
    pub resource_barriers: usize,
    pub barrier_calls: usize,
    pub memory_barriers: usize,
    pub image_barriers: usize,
    pub timeline_waits: usize,
    pub timeline_signals: usize,
    pub physical_queues: usize,
    pub wsi_transitions: usize,
    pub wsi_binary_pairs: usize,
}
pub struct Frame {
    pub first_profile: Arc<FrameProfile>,
    pub profile: FrameProfile,
    pub completed: Instant,
    pub sequence: u64,
    pub pixels: Vec<u8>,
    pub counters: std::collections::BTreeMap<&'static str, u64>,
    pub extent: [u32; 2],
    pub passes: Vec<PassReport>,
    pub gpu: String,
    pub memory: FrameMemory,
    pub presentation: Option<String>,
    pub submissions: usize,
}
#[derive(Debug, serde::Serialize)]
pub struct QueueBatch {
    pub queue: usize,
    pub signal: u64,
    /// Timeline points actually waited by vkQueueSubmit2 after wait coalescing.
    pub waits: Vec<(usize, u64)>,
}
pub struct PassReport {
    pub batch: Arc<QueueBatch>,
    /// GPU start and end in milliseconds from this frame's earliest scope.
    pub interval: Option<[f64; 2]>,
    pub phase: crate::rdg::PassPhase,
    pub queue: Queue,
    pub name: String,
    /// Indices in Frame::passes, in submission order.
    pub parents: Vec<usize>,
    pub barriers: Vec<String>,
    pub ms: f64,
    pub raster: Option<RasterCounts>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RasterCounts {
    pub fragment_invocations: u64,
    pub passed_samples: u64,
}
