//! Dependency-ordered simulation with immutable reads and deterministic deferred writes.
pub use crate::scene::TickRate;
mod clocks;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemOrder {
    /// A stable, unique key for systems referenced by other modules.
    pub key: Option<&'static str>,
    pub after: &'static [&'static str],
}

/// Implement this interface for systems that can share a parallel simulation batch.
pub trait SimulationSystem: Send + Sync {
    fn access(&self) -> SystemAccess;
    /// When all target IDs come from this system's immutable binding, supply them
    /// once per bound system instance. A rebind installs a replacement instance.
    fn bound_transforms(&self) -> Option<TransformTargets> {
        None
    }
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
        let targets = match self.bound_transforms() {
            Some(mut targets) if access.scoped() => {
                targets.normalize();
                Some(targets)
            }
            _ => resolve_targets(self, access, world)?,
        };
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
    bound: Option<Arc<TransformTargets>>,
}
impl Entry {
    fn new(system: Arc<dyn System>) -> Self {
        if let Some(checked) = system.clone().checked() {
            Self {
                bound: checked
                    .bound_transforms()
                    .filter(|_| checked.access().scoped())
                    .map(|mut t| {
                        t.normalize();
                        Arc::new(t)
                    }),
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
                bound: None,
            }
        }
    }
    fn targets(&self, world: &World) -> Result<Option<Arc<TransformTargets>>> {
        if let Some(bound) = &self.bound {
            return Ok(Some(bound.clone()));
        }
        match &self.system {
            Implementation::Checked(s) => {
                Ok(resolve_targets(s.as_ref(), self.access, world)?.map(Arc::new))
            }
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

/// Counts committed commands and known matrix payloads. Opaque edits have no byte estimate.
#[derive(Default, Debug, Clone, Copy)]
pub struct CommitCounts {
    pub writes: usize,
    pub components: usize,
    pub pose_bytes: usize,
    pub undo_records: usize,
    pub undo_pose_bytes: usize,
}

/// Wall-clock intervals in the most recent tick, summed across its fixed substeps.
#[derive(Default, Debug, Clone, Copy)]
pub struct TickProfile {
    pub counts: CommitCounts,
    pub binding: Duration,
    pub evaluate: Duration,
    /// Exclusive of rollback preparation.
    pub commit: Duration,
    pub capture_undo: Duration,
    pub rollback: Duration,
}
fn timed<T>(counter: Option<&mut Duration>, work: impl FnOnce() -> T) -> T {
    if let Some(counter) = counter {
        let start = std::time::Instant::now();
        let result = work();
        *counter += start.elapsed();
        result
    } else {
        work()
    }
}
#[derive(Default)]
pub struct Schedule {
    interrupted: bool,
    profiling: bool,
    profile: TickProfile,
    phases: Vec<Phase>,
    compiled: OnceLock<std::result::Result<Plan, String>>,
}
#[derive(Default)]
struct Phase {
    frame_due: Option<f32>,
    entries: Vec<Entry>,
    automatic: bool,
    fixed: Option<FixedStep>,
    clock: Option<clocks::Declaration>,
    rate: TickRate,
    dropped_seconds: f64,
    memberships: Vec<Membership>,
    completed: u64,
}
pub type BindingCheck = Box<dyn Fn(&World) -> Result<bool> + Send + Sync>;
type SystemGroup = (Vec<Arc<dyn System>>, BindingCheck);
type BindSystems = dyn Fn(&World) -> Result<SystemGroup> + Send + Sync;
struct Membership {
    entries: std::ops::Range<usize>,
    component: &'static str,
    owners: Vec<Id>,
    hierarchy_source: Arc<()>,
    hierarchy_revision: u64,
    is_current: BindingCheck,
    bind: Box<BindSystems>,
}
impl Phase {
    fn refresh_membership(&mut self, world: &World) -> Result<()> {
        for membership in &mut self.memberships {
            let owners = world.entities.component_owners(membership.component);
            if Arc::ptr_eq(
                &membership.hierarchy_source,
                world.entities.hierarchy_source(),
            ) && owners == membership.owners
                && membership.hierarchy_revision == world.entities.hierarchy_revision()
                && (membership.is_current)(world)?
            {
                continue;
            }
            let (systems, check) = (membership.bind)(world)?;
            let replacement: Vec<_> = systems.into_iter().map(Entry::new).collect();
            let current = &mut self.entries[membership.entries.clone()];
            anyhow::ensure!(
                replacement.len() == current.len()
                    && replacement
                        .iter()
                        .zip(current.iter())
                        .all(|(a, b)| a.access == b.access && a.order == b.order),
                "membership refresh changed the declared simulation schedule"
            );
            for (entry, new) in current.iter_mut().zip(replacement) {
                *entry = new;
            }
            membership.is_current = check;
            membership.hierarchy_source = world.entities.hierarchy_source().clone();
            membership.hierarchy_revision = world.entities.hierarchy_revision();
            membership.owners.clear();
            membership.owners.extend_from_slice(owners);
        }
        Ok(())
    }
}
/// Modules using the same name share one clock, dependency graph, and substep transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedDomain {
    pub name: &'static str,
    pub step: Duration,
    pub max_steps: u32,
}

type SystemFactory<B> = dyn Fn(&B) -> Arc<dyn System> + Send + Sync;

/// Independently registered systems share one immutable membership binding per rebuild.
pub struct BoundSystems<B> {
    factories: Vec<Box<SystemFactory<B>>>,
}
impl<B> Default for BoundSystems<B> {
    fn default() -> Self {
        Self {
            factories: Vec::new(),
        }
    }
}
impl<B: 'static> BoundSystems<B> {
    pub fn register<S: System + 'static>(
        &mut self,
        factory: impl Fn(&B) -> S + Send + Sync + 'static,
    ) {
        self.factories
            .push(Box::new(move |binding| Arc::new(factory(binding))));
    }
    pub fn install(
        self,
        schedule: &mut Schedule,
        domain: FixedDomain,
        world: &World,
        component: &'static str,
        bind: impl Fn(&World) -> Result<(B, BindingCheck)> + Send + Sync + 'static,
    ) -> Result<()> {
        schedule.bind_fixed_systems(domain, world, component, move |world| {
            let (binding, is_current) = bind(world)?;
            Ok((
                self.factories
                    .iter()
                    .map(|factory| factory(&binding))
                    .collect(),
                is_current,
            ))
        })
    }
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
                seconds: clocks::step_seconds(step),
                remainder: 0.,
                max_steps,
            }),
        );
        Ok(())
    }
    /// Plugins can register independently, in any order within a named clock. Its position
    /// relative to other phases is determined by its first registration.
    pub fn add_fixed_system(&mut self, domain: FixedDomain, system: Arc<dyn System>) -> Result<()> {
        self.named_phase(clocks::Declaration::fixed(domain)?)?
            .entries
            .push(Entry::new(system));
        Ok(())
    }

    /// Related systems may share a costly binding rebuild while participating in the same
    /// dependency graph as independently registered systems in this clock.
    fn bind_fixed_systems(
        &mut self,
        domain: FixedDomain,
        world: &World,
        component: &'static str,
        bind: impl Fn(&World) -> Result<SystemGroup> + Send + Sync + 'static,
    ) -> Result<()> {
        let (systems, is_current) = bind(world)?;
        let phase = self.named_phase(clocks::Declaration::fixed(domain)?)?;
        let start = phase.entries.len();
        phase.entries.extend(systems.into_iter().map(Entry::new));
        phase.memberships.push(Membership {
            entries: start..phase.entries.len(),
            component,
            owners: world.entities.component_owners(component).to_vec(),
            hierarchy_source: world.entities.hierarchy_source().clone(),
            hierarchy_revision: world.entities.hierarchy_revision(),
            is_current,
            bind: Box::new(bind),
        });
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
        for phase in &mut self.phases {
            phase.refresh_membership(world)?;
        }
        self.validate()?;
        let plan = self.compiled.get_mut().unwrap().as_mut().unwrap();
        plan.preview(&self.phases, world)
    }
    pub fn stages(&self) -> impl Iterator<Item = (&[SystemAccess], u64)> {
        self.plan()
            .ok()
            .into_iter()
            .flat_map(|plan| &plan.reports(&self.phases).groups)
            .map(|(access, phase)| (access.as_slice(), self.phases[*phase].completed))
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
                || format!("{} variable", p.clock.map_or("independent", |d| d.name)),
                |f| {
                    format!(
                        "{} fixed {:.6} s, max {}",
                        p.clock.map_or("independent", |d| d.name),
                        f.seconds,
                        f.max_steps
                    )
                },
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
        for reason in &plan.reports(&self.phases).clock_hazards {
            let _ = write!(out, "\n  {reason}");
        }
        out
    }
    pub fn set_profiling(&mut self, enabled: bool) {
        self.profiling = enabled;
        self.profile = TickProfile::default();
    }
    pub fn tick_profile(&self) -> Option<TickProfile> {
        self.profiling.then_some(self.profile)
    }
    pub fn tick(&mut self, world: &mut Arc<World>, dt: f32, pool: &ThreadPool) -> Result<()> {
        anyhow::ensure!(dt.is_finite() && dt >= 0., "invalid simulation time");
        self.validate()?;
        self.profile = TickProfile::default();
        if self.interrupted {
            self.finish_tick(world, world.time_seconds(), pool)?;
            if dt == 0. {
                return Ok(());
            }
        }
        let end = world.time_seconds() + f64::from(dt);
        for phase in &mut self.phases {
            if let Some(f) = &mut phase.fixed {
                let elapsed = f.remainder + f64::from(dt);
                let (retained, dropped) = clocks::bounded_debt(elapsed, f.seconds, f.max_steps);
                f.remainder = retained;
                phase.dropped_seconds += dropped;
            } else {
                phase.frame_due = Some(dt);
            }
        }
        self.finish_tick(world, end, pool)
    }

    fn finish_tick(&mut self, world: &mut Arc<World>, end: f64, pool: &ThreadPool) -> Result<()> {
        let result = self.dispatch_due(world, end, pool);
        if world.time_seconds() != end {
            Arc::make_mut(world).time_seconds = end;
        }
        for phase in &self.phases {
            if let Some(clock) = phase.clock {
                let clocks = &mut Arc::make_mut(world).clocks;
                if let Some(slot) = clocks.get_mut(clock.name) {
                    *slot = phase.checkpoint();
                } else {
                    clocks.insert(clock.name.to_owned(), phase.checkpoint());
                }
            }
        }
        self.interrupted = result.is_err();
        result
    }

    fn dispatch_due(&mut self, world: &mut Arc<World>, end: f64, pool: &ThreadPool) -> Result<()> {
        let plan = self.compiled.get_mut().unwrap().as_mut().unwrap();
        plan.reports.take();
        let profile = &mut self.profile;
        let enabled = self.profiling;
        let mut last = 0.;
        while let Some((first, deadline)) = clocks::next_due(&self.phases, end) {
            // Equal deadlines execute in registration/dependency order even if rounded differently.
            last = deadline.max(last);
            if world.time_seconds() != last {
                Arc::make_mut(world).time_seconds = last;
            }
            let candidates = self.phases[first..]
                .iter()
                .take_while(|p| p.frame_due.is_some() && clocks::frame_candidate(p))
                .count();
            let wave = if candidates > 1 {
                timed(enabled.then_some(&mut profile.binding), || {
                    clocks::bind_frame_wave(
                        &self.phases[first..first + candidates],
                        &mut plan.phases[first..first + candidates],
                        world,
                    )
                })?
            } else {
                0
            };
            if wave > 1 {
                let phases = &mut self.phases[first..first + wave];
                let edits = timed(enabled.then_some(&mut profile.evaluate), || {
                    pool.install(|| {
                        phases
                            .par_iter()
                            .zip(&plan.phases[first..first + wave])
                            .map(|(phase, segments)| {
                                phase.entries[0].prepare(
                                    world,
                                    phase.frame_due.expect("pending frame"),
                                    segments[0].bindings.get(&0).map(Arc::as_ref),
                                )
                            })
                            .collect::<Result<Vec<_>>>()
                    })
                })?;
                timed(enabled.then_some(&mut profile.commit), || {
                    for (phase, changes) in phases.iter_mut().zip(edits) {
                        for change in changes {
                            change.apply_timed(
                                Arc::make_mut(world),
                                None,
                                None,
                                enabled.then_some(&mut profile.counts),
                            );
                        }
                        phase.frame_due = None;
                        phase.completed += 1;
                    }
                });
                continue;
            }
            let phase = &mut self.phases[first];
            let interval = phase
                .fixed
                .as_ref()
                .map(|f| f.seconds as f32)
                .or(phase.frame_due)
                .expect("due phase");
            timed(enabled.then_some(&mut profile.binding), || {
                phase.refresh_membership(world)
            })?;
            run_substep(
                phase,
                &mut plan.phases[first],
                world,
                interval,
                pool,
                enabled,
                profile,
            )?;
            if let Some(f) = &mut phase.fixed {
                f.remainder = (f.remainder - f.seconds).max(0.);
            }
            phase.frame_due = None;
            phase.completed += 1;
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
    enabled: bool,
    profile: &mut TickProfile,
) -> Result<()> {
    let mut undo = Vec::new();
    let segment_count = segments.len();
    let result = (|| {
        for (index, segment) in segments.iter_mut().enumerate() {
            timed(enabled.then_some(&mut profile.binding), || {
                segment.bind(phase, world, None)
            })?;
            for (level, batch) in segment.batches.iter().enumerate() {
                let edits = timed(enabled.then_some(&mut profile.evaluate), || {
                    pool.install(|| {
                        batch
                            .ids
                            .par_iter()
                            .map(|&i| {
                                phase.entries[i].prepare(
                                    world,
                                    dt,
                                    segment.bindings.get(&i).map(Arc::as_ref),
                                )
                            })
                            .collect::<Result<Vec<_>>>()
                    })
                })?;
                let more = index + 1 < segment_count || level + 1 < segment.batches.len();
                let mut capture = Duration::ZERO;
                timed(enabled.then_some(&mut profile.commit), || {
                    for change in edits.into_iter().flatten() {
                        change.apply_timed(
                            Arc::make_mut(world),
                            more.then_some(&mut undo),
                            enabled.then_some(&mut capture),
                            enabled.then_some(&mut profile.counts),
                        );
                    }
                });
                profile.capture_undo += capture;
                profile.commit -= capture;
            }
        }
        Ok(())
    })();
    if result.is_err() {
        timed(enabled.then_some(&mut profile.rollback), || {
            for revert in undo.into_iter().rev() {
                revert.restore(Arc::make_mut(world));
            }
        });
    }
    result
}
