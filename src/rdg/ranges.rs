//! Subdivide only buffers with explicitly declared partial accesses. Track IDs are compiler-local.
use super::*;
use std::{collections::BTreeMap, ops::Range};

type Slices = Vec<(Range<usize>, Access)>;
#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct Declarations {
    passes: BTreeMap<(usize, Resource), Slices>,
    pub(super) split: BTreeSet<Resource>,
}
impl Declarations {
    pub(super) fn remap_passes(&mut self, live: &[bool], remap: &[usize]) {
        self.passes = std::mem::take(&mut self.passes)
            .into_iter()
            .filter(|((p, _), _)| live[*p])
            .map(|((p, r), v)| ((remap[p], r), v))
            .collect();
    }
}
impl Graph {
    /// Narrow one compute buffer binding to disjoint byte intervals with its actual read/write modes.
    pub fn buffer_ranges(
        &mut self,
        pass: usize,
        buffer: Resource,
        mut slices: Vec<(Range<usize>, Access)>,
    ) -> Result<()> {
        let bytes = match self.resources.get(buffer.0) {
            Some((_, Storage::Buffer { bytes, .. })) => *bytes,
            _ => anyhow::bail!("range declaration requires a buffer"),
        };
        let op = &self.passes.get(pass).context("unknown range pass")?.op;
        ensure!(
            matches!(op, Op::Compute { .. } | Op::ComputeIndirect { .. }),
            "range declaration requires compute"
        );
        let binding = op
            .program()
            .and_then(|p| p.bindings.iter().find(|(r, _)| *r == buffer))
            .context("undeclared range binding")?
            .1;
        ensure!(
            matches!(
                binding,
                Access::ComputeRead | Access::ComputeWrite | Access::ComputeReadWrite
            ),
            "unsupported range binding"
        );
        slices.sort_by_key(|(r, _)| r.start);
        let mut end = 0;
        ensure!(!slices.is_empty(), "empty range declaration");
        for (r, a) in &slices {
            ensure!(
                r.start >= end && r.start < r.end && r.end <= bytes,
                "overlapping or invalid byte range"
            );
            ensure!(
                *a == binding
                    || binding == Access::ComputeReadWrite
                        && matches!(a, Access::ComputeRead | Access::ComputeWrite),
                "incompatible range access"
            );
            end = r.end;
        }
        ensure!(
            !self.subranges.passes.contains_key(&(pass, buffer)),
            "duplicate range declaration"
        );
        self.subranges.passes.insert((pass, buffer), slices);
        self.subranges.split.insert(buffer);
        Ok(())
    }
    pub(crate) fn split_buffer(&self, r: Resource) -> bool {
        self.subranges.split.contains(&r)
    }
}

pub(super) struct Tracks {
    // Each interval begins at this byte and has an independent hazard tracker.
    intervals: BTreeMap<Resource, Vec<(usize, Resource)>>,
    pub physical: Vec<Resource>,
}
impl Tracks {
    pub fn new(g: &Graph) -> Self {
        let mut cuts = BTreeMap::<Resource, BTreeSet<usize>>::new();
        for ((_, r), slices) in &g.subranges.passes {
            let Storage::Buffer { bytes, .. } = g.resources[r.0].1 else {
                unreachable!()
            };
            let bounds = cuts.entry(*r).or_default();
            bounds.insert(0);
            for (s, _) in slices {
                bounds.insert(s.start);
                if s.end < bytes {
                    bounds.insert(s.end);
                }
            }
        }
        let mut physical: Vec<_> = (0..g.resources.len()).map(Resource).collect();
        let intervals = cuts
            .into_iter()
            .map(|(r, cuts)| {
                let parts = cuts
                    .into_iter()
                    .map(|begin| {
                        let track = if begin == 0 {
                            r
                        } else {
                            let t = Resource(physical.len());
                            physical.push(r);
                            t
                        };
                        (begin, track)
                    })
                    .collect();
                (r, parts)
            })
            .collect();
        Self {
            intervals,
            physical,
        }
    }
    pub fn uses(
        &self,
        g: &Graph,
        pass: usize,
        inputs: &[(Resource, Access)],
    ) -> Vec<(Resource, Access)> {
        let mut out = Vec::with_capacity(inputs.len());
        for &(r, access) in inputs {
            if let Some(parts) = self.intervals.get(&r) {
                if let Some(slices) = g.subranges.passes.get(&(pass, r)) {
                    for (range, a) in slices {
                        let first = parts.partition_point(|(start, _)| *start < range.start);
                        let end = parts.partition_point(|(start, _)| *start < range.end);
                        out.extend(parts[first..end].iter().map(|(_, track)| (*track, *a)));
                    }
                } else {
                    out.extend(parts.iter().map(|(_, track)| (*track, access)));
                }
            } else {
                out.push((r, access));
            }
        }
        out
    }
}
