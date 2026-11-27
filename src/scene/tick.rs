//! Serializable simulation clock checkpoints. All time values are seconds.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// A fixed rate override for a module, or its registration default.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum TickRate {
    #[default]
    #[serde(rename = "default")]
    Default,
    #[serde(rename = "frame")]
    Frame,
    #[serde(rename = "hz")]
    Hertz(u32),
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ClockCheckpoint {
    pub rate: TickRate,
    pub pending_seconds: f64,
    /// Present when a failed admission still owes this frame-driven phase an update.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_frame: Option<f32>,
    pub completed: u64,
    pub dropped_seconds: f64,
}
impl ClockCheckpoint {
    pub(crate) fn validate(self) -> Result<()> {
        ensure!(
            self.pending_frame
                .is_none_or(|dt| dt.is_finite() && dt >= 0.),
            "invalid pending frame interval"
        );
        ensure!(!matches!(self.rate, TickRate::Hertz(0)), "zero clock rate");
        ensure!(
            self.pending_seconds.is_finite() && self.pending_seconds >= 0.,
            "invalid pending simulation time"
        );
        ensure!(
            self.dropped_seconds.is_finite() && self.dropped_seconds >= 0.,
            "invalid discarded simulation time"
        );
        Ok(())
    }
}
