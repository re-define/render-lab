//! A save stores input transitions at fixed ticks. Old particle checkpoints remain valid seeds.
use super::{
    Controls, GUIDES, HZ,
    solver::{self, Strand},
};
use anyhow::{Result, ensure};
use glam::Mat4;
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

fn generation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Serialize, Deserialize)]
struct Change {
    start: u64,
    phase: f64,
    controls: Controls,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Dynamics {
    #[serde(skip, default = "generation")]
    pub generation: u64,
    time: f64,
    phase: f64,
    remainder: f64,
    #[serde(default)]
    pub tick: u64,
    #[serde(default, alias = "strands", skip_serializing_if = "Option::is_none")]
    pub seed: Option<Arc<Vec<Strand>>>,
    #[serde(default)]
    changes: Arc<Vec<Change>>,
}

impl Default for Dynamics {
    fn default() -> Self {
        Self {
            generation: generation(),
            time: 0.,
            phase: 0.,
            remainder: 0.,
            tick: 0,
            seed: None,
            changes: Default::default(),
        }
    }
}

impl Dynamics {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.time.is_finite()
                && self.time >= 0.
                && self.phase.is_finite()
                && (0. ..1. / HZ).contains(&self.remainder)
                && self
                    .seed
                    .as_ref()
                    .is_none_or(|strands| strands.len() == GUIDES
                        && strands.iter().all(|s| s
                            .points
                            .iter()
                            .chain(&s.previous)
                            .all(|p| p.is_finite()))),
            "invalid saved groom checkpoint"
        );
        ensure!(
            self.changes
                .first()
                .map_or(self.tick == 0, |c| c.start == 0)
                && self.changes.windows(2).all(|p| p[0].start < p[1].start),
            "invalid groom input history"
        );
        for c in self.changes.iter() {
            ensure!(
                c.start < self.tick && c.phase.is_finite(),
                "invalid groom input tick"
            );
            c.controls.validate()?;
        }
        Ok(())
    }
    pub fn time_at(&self, tick: u64) -> f64 {
        self.time + tick as f64 / HZ
    }
    fn change_at(&self, tick: u64) -> Option<&Change> {
        self.changes
            .partition_point(|c| c.start < tick)
            .checked_sub(1)
            .map(|i| &self.changes[i])
    }
    pub fn phase_at(&self, tick: u64) -> f64 {
        self.change_at(tick).map_or(self.phase, |c| {
            c.phase
                + if c.controls.motion {
                    (tick - c.start) as f64 * 2.6 / HZ
                } else {
                    0.
                }
        })
    }
    pub fn sample(&self, tick: u64) -> (Mat4, Controls, f32) {
        let c = self.change_at(tick).expect("validated groom step").controls;
        (
            solver::head_frame(self.phase_at(tick) as f32),
            c,
            self.time_at(tick) as f32,
        )
    }
    pub fn advance(&mut self, controls: Controls, dt: f32) -> Result<bool> {
        self.remainder += f64::from(dt);
        let steps = (self.remainder * HZ + 1e-7).floor() as u64;
        self.remainder = (self.remainder - steps as f64 / HZ).max(0.);
        if steps == 0 {
            return Ok(false);
        }
        if self
            .changes
            .last()
            .is_none_or(|last| last.controls != controls)
        {
            let phase = self.phase_at(self.tick);
            Arc::make_mut(&mut self.changes).push(Change {
                start: self.tick,
                phase,
                controls,
            });
        }
        self.tick = self
            .tick
            .checked_add(steps)
            .ok_or_else(|| anyhow::anyhow!("groom clock overflow"))?;
        Ok(true)
    }
}
