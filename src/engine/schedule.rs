//! Dependency-ordered simulation with immutable reads and deterministic deferred writes.
mod context;
mod plan;
mod transforms;
use crate::scene::{Id, World};
use anyhow::Result;
use context::Change;
pub use context::SystemContext;
use plan::Plan;
use rayon::{ThreadPool, prelude::*};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, OnceLock},
    time::Duration,
};
pub use transforms::{TransformBindings, TransformTargets};

pub type Edit = Box<dyn FnOnce(&mut World) + Send>;

/// The granularity at which simulation systems declare their dependencies.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SimulationData {
    /// Unknown access or structural changes (including spawning and deleting entities).
    World,
    /// Unbounded local transform access for systems that may visit any entity.
    LocalTransform,
    /// Access restricted to the IDs supplied by SimulationSystem::transform_targets.
    LocalTransformsById,
    Component(&'static str),
    Resource(&'static str),
}
impl SimulationData {
    fn is_transform(self) -> bool {
        matches!(self, Self::LocalTransform | Self::LocalTransformsById)
    }
    fn overlap(self, rhs: Self) -> Option<Self> {
        match (self, rhs) {
            (Self::World, other) | (other, Self::World) => Some(other),
            (a, b) if a.is_transform() && b.is_transform() => Some(Self::LocalTransform),
            (left, right) => (left == right).then_some(left),
        }
    }
}
impl fmt::Display for SimulationData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::World => f.write_str("World"),
            Self::LocalTransform => f.write_str("LocalTransform"),
            Self::LocalTransformsById => f.write_str("entity local transforms"),
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
    fn scoped(self) -> bool {
        self.reads.contains(&SimulationData::LocalTransformsById)
            || self.writes.contains(&SimulationData::LocalTransformsById)
    }
    fn require(&self, data: SimulationData, writing: bool) -> Result<()> {
        let declared = if writing { self.writes } else { self.reads };
        anyhow::ensure!(
            declared.contains(&SimulationData::World) || declared.contains(&data),
            "{}: undeclared {} of {data}",
            self.name,
            if writing { "write" } else { "read" }
        );
        Ok(())
    }
    fn hazards(
        self,
        next: Self,
        left_targets: Option<&TransformTargets>,
        right_targets: Option<&TransformTargets>,
    ) -> BTreeMap<(SimulationData, Option<Id>), &'static str> {
        let mut hazards = BTreeMap::new();
        for (kind, left, right, lw, rw) in [
            ("write/write", self.writes, next.writes, true, true),
            ("write/read", self.writes, next.reads, true, false),
            ("read/write", self.reads, next.writes, false, true),
        ] {
            for &a in left {
                for &b in right {
                    if let Some(data) = a.overlap(b) {
                        let id = if a.is_transform() && b.is_transform() {
                            let left = left_targets
                                .filter(|_| a == SimulationData::LocalTransformsById)
                                .map(|t| t.ids(lw));
                            let right = right_targets
                                .filter(|_| b == SimulationData::LocalTransformsById)
                                .map(|t| t.ids(rw));
                            let Some(witness) = transforms::overlap(left, right) else {
                                continue;
                            };
                            witness
                        } else {
                            None
                        };
                        hazards.entry((data, id)).or_insert(kind);
                    }
                }
            }
        }
        if hazards.is_empty()
            && (self.writes.contains(&SimulationData::World)
                || next.writes.contains(&SimulationData::World))
        {
            hazards.insert((SimulationData::World, None), "exclusive");
        }
        hazards
    }
}

#[derive(Clone, Copy, Default)]
pub struct SystemOrder {
    /// A stable, unique key for systems referenced by other modules.
    pub key: Option<&'static str>,
    pub after: &'static [&'static str],
}

/// Implement this interface for systems that can share a parallel simulation batch.
pub trait SimulationSystem: Send + Sync {
    fn access(&self) -> SystemAccess;
    /// Resolve separate read and write sets from this snapshot. The selector may
    /// read its declared component inputs and should have no side effects.
    fn transform_targets(&self, _: &TransformBindings<'_>) -> Result<TransformTargets> {
        Ok(TransformTargets::default())
    }
    fn order(&self) -> SystemOrder {
        SystemOrder::default()
    }
    fn run(&self, context: &mut SystemContext<'_>, dt: f32) -> Result<()>;
}

/// Compatibility interface. An opaque update can inspect and change the entire world.
pub trait System: Send + Sync {
    fn access(&self) -> SystemAccess {
        SystemAccess {
            name: std::any::type_name::<Self>(),
            reads: &[SimulationData::World],
            writes: &[SimulationData::World],
        }
    }
    fn order(&self) -> SystemOrder {
        SystemOrder::default()
    }
    fn update(&self, world: &World, dt: f32) -> Result<Edit>;
    #[doc(hidden)]
    fn checked(self: Arc<Self>) -> Option<Arc<dyn SimulationSystem>> {
        None
    }
}
impl<T: SimulationSystem + 'static> System for T {
    fn access(&self) -> SystemAccess {
        SimulationSystem::access(self)
    }
    fn order(&self) -> SystemOrder {
        SimulationSystem::order(self)
    }
    fn checked(self: Arc<Self>) -> Option<Arc<dyn SimulationSystem>> {
        Some(self)
    }
    fn update(&self, world: &World, dt: f32) -> Result<Edit> {
        let access = SimulationSystem::access(self);
        let targets = resolve_targets(self, access, world)?;
        let mut context = SystemContext::new(world, access, targets.as_ref());
        self.run(&mut context, dt)?;
        let changes = context.finish();
        Ok(Box::new(move |world| {
            for change in changes {
                change.apply(world, None);
            }
        }))
    }
}

fn resolve_targets(
    system: &dyn SimulationSystem,
    access: SystemAccess,
    world: &World,
) -> Result<Option<TransformTargets>> {
    if !access.scoped() {
        return Ok(None);
    }
    let mut targets = system.transform_targets(&TransformBindings { world, access })?;
    targets.normalize();
    Ok(Some(targets))
}

enum Implementation {
    Checked(Arc<dyn SimulationSystem>),
    Opaque(Arc<dyn System>),
}
struct Entry {
    system: Implementation,
    access: SystemAccess,
    order: SystemOrder,
}
impl Entry {
    fn new(system: Arc<dyn System>) -> Self {
        if let Some(checked) = system.clone().checked() {
            Self {
                access: checked.access(),
                order: checked.order(),
                system: Implementation::Checked(checked),
            }
        } else {
            Self {
                access: SystemAccess {
                    name: system.access().name,
                    reads: &[SimulationData::World],
                    writes: &[SimulationData::World],
                },
                order: system.order(),
                system: Implementation::Opaque(system),
            }
        }
    }
    fn targets(&self, world: &World) -> Result<Option<TransformTargets>> {
        match &self.system {
            Implementation::Checked(s) => resolve_targets(s.as_ref(), self.access, world),
            Implementation::Opaque(_) => Ok(None),
        }
    }
    fn prepare(
        &self,
        world: &World,
        dt: f32,
        targets: Option<&TransformTargets>,
    ) -> Result<Vec<Change>> {
        match &self.system {
            Implementation::Checked(system) => {
                let mut context = SystemContext::new(world, self.access, targets);
                system.run(&mut context, dt)?;
                Ok(context.finish())
            }
            Implementation::Opaque(system) => Ok(vec![Change::Whole(system.update(world, dt)?)]),
        }
    }
}

#[derive(Default)]
pub struct Schedule {
    phases: Vec<Phase>,
    compiled: OnceLock<std::result::Result<Plan, String>>,
}
#[derive(Default)]
struct Phase {
    entries: Vec<Entry>,
    automatic: bool,
    fixed: Option<FixedStep>,
    completed: u64,
}
struct FixedStep {
    seconds: f64,
    remainder: f64,
    max_steps: u32,
}

impl Schedule {
    pub fn add_system(&mut self, system: Arc<dyn System>) {
        self.compiled.take();
        if self.phases.last().is_none_or(|p| !p.automatic) {
            self.phases.push(Phase {
                automatic: true,
                ..Default::default()
            });
        }
        self.phases
            .last_mut()
            .unwrap()
            .entries
            .push(Entry::new(system));
    }
    pub fn stage(&mut self, systems: Vec<Arc<dyn System>>) {
        self.push_phase(systems, None);
    }
    fn push_phase(&mut self, systems: Vec<Arc<dyn System>>, fixed: Option<FixedStep>) {
        self.compiled.take();
        self.phases.push(Phase {
            entries: systems.into_iter().map(Entry::new).collect(),
            fixed,
            ..Default::default()
        });
    }
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
        self.push_phase(
            systems,
            Some(FixedStep {
                seconds: step.as_secs_f64(),
                remainder: 0.,
                max_steps,
            }),
        );
        Ok(())
    }
    fn plan(&self) -> Result<&Plan> {
        self.compiled
            .get_or_init(|| Plan::compile(&self.phases).map_err(|e| e.to_string()))
            .as_ref()
            .map_err(|e| anyhow::anyhow!("{e}"))
    }
    pub fn validate(&self) -> Result<()> {
        self.plan().map(|_| ())
    }
    /// Resolve the schedule report against a snapshot. Execution binds again at
    /// each substep boundary so it observes edits made by preceding systems.
    pub fn bind_transforms(&mut self, world: &World) -> Result<()> {
        self.validate()?;
        let plan = self.compiled.get_mut().unwrap().as_mut().unwrap();
        plan.preview(&self.phases, world)
    }
    pub fn stages(&self) -> impl Iterator<Item = (&[SystemAccess], u64)> {
        self.plan()
            .ok()
            .into_iter()
            .flat_map(|plan| plan.phases.iter().zip(&self.phases))
            .flat_map(|(groups, phase)| {
                groups
                    .iter()
                    .flat_map(|segment| &segment.batches)
                    .map(move |g| (g.access.as_slice(), phase.completed))
            })
    }
    pub fn describe(&self) -> String {
        use fmt::Write;
        let plan = match self.plan() {
            Ok(p) => p,
            Err(e) => return format!("Invalid simulation schedule: {e}"),
        };
        let mut out = format!(
            "Simulation schedule: {} clock stages, {} batches",
            self.phases.len(),
            self.stages().count()
        );
        for (phase, (p, groups)) in self.phases.iter().zip(&plan.phases).enumerate() {
            let clock = p.fixed.as_ref().map_or_else(
                || "variable".into(),
                |f| format!("fixed {:.6} s, max {}", f.seconds, f.max_steps),
            );
            for (level, (segment, g)) in groups
                .iter()
                .flat_map(|s| s.batches.iter().map(move |b| (s, b)))
                .enumerate()
            {
                let _ = write!(
                    out,
                    "\nclock stage {} · {clock} · batch {} · {} committed steps",
                    phase + 1,
                    level + 1,
                    p.completed
                );
                for (&i, a) in g.ids.iter().zip(&g.access) {
                    let order = p.entries[i].order;
                    let _ = write!(
                        out,
                        "\n  {} [{}]: read {:?}, write {:?}",
                        a.name,
                        order.key.unwrap_or("unnamed"),
                        a.reads,
                        a.writes
                    );
                    if let Some(targets) = segment.bindings.get(&i) {
                        let _ = write!(
                            out,
                            "; local entities: {} read, {} write",
                            targets.reads.len(),
                            targets.writes.len()
                        );
                    } else if a.scoped() {
                        out.push_str("; local entities pending binding");
                    }
                    for key in order.after {
                        let _ = write!(out, "\n    after {key}");
                    }
                }
                for reason in &g.hazards {
                    let _ = write!(out, "\n  stage boundary: {reason}");
                }
            }
        }
        out
    }
    pub fn tick(&mut self, world: &mut Arc<World>, dt: f32, pool: &ThreadPool) -> Result<()> {
        anyhow::ensure!(dt.is_finite() && dt >= 0., "invalid simulation time");
        self.validate()?;
        let plan = self.compiled.get_mut().unwrap().as_mut().unwrap();
        for (p, groups) in self.phases.iter_mut().zip(&mut plan.phases) {
            let (steps, interval) = match &mut p.fixed {
                Some(f) => {
                    f.remainder =
                        (f.remainder + f64::from(dt)).min(f.seconds * f64::from(f.max_steps));
                    ((f.remainder / f.seconds).floor() as u32, f.seconds as f32)
                }
                None => (1, dt),
            };
            for _ in 0..steps {
                run_substep(p, groups, world, interval, pool)?;
                if let Some(f) = &mut p.fixed {
                    f.remainder = (f.remainder - f.seconds).max(0.);
                }
                p.completed += 1;
            }
        }
        Ok(())
    }
}

fn run_substep(
    phase: &Phase,
    segments: &mut [plan::Segment],
    world: &mut Arc<World>,
    dt: f32,
    pool: &ThreadPool,
) -> Result<()> {
    let mut undo = Vec::new();
    let segment_count = segments.len();
    let result = (|| {
        for (index, segment) in segments.iter_mut().enumerate() {
            segment.bind(phase, world, None)?;
            for (level, batch) in segment.batches.iter().enumerate() {
                let edits = pool.install(|| {
                    batch
                        .ids
                        .par_iter()
                        .map(|&i| phase.entries[i].prepare(world, dt, segment.bindings.get(&i)))
                        .collect::<Result<Vec<_>>>()
                })?;
                let more = index + 1 < segment_count || level + 1 < segment.batches.len();
                for change in edits.into_iter().flatten() {
                    change.apply(Arc::make_mut(world), more.then_some(&mut undo));
                }
            }
        }
        Ok(())
    })();
    if result.is_err() {
        for revert in undo.into_iter().rev() {
            revert(Arc::make_mut(world));
        }
    }
    result
}
