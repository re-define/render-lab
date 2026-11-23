//! Simulation stages share an immutable world and commit edits in registration order.
use crate::scene::World;
use anyhow::Result;
use rayon::{ThreadPool, prelude::*};
use std::{collections::BTreeMap, fmt, sync::Arc, time::Duration};

pub type Edit = Box<dyn FnOnce(&mut World) + Send>;

/// The granularity at which simulation systems declare their dependencies.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SimulationData {
    /// Unknown access or structural changes (including spawning and deleting entities).
    World,
    LocalTransform,
    Component(&'static str),
    Resource(&'static str),
}
impl SimulationData {
    fn overlap(self, rhs: Self) -> Option<Self> {
        match (self, rhs) {
            (Self::World, other) | (other, Self::World) => Some(other),
            (left, right) => (left == right).then_some(left),
        }
    }
}
impl fmt::Display for SimulationData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::World => f.write_str("World"),
            Self::LocalTransform => f.write_str("LocalTransform"),
            Self::Component(key) => write!(f, "component {key}"),
            Self::Resource(key) => write!(f, "resource {key}"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SystemAccess {
    pub name: &'static str,
    pub reads: &'static [SimulationData],
    pub writes: &'static [SimulationData],
}
impl SystemAccess {
    fn hazards(self, next: Self) -> BTreeMap<SimulationData, &'static str> {
        let mut hazards = BTreeMap::new();
        for (kind, left, right) in [
            ("write/write", self.writes, next.writes),
            ("write/read", self.writes, next.reads),
            ("read/write", self.reads, next.writes),
        ] {
            for &a in left {
                for &b in right {
                    if let Some(data) = a.overlap(b) {
                        hazards.entry(data).or_insert(kind);
                    }
                }
            }
        }
        hazards
    }
}

pub trait System: Send + Sync {
    /// Unannotated plugins retain their ordered stage semantics and are isolated in automatic groups.
    fn access(&self) -> SystemAccess {
        SystemAccess {
            name: std::any::type_name::<Self>(),
            reads: &[SimulationData::World],
            writes: &[SimulationData::World],
        }
    }
    /// Systems in a stage see the same snapshot. Edits commit in registration order.
    fn update(&self, world: &World, dt: f32) -> Result<Edit>;
}
#[derive(Default)]
pub struct Schedule {
    stages: Vec<Stage>,
}
struct Stage {
    systems: Vec<Arc<dyn System>>,
    access: Vec<SystemAccess>,
    automatic: bool,
    completed_steps: u64,
    fixed: Option<FixedStep>,
}
impl Stage {
    fn explicit(systems: Vec<Arc<dyn System>>, fixed: Option<FixedStep>) -> Self {
        Self {
            access: systems.iter().map(|system| system.access()).collect(),
            systems,
            fixed,
            automatic: false,
            completed_steps: 0,
        }
    }
}
struct FixedStep {
    seconds: f64,
    remainder: f64,
    max_steps: u32,
}
impl Schedule {
    /// Access declarations in commit order and the number of completed updates for each stage.
    pub fn stages(&self) -> impl Iterator<Item = (&[SystemAccess], u64)> {
        self.stages
            .iter()
            .map(|s| (s.access.as_slice(), s.completed_steps))
    }
    /// Group contiguous independent systems. An explicit stage or fixed clock always ends a group.
    pub fn add_system(&mut self, system: Arc<dyn System>) {
        let access = system.access();
        if let Some(last) = self.stages.last_mut()
            && last.automatic
            && last.access.iter().all(|a| a.hazards(access).is_empty())
        {
            last.access.push(access);
            last.systems.push(system);
        } else {
            self.stages.push(Stage {
                systems: vec![system],
                access: vec![access],
                automatic: true,
                completed_steps: 0,
                fixed: None,
            });
        }
    }
    pub fn stage(&mut self, systems: Vec<Arc<dyn System>>) {
        self.stages.push(Stage::explicit(systems, None));
    }
    /// Run a fixed interval stage at most max_steps times per tick.
    pub fn fixed_stage(
        &mut self,
        step: Duration,
        max_steps: u32,
        systems: Vec<Arc<dyn System>>,
    ) -> Result<()> {
        anyhow::ensure!(
            !step.is_zero() && max_steps > 0,
            "invalid fixed simulation step"
        );
        self.stages.push(Stage::explicit(
            systems,
            Some(FixedStep {
                seconds: step.as_secs_f64(),
                remainder: 0.,
                max_steps,
            }),
        ));
        Ok(())
    }
    /// Registration order within each snapshot stage, successful substeps and declared hazards.
    pub fn describe(&self) -> String {
        use fmt::Write;
        let mut output = format!("Simulation stages: {}", self.stages.len());
        for (index, stage) in self.stages.iter().enumerate() {
            let clock = match &stage.fixed {
                Some(fixed) => format!("fixed {:.6} s, max {}", fixed.seconds, fixed.max_steps),
                None if stage.automatic => "variable, automatic".into(),
                None => "variable, explicit".into(),
            };
            let _ = write!(
                output,
                "\n{} · {clock} · {} committed steps",
                index + 1,
                stage.completed_steps
            );
            if stage.automatic && index > 0 && self.stages[index - 1].automatic {
                let first = stage.access[0];
                for earlier in &self.stages[index - 1].access {
                    for (data, hazard) in earlier.hazards(first) {
                        let _ = write!(
                            output,
                            "\n  stage boundary: {hazard} {data}: {} → {}",
                            earlier.name, first.name
                        );
                    }
                }
            }
            for (i, system) in stage.access.iter().enumerate() {
                let _ = write!(
                    output,
                    "\n  {}: read {:?}, write {:?}",
                    system.name, system.reads, system.writes
                );
                for earlier in &stage.access[..i] {
                    for (data, hazard) in earlier.hazards(*system) {
                        let _ = write!(
                            output,
                            "\n    {hazard} {data}: {} → {}",
                            earlier.name, system.name
                        );
                    }
                }
            }
        }
        output
    }
    pub fn tick(&mut self, world: &mut Arc<World>, dt: f32, pool: &ThreadPool) -> Result<()> {
        anyhow::ensure!(dt.is_finite() && dt >= 0., "invalid simulation time");
        for stage in &mut self.stages {
            let (steps, interval) = match &mut stage.fixed {
                Some(fixed) => {
                    fixed.remainder = (fixed.remainder + f64::from(dt))
                        .min(fixed.seconds * f64::from(fixed.max_steps));
                    let count = (fixed.remainder / fixed.seconds).floor() as u32;
                    (count, fixed.seconds as f32)
                }
                None => (1, dt),
            };
            for _ in 0..steps {
                let edits = pool.install(|| {
                    stage
                        .systems
                        .par_iter()
                        .map(|s| s.update(world, interval))
                        .collect::<Result<Vec<_>>>()
                })?;
                for edit in edits {
                    edit(Arc::make_mut(world));
                }
                if let Some(fixed) = &mut stage.fixed {
                    fixed.remainder = (fixed.remainder - fixed.seconds).max(0.);
                }
                stage.completed_steps += 1;
            }
        }
        Ok(())
    }
}
