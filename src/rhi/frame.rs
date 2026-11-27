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
    /// Logical atlas bytes visible through the descriptor. atlas.allocated_bytes includes growth capacity.
    pub atlas_buffer_bytes: u64,
    pub atlas: Option<AllocationInfo>,
    /// Binding ranges for live RDG resources and the descriptor heap in this frame slot.
    pub device_local_bytes: u64,
    pub nonlocal_bytes: u64,
}

/// CPU-side observations in microseconds on one engine clock. GPU intervals have their own origin.
#[derive(Default, Clone, serde::Serialize)]
pub struct CpuFrameTimes {
    pub tick: u64,
    pub snapshot: u64,
    pub render: u64,
    pub plan: u64,
    pub rhi: u64,
    pub acquire: u64,
    pub submit: u64,
    pub present_return: Option<u64>,
    pub retired: u64,
}
#[derive(Default, Clone, serde::Serialize)]
pub struct FrameProfile {
    pub cpu_times_us: Option<CpuFrameTimes>,
    pub tick_id: u64,
    pub snapshot_id: u64,
    /// Both views of a tick share the snapshot ID and have distinct submissions.
    pub secondary_view: bool,
    /// Monotonic frame submission ID for this backend, including offscreen frames.
    pub submission_id: u64,
    pub skipped_render_ticks: u64,
    pub history_generation: u64,
    /// Time in the main-to-render channel and from plan completion to RHI preparation.
    pub render_queue_ms: f64,
    pub rhi_queue_ms: f64,
    /// CPU time from simulation tick start to return from vkQueuePresentKHR, when issued.
    pub tick_to_present_request_ms: Option<f64>,
    pub tick_to_completion_ms: Option<f64>,
    pub frames_in_flight: usize,
    pub resident_upload_bytes: u64,
    /// Bytes actually written to the texture atlas in this submission, included in resident uploads.
    pub atlas_upload_bytes: u64,
    pub atlas_revision: u64,
    /// Payload bytes copied into CPU pages when constructing this atlas version, excluding page COW.
    /// Count once per revision, even when the same version serves multiple submissions.
    pub atlas_version_repack_bytes: usize,
    /// Logical temporary buffer payloads served by an earlier physical allocation in this frame.
    pub transient_reuse_bytes: u64,
    /// Payload bytes written through mapped buffers, excluding resident assets and zero padding.
    pub dynamic_upload_bytes: u64,
    pub scene_table_upload_bytes: u64,
    /// Current and previous skin matrix bytes actually copied to mapped GPU pages.
    pub skin_pose_upload_bytes: u64,
    /// CPU visits to instances during geometry routing and packet preparation.
    pub geometry_instance_visits: usize,
    pub extraction_full_rebuild: bool,
    pub updated_transforms: usize,
    pub updated_palettes: usize,
    pub pose_blocks_read: usize,
    pub pose_locals_read: usize,
    pub palette_joints_examined: usize,
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
    pub recording_chunks: usize,
    pub overlapping_recordings: usize,
    pub dependency_edges: usize,
    pub culled_passes: usize,
    pub pipeline_creations: usize,
    pub pipeline_create_ms: f64,
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
