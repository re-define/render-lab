//! Per-module tick rates and deadlines on the shared world simulation timeline.
use super::*;
use crate::scene::ClockCheckpoint;
use anyhow::{Context, ensure};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Declaration {
    pub(super) name: &'static str,
    default_step: Option<Duration>,
    max_steps: u32,
}
impl Declaration {
    pub(super) fn fixed(domain: FixedDomain) -> Result<Self> {
        ensure!(
            !domain.step.is_zero() && domain.max_steps > 0,
            "invalid fixed domain"
        );
        Ok(Self {
            name: domain.name,
            default_step: Some(domain.step),
            max_steps: domain.max_steps,
        })
    }

    fn step(self, rate: TickRate) -> Result<Option<f64>> {
        match rate {
            TickRate::Default => Ok(self.default_step.map(step_seconds)),
            TickRate::Frame => {
                ensure!(
                    self.default_step.is_none(),
                    "{} requires fixed substeps",
                    self.name
                );
                Ok(None)
            }
            TickRate::Hertz(hz) => {
                ensure!(hz > 0, "zero clock rate");
                Ok(Some(1. / f64::from(hz)))
            }
        }
    }
}

/// Duration stores nanoseconds. Preserve coincident deadlines for common rates such as 30/60 Hz.
pub(super) fn step_seconds(step: Duration) -> f64 {
    let seconds = step.as_secs_f64();
    let hz = seconds.recip().round();
    if hz >= 1. && Duration::from_secs_f64(hz.recip()) == step {
        hz.recip()
    } else {
        seconds
    }
}

/// Skip whole overdue ticks and retain the fractional phase on the world timeline.
pub(super) fn bounded_debt(elapsed: f64, step: f64, limit: u32) -> (f64, f64) {
    let steps = elapsed / step;
    if steps.floor() <= f64::from(limit) {
        return (elapsed, 0.);
    }
    let retained = (f64::from(limit) + steps.fract()) * step;
    (retained, elapsed - retained)
}

impl Phase {
    pub(super) fn checkpoint(&self) -> ClockCheckpoint {
        ClockCheckpoint {
            rate: self.rate,
            pending_frame: self.frame_due,
            pending_seconds: self.fixed.as_ref().map_or(0., |f| f.remainder),
            completed: self.completed,
            dropped_seconds: self.dropped_seconds,
        }
    }

    fn set_clock(
        &mut self,
        rate: TickRate,
        mut checkpoint: ClockCheckpoint,
        restoring: bool,
    ) -> Result<()> {
        let clock = self.clock.context("expected a named clock")?;
        checkpoint.validate()?;
        let step = clock.step(rate)?;
        let mut pending = checkpoint.pending_seconds;
        if step.is_some() {
            ensure!(
                !restoring || checkpoint.pending_frame.is_none(),
                "fixed domain has pending frame update"
            );
            pending += f64::from(checkpoint.pending_frame.take().unwrap_or(0.));
        }
        let fixed = if let Some(seconds) = step {
            let (remainder, dropped) = bounded_debt(pending, seconds, clock.max_steps);
            if restoring {
                ensure!(
                    dropped == 0.,
                    "pending simulation time exceeds {} catch-up limit",
                    clock.name
                );
            }
            checkpoint.dropped_seconds += dropped;
            Some(FixedStep {
                seconds,
                remainder,
                max_steps: clock.max_steps,
            })
        } else {
            if restoring {
                ensure!(pending == 0., "frame clock cannot have pending fixed steps");
            }
            checkpoint.dropped_seconds += pending;
            None
        };
        self.frame_due = checkpoint.pending_frame;
        self.fixed = fixed;
        self.rate = rate;
        self.completed = checkpoint.completed;
        self.dropped_seconds = checkpoint.dropped_seconds;
        Ok(())
    }
}

impl Schedule {
    pub(super) fn named_phase(&mut self, clock: Declaration) -> Result<&mut Phase> {
        ensure!(!clock.name.is_empty(), "empty simulation clock name");
        self.compiled.take();
        if let Some(i) = self
            .phases
            .iter()
            .position(|p| p.clock.is_some_and(|c| c.name == clock.name))
        {
            ensure!(
                self.phases[i].clock == Some(clock),
                "conflicting timestep or catch-up limit in domain {}",
                clock.name
            );
            return Ok(&mut self.phases[i]);
        }
        let mut phase = Phase {
            clock: Some(clock),
            ..Phase::default()
        };
        phase.set_clock(TickRate::Default, ClockCheckpoint::default(), true)?;
        self.phases.push(phase);
        Ok(self.phases.last_mut().unwrap())
    }

    /// Register a module with its own cadence on the world timeline. Its default is one update per simulation tick.
    pub fn add_frame_system(&mut self, name: &'static str, system: Arc<dyn System>) -> Result<()> {
        self.named_phase(Declaration {
            name,
            default_step: None,
            max_steps: 8,
        })?
        .entries
        .push(Entry::new(system));
        Ok(())
    }

    /// Change every system in a named phase together. Keep pending wall time up to its catch-up limit.
    pub fn set_tick_rate(&mut self, name: &str, rate: TickRate) -> Result<()> {
        let p = self
            .phases
            .iter_mut()
            .find(|p| p.clock.is_some_and(|c| c.name == name))
            .with_context(|| format!("unknown simulation clock '{name}'"))?;
        p.set_clock(rate, p.checkpoint(), false)?;
        if let Some(Ok(plan)) = self.compiled.get_mut() {
            plan.reports.take();
        }
        Ok(())
    }

    /// None for inactive and frame-driven domains; returns the solver time step in seconds.
    pub fn fixed_seconds(&self, name: &str) -> Option<f64> {
        self.phases
            .iter()
            .find(|p| p.clock.is_some_and(|c| c.name == name))?
            .fixed
            .as_ref()
            .map(|f| f.seconds)
    }

    pub fn describe_clocks(&self) -> String {
        let mut out = String::from("Simulation clocks");
        for p in &self.phases {
            use std::fmt::Write;
            let Some(clock) = p.clock else { continue };
            let rate = p
                .fixed
                .as_ref()
                .map_or_else(|| "frame".into(), |f| format!("{:.2} Hz", 1. / f.seconds));
            let entries = p
                .entries
                .iter()
                .map(|e| e.access.name)
                .collect::<Vec<_>>()
                .join(", ");
            let cp = p.checkpoint();
            let _ = write!(
                out,
                "\n{} · {rate} · max {} · {} steps · {:.3} ms pending · {:.3} ms dropped · {entries}",
                clock.name,
                clock.max_steps,
                cp.completed,
                cp.pending_seconds * 1000.,
                cp.dropped_seconds * 1000.
            );
        }
        out.push_str("\nInput/UI · events; rendering · admitted snapshots; RHI · submitted plans; presentation · swapchain");
        out
    }

    pub(crate) fn checkpoints(&self, world: &World) -> BTreeMap<String, ClockCheckpoint> {
        let mut clocks = world.clocks.clone();
        for phase in &self.phases {
            if let Some(clock) = phase.clock {
                clocks.insert(clock.name.to_owned(), phase.checkpoint());
            }
        }
        clocks
    }

    pub(crate) fn restore_clocks(&mut self, world: &World) -> Result<()> {
        for p in &mut self.phases {
            if let Some(c) = p.clock
                && let Some(&cp) = world.clocks.get(c.name)
            {
                p.set_clock(cp.rate, cp, true)?;
            }
        }
        self.interrupted = self.phases.iter().any(|p| {
            p.frame_due.is_some() || p.fixed.as_ref().is_some_and(|f| f.remainder >= f.seconds)
        });
        Ok(())
    }
}

/// Earliest due tick wins. Ties retain the stage order validated by Plan::compile.
pub(super) fn next_due(phases: &[Phase], end: f64) -> Option<(usize, f64)> {
    let mut next: Option<(usize, f64)> = None;
    for (i, phase) in phases.iter().enumerate() {
        let overdue = if let Some(f) = &phase.fixed {
            let debt = f.remainder - f.seconds;
            if debt < -4. * f64::EPSILON * f.remainder {
                continue;
            }
            debt.max(0.)
        } else if phase.frame_due.is_some() {
            0.
        } else {
            continue;
        };
        // Compare offsets before adding the world epoch, which may be much larger than a tick.
        let tolerance = 4. * f64::EPSILON * overdue.max(1.);
        if next.is_none_or(|(_, prior)| overdue > prior + tolerance) {
            next = Some((i, overdue));
        }
    }
    next.map(|(i, overdue)| (i, (end - overdue).max(0.)))
}

/// Only adjacent single-system frame modules may coalesce across clock boundaries.
pub(super) fn frame_candidate(p: &Phase) -> bool {
    (p.clock.is_some() || p.automatic)
        && p.fixed.is_none()
        && p.memberships.is_empty()
        && p.entries.len() == 1
}

fn same_wave(phases: &[Phase], plans: &[Vec<plan::Segment>], next: usize) -> bool {
    let entry = &phases[next].entries[0];
    let target = plans[next][0].bindings.get(&0).map(Arc::as_ref);
    phases[..next].iter().enumerate().all(|(i, phase)| {
        let prior = &phase.entries[0];
        !prior
            .order
            .key
            .is_some_and(|k| entry.order.after.contains(&k))
            && prior
                .access
                .hazards(
                    entry.access,
                    plans[i][0].bindings.get(&0).map(Arc::as_ref),
                    target,
                )
                .is_empty()
    })
}

pub(super) fn parallel_frame_wave(phases: &[Phase], plans: &[Vec<plan::Segment>]) -> usize {
    (0..phases.len())
        .take_while(|&i| frame_candidate(&phases[i]) && same_wave(phases, plans, i))
        .count()
}

/// Bind candidates one by one, stopping before any earlier module can invalidate the next selector.
pub(super) fn bind_frame_wave(
    phases: &[Phase],
    plans: &mut [Vec<plan::Segment>],
    world: &World,
) -> Result<usize> {
    let mut written = std::collections::BTreeSet::new();
    for (i, phase) in phases.iter().enumerate() {
        if !frame_candidate(phase) {
            return Ok(i);
        }
        let entry = &phase.entries[0];
        if i > 0
            && entry.access.scoped()
            && entry.bound.is_none()
            && plan::binding_input(&written, entry.access).is_some()
        {
            return Ok(i);
        }
        plans[i][0].bind(phase, world, None)?;
        if !same_wave(phases, plans, i) {
            return Ok(i);
        }
        written.extend(entry.access.writes);
    }
    Ok(phases.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Step;
    impl System for Step {
        fn update(&self, _: &World, _: f32) -> Result<Edit> {
            Ok(Box::new(|_| {}))
        }
    }
    #[test]
    fn deadline_order_is_independent_of_the_world_epoch() {
        let phase = |debt| Phase {
            fixed: Some(FixedStep {
                seconds: 0.0005,
                remainder: debt,
                max_steps: 8,
            }),
            ..Default::default()
        };
        let phases = [phase(0.0005), phase(0.001)];
        for end in [0.1, 1e12] {
            assert_eq!(next_due(&phases, end).unwrap().0, 1);
        }
    }

    #[test]
    fn capped_ticks_keep_fraction_and_frame_retry_survives_save() -> Result<()> {
        use std::sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        };
        struct Observe {
            events: Arc<Mutex<Vec<(f64, f32)>>>,
            fail: Arc<AtomicBool>,
        }
        impl System for Observe {
            fn update(&self, w: &World, dt: f32) -> Result<Edit> {
                ensure!(
                    !self.fail.swap(false, Ordering::Relaxed),
                    "injected failure"
                );
                self.events.lock().unwrap().push((w.time_seconds(), dt));
                Ok(Box::new(|_| {}))
            }
        }
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        let fixed = Arc::new(Mutex::new(Vec::new()));
        let frames = Arc::new(Mutex::new(Vec::new()));
        let fail = Arc::new(AtomicBool::new(true));
        let make = || -> Result<Schedule> {
            let mut s = Schedule::default();
            s.add_fixed_system(
                FixedDomain {
                    name: "fixed",
                    step: Duration::from_millis(125),
                    max_steps: 2,
                },
                Arc::new(Observe {
                    events: fixed.clone(),
                    fail: Arc::new(false.into()),
                }),
            )?;
            s.add_frame_system(
                "presentation input",
                Arc::new(Observe {
                    events: frames.clone(),
                    fail: fail.clone(),
                }),
            )?;
            Ok(s)
        };
        let mut a = make()?;
        let mut world = Arc::new(World::default());
        assert!(a.tick(&mut world, 0.5625, &pool).is_err());
        assert_eq!(world.time_seconds(), 0.5625);
        assert_eq!(*fixed.lock().unwrap(), [(0.375, 0.125), (0.5, 0.125)]);
        let cp = a.checkpoints(&world)["fixed"];
        assert_eq!((cp.pending_seconds, cp.dropped_seconds), (0.0625, 0.25));
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("clock.json");
        world.save(&path)?;
        let mut world = Arc::new(World::load(&path)?);
        let mut restored = make()?;
        restored.restore_clocks(&world)?;
        restored.tick(&mut world, 0.0625, &pool)?;
        assert_eq!(*frames.lock().unwrap(), [(0.5625, 0.5625), (0.625, 0.0625)]);
        assert_eq!(fixed.lock().unwrap().last(), Some(&(0.625, 0.125)));
        Ok(())
    }

    #[test]
    fn fixed_clock_saves_between_steps_and_reports_catch_up_overflow() -> Result<()> {
        let domain = FixedDomain {
            name: "solver",
            step: Duration::from_millis(25),
            max_steps: 4,
        };
        let make = || -> Result<_> {
            let mut s = Schedule::default();
            s.add_fixed_system(domain, Arc::new(Step))?;
            Ok(s)
        };
        let mut a = make()?;
        let mut w = Arc::new(World::default());
        a.set_tick_rate("solver", TickRate::Hertz(32))?;
        assert!(a.set_tick_rate("solver", TickRate::Frame).is_err());
        let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
        a.tick(&mut w, 1. / 64., &pool)?;
        let path = tempfile::tempdir()?;
        let file = path.path().join("clock.json");
        w.save_with_clocks(&file, &a.checkpoints(&w))?;
        let mut loaded = Arc::new(World::load(&file)?);
        let mut b = make()?;
        assert_eq!(loaded.time_seconds(), 1. / 64.);
        b.restore_clocks(&loaded)?;
        b.tick(&mut loaded, 1. / 64., &pool)?;
        assert_eq!(b.stages().next().unwrap().1, 1);
        assert_eq!(b.fixed_seconds("solver"), Some(1. / 32.));
        b.tick(&mut loaded, 0.5, &pool)?;
        let state = b.checkpoints(&loaded)["solver"];
        assert_eq!(loaded.time_seconds(), 0.53125);
        assert_eq!(state.completed, 5);
        assert_eq!(state.pending_seconds, 0.);
        assert_eq!(state.dropped_seconds, 0.375);
        let mut legacy: World = serde_json::from_str(r#"{"version":1,"entities":{}}"#)?;
        let mut b = make()?;
        b.restore_clocks(&legacy)?;
        assert_eq!(b.fixed_seconds("solver"), Some(0.025));
        legacy.clocks.insert(
            "parked plugin".into(),
            ClockCheckpoint {
                rate: TickRate::Hertz(32),
                pending_seconds: 1. / 64.,
                completed: 3,
                pending_frame: None,
                dropped_seconds: 0.,
            },
        );
        b.restore_clocks(&legacy)?;
        assert!(
            b.set_tick_rate("parked plugin", TickRate::Hertz(60))
                .is_err()
        );
        assert_eq!(b.checkpoints(&legacy)["parked plugin"].completed, 3);
        b.add_frame_system("parked plugin", Arc::new(Step))?;
        b.restore_clocks(&legacy)?;
        assert_eq!(b.fixed_seconds("parked plugin"), Some(1. / 32.));
        legacy.clocks.insert(
            "solver".into(),
            ClockCheckpoint {
                pending_seconds: -2.,
                ..Default::default()
            },
        );
        assert!(legacy.validate().is_err());
        Ok(())
    }
}
