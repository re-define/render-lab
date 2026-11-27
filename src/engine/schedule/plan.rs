use super::{Phase, SimulationData, SystemAccess, TransformTargets};
use crate::scene::World;
use anyhow::{Result, bail, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Default)]
pub(super) struct Batch {
    pub ids: Vec<usize>,
    pub access: Vec<SystemAccess>,
    pub hazards: Vec<String>,
}

pub(super) struct Plan {
    pub phases: Vec<Vec<Segment>>,
    pub(super) reports: std::sync::OnceLock<Reports>,
}

pub(super) struct Reports {
    pub groups: Vec<(Vec<SystemAccess>, usize)>,
    pub clock_hazards: Vec<String>,
}

impl Plan {
    pub fn compile(phases: &[Phase]) -> Result<Self> {
        let mut keys = BTreeMap::new();
        for (p, phase) in phases.iter().enumerate() {
            for (i, entry) in phase.entries.iter().enumerate() {
                if let Some(key) = entry.order.key {
                    ensure!(
                        !key.is_empty() && keys.insert(key, (p, i)).is_none(),
                        "empty or duplicate simulation system key '{key}'"
                    );
                }
            }
        }
        let mut batches = Vec::with_capacity(phases.len());
        for (p, phase) in phases.iter().enumerate() {
            let mut parents = vec![BTreeSet::new(); phase.entries.len()];
            for (i, entry) in phase.entries.iter().enumerate() {
                for &key in entry.order.after {
                    let Some(&(source, producer)) = keys.get(key) else {
                        bail!(
                            "{} depends on unknown simulation system '{key}'",
                            entry.access.name
                        )
                    };
                    ensure!(
                        source <= p,
                        "{} depends on '{key}' in a later clock/stage",
                        entry.access.name
                    );
                    if source == p {
                        parents[i].insert(producer);
                    }
                }
            }
            // Topologically order explicit edges before orienting conflicts. This
            // permits consumers to register before producers, even for shared writes.
            let mut ordered = Vec::with_capacity(parents.len());
            let mut done = vec![false; parents.len()];
            while ordered.len() < parents.len() {
                let Some(next) =
                    (0..parents.len()).find(|&i| !done[i] && parents[i].iter().all(|&j| done[j]))
                else {
                    let blocked = phase
                        .entries
                        .iter()
                        .zip(done)
                        .filter_map(|(s, ready)| {
                            (!ready).then_some(s.order.key.unwrap_or(s.access.name))
                        })
                        .collect::<Vec<_>>();
                    bail!(
                        "cycle in simulation dependencies; blocked systems: {}",
                        blocked.join(", ")
                    );
                };
                done[next] = true;
                ordered.push(next);
            }
            let mut segments = Vec::new();
            let mut nodes = Vec::new();
            let mut writes = BTreeSet::new();
            let mut boundary = None;
            for id in ordered {
                let access = phase.entries[id].access;
                if access.scoped()
                    && let Some(input) = binding_input(&writes, access)
                {
                    segments.push(Segment::new(
                        phase,
                        std::mem::take(&mut nodes),
                        &parents,
                        boundary,
                    ));
                    boundary = Some(format!(
                        "resolve {} targets after write to {input}",
                        access.name
                    ));
                    writes.clear();
                }
                writes.extend(access.writes.iter().copied());
                nodes.push(id);
            }
            segments.push(Segment::new(phase, nodes, &parents, boundary));
            batches.push(segments);
        }
        Ok(Self {
            phases: batches,
            reports: Default::default(),
        })
    }

    pub(super) fn reports(&self, phases: &[Phase]) -> &Reports {
        self.reports.get_or_init(|| self.build_reports(phases))
    }

    fn build_reports(&self, phases: &[Phase]) -> Reports {
        let mut groups = Vec::new();
        let mut clock_hazards = Vec::new();
        let mut first = 0;
        while first < phases.len() {
            let n = super::clocks::parallel_frame_wave(&phases[first..], &self.phases[first..]);
            if n > 1 {
                groups.push((
                    phases[first..first + n]
                        .iter()
                        .map(|p| p.entries[0].access)
                        .collect(),
                    first,
                ));
            } else {
                for batch in self.phases[first].iter().flat_map(|s| &s.batches) {
                    groups.push((batch.access.clone(), first));
                }
            }
            let next = first + n.max(1);
            if n > 0 && next < phases.len() && super::clocks::frame_candidate(&phases[next]) {
                let right = &phases[next].entries[0];
                let target = self.phases[next][0].bindings.get(&0).map(Arc::as_ref);
                for (i, left) in phases.iter().enumerate().take(next).skip(first) {
                    let left = &left.entries[0];
                    for ((data, entity), hazard) in left.access.hazards(
                        right.access,
                        self.phases[i][0].bindings.get(&0).map(Arc::as_ref),
                        target,
                    ) {
                        let entity = entity.map(|id| format!(" entity {id}")).unwrap_or_default();
                        clock_hazards.push(format!(
                            "stage boundary: {hazard} {data}{entity}: {} → {}",
                            left.access.name, right.access.name
                        ));
                    }
                }
            }
            first = next;
        }
        Reports {
            groups,
            clock_hazards,
        }
    }

    pub fn preview(&mut self, phases: &[Phase], world: &World) -> Result<()> {
        let mut writes = BTreeSet::new();
        for (phase, segments) in phases.iter().zip(&mut self.phases) {
            for segment in segments {
                segment.bind(phase, world, Some(&writes))?;
                for (id, _) in &segment.nodes {
                    writes.extend(phase.entries[*id].access.writes.iter().copied());
                }
            }
        }
        self.reports.take();
        Ok(())
    }
}

// A selector may inspect declared component inputs. Resolve it after any earlier
// change to those inputs or world structure has committed within this substep.
pub(super) fn binding_input(
    writes: &BTreeSet<SimulationData>,
    access: SystemAccess,
) -> Option<SimulationData> {
    use SimulationData::{Component, World};
    if writes.contains(&World) {
        return Some(World);
    }
    writes.iter().copied().find(|data| {
        matches!(data, Component(_))
            && (access.reads.contains(data) || access.reads.contains(&World))
    })
}

pub(super) struct Segment {
    nodes: Vec<(usize, Vec<usize>)>,
    scoped: Vec<usize>,
    boundary: Option<String>,
    pub bindings: BTreeMap<usize, Arc<TransformTargets>>,
    pub batches: Vec<Batch>,
}

impl Segment {
    fn new(
        phase: &Phase,
        ids: Vec<usize>,
        parents: &[BTreeSet<usize>],
        boundary: Option<String>,
    ) -> Self {
        let active: BTreeSet<_> = ids.iter().copied().collect();
        let scoped = ids
            .iter()
            .copied()
            .filter(|&i| phase.entries[i].access.scoped())
            .collect();
        let nodes = ids
            .into_iter()
            .map(|i| (i, parents[i].intersection(&active).copied().collect()))
            .collect();
        let mut segment = Self {
            nodes,
            scoped,
            boundary,
            bindings: BTreeMap::new(),
            batches: Vec::new(),
        };
        segment.group(phase);
        segment
    }

    pub fn bind(
        &mut self,
        phase: &Phase,
        world: &World,
        pending_writes: Option<&BTreeSet<SimulationData>>,
    ) -> Result<()> {
        if self.scoped.is_empty() {
            return Ok(());
        }
        let mut bindings = BTreeMap::new();
        for &id in &self.scoped {
            let entry = &phase.entries[id];
            // During preview a producer can still be responsible for creating the binding.
            if pending_writes.is_some_and(|writes| binding_input(writes, entry.access).is_some()) {
                continue;
            }
            if let Some(targets) = entry.targets(world)? {
                bindings.insert(id, targets);
            }
        }
        if bindings != self.bindings {
            self.bindings = bindings;
            self.group(phase);
        }
        Ok(())
    }

    fn group(&mut self, phase: &Phase) {
        let mut levels = vec![0; phase.entries.len()];
        let mut groups = vec![Batch::default()];
        for (rank, (id, parents)) in self.nodes.iter().enumerate() {
            let access = phase.entries[*id].access;
            let mut level = parents.iter().map(|&j| levels[j] + 1).max().unwrap_or(0);
            let mut reasons = Vec::new();
            for (j, _) in &self.nodes[..rank] {
                let prior = phase.entries[*j].access;
                for ((data, entity), hazard) in prior.hazards(
                    access,
                    self.bindings.get(j).map(Arc::as_ref),
                    self.bindings.get(id).map(Arc::as_ref),
                ) {
                    level = level.max(levels[*j] + 1);
                    let entity = entity.map(|id| format!(" entity {id}")).unwrap_or_default();
                    reasons.push(format!(
                        "{hazard} {data}{entity}: {} → {}",
                        prior.name, access.name
                    ));
                }
            }
            levels[*id] = level;
            groups.resize_with(groups.len().max(level + 1), Batch::default);
            groups[level].ids.push(*id);
            groups[level].hazards.extend(reasons);
        }
        if let Some(boundary) = &self.boundary {
            groups[0].hazards.push(boundary.clone());
        }
        for g in &mut groups {
            g.ids.sort_unstable();
            g.access = g.ids.iter().map(|&i| phase.entries[i].access).collect();
        }
        self.batches = groups;
    }
}
