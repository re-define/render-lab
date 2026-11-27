//! Wait for the single in-flight frame used by the synchronous comparison programs.
use anyhow::{Result, ensure};
use render_lab::{engine::Engine, rhi::Frame};
use std::time::{Duration, Instant};

pub fn wait_frame(engine: &Engine, deadline: Instant, poll_interval: Duration) -> Result<Frame> {
    loop {
        if let Some(frame) = engine.take_frame() {
            return frame.map_err(anyhow::Error::msg);
        }
        ensure!(Instant::now() < deadline, "GPU frame timed out");
        std::thread::sleep(poll_interval);
    }
}
