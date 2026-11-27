//! Reuse equal-size temporary buffers only across proven queue lifetimes.
use super::{queues::physical_queue, *};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct Reuse {
    pub groups: BTreeMap<Resource, Resource>,
    pub saved_bytes: u64,
}

#[derive(Default)]
struct Interval {
    first: Option<(usize, Access)>,
    // One-based order in the compiled command sequence, zero means unused on that queue.
    last: [usize; 3],
    accesses: Vec<Access>,
}

impl Reuse {
    pub fn plan(plan: &mut FramePlan, queues: usize) -> Self {
        let output = plan.output;
        let plan = &mut plan.commands;
        let mut intervals: Vec<_> = (0..plan.graph.resources.len())
            .map(|_| Interval::default())
            .collect();
        let mut forbidden = BTreeSet::from([output]);
        forbidden.extend(plan.graph.addresses.values().flatten().copied());
        forbidden.extend(plan.graph.histories.keys().copied());
        forbidden.extend(plan.graph.scratch_owners.keys().copied());
        forbidden.extend(plan.graph.cluster_caches.keys().copied());
        let mut queue_clocks = [[0usize; 3]; 3];
        let mut clocks = vec![[0usize; 3]; plan.steps.len()];
        for (rank, step) in plan.steps.iter().enumerate() {
            let q = physical_queue(plan.graph.passes[step.index].queue, queues);
            let mut seen = queue_clocks[q];
            for &parent in &step.parents {
                for (value, parent) in seen.iter_mut().zip(clocks[parent]) {
                    *value = (*value).max(parent);
                }
            }
            seen[q] = rank + 1;
            queue_clocks[q] = seen;
            clocks[step.index] = seen;
            for &(r, access) in &plan.pass_uses[step.index] {
                if !matches!(
                    access,
                    Access::ComputeWrite
                        | Access::ComputeRead
                        | Access::ComputeReadWrite
                        | Access::Mesh
                        | Access::MeshFragment
                        | Access::Indirect
                        | Access::Fragment
                        | Access::Vertex
                        | Access::CopySrc
                        | Access::CopyDst
                        | Access::RayRead
                        | Access::RayWrite
                        | Access::RayReadWrite
                ) {
                    forbidden.insert(r);
                }
                let interval = &mut intervals[r.0];
                interval.first.get_or_insert((rank, access));
                interval.last[q] = rank + 1;
                if !interval.accesses.contains(&access) {
                    interval.accesses.push(access);
                }
            }
        }
        let mut starts = Vec::new();
        for (index, (_, storage)) in plan.graph.resources.iter().enumerate() {
            let r = Resource(index);
            if let Storage::Buffer {
                bytes,
                initial: None,
            } = storage
                && let Some((
                    start,
                    access @ (Access::ComputeWrite | Access::CopyDst | Access::RayWrite),
                )) = intervals[index].first
                && !forbidden.contains(&r)
            {
                starts.push((start, r, *bytes, access));
            }
        }
        starts.sort_by_key(|&(rank, r, ..)| (rank, r));
        let mut chains = BTreeMap::<(usize, bool), Vec<Vec<Resource>>>::new();
        let mut result = Self::default();
        for (start, r, bytes, first_access) in starts {
            let index = plan.steps[start].index;
            let q = physical_queue(plan.graph.passes[index].queue, queues);
            let chains = chains
                .entry((bytes, plan.graph.local_buffers.contains(&r)))
                .or_default();
            let mut seen = clocks[index];
            seen[q] = start; // Two resources touched in one pass always overlap.
            let Some(chain) = chains.iter_mut().find(|chain| {
                intervals[chain.last().unwrap().0]
                    .last
                    .iter()
                    .zip(seen)
                    .all(|(&end, frontier)| end <= frontier)
            }) else {
                chains.push(vec![r]);
                continue;
            };
            let previous = &intervals[chain.last().unwrap().0];
            for (src, &end) in previous.last.iter().enumerate() {
                if end == 0 || src == q {
                    continue;
                }
                let parent = plan.steps[end - 1].index;
                // Reuse only already-ordered work. Widen the semaphore wait to cover the new binding.
                if !plan.steps[start].parents.contains(&parent) {
                    plan.steps[start].parents.push(parent);
                }
                if !plan.graph.passes[index].after.contains(&parent) {
                    plan.graph.passes[index].after.push(parent);
                }
            }
            plan.steps[start].barriers.push(Barrier {
                resource: r,
                from: previous.accesses.clone(),
                to: first_access,
            });
            result.saved_bytes += bytes as u64;
            chain.push(r);
        }
        for chain in chains.into_values().flatten().filter(|c| c.len() > 1) {
            let leader = *chain.iter().min().unwrap();
            result.groups.extend(chain.into_iter().map(|r| (r, leader)));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::FrameGraph;

    fn graph(second_queue: Queue, order: bool, late_read: bool) -> Result<FramePlan> {
        let mut g = FrameGraph::new([1, 1]);
        let storage = Storage::Buffer {
            bytes: 4096,
            initial: None,
        };
        let a = g.resource("A", storage.clone());
        let b = g.resource("B", storage);
        let write = |r| Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(&[]),
                bindings: vec![(r, Access::ComputeWrite)],
                push: vec![],
            },
            groups: [1; 3],
        };
        let first = g.pass("write A", write(a));
        let second = g.pass("write B", write(b));
        // The probe observes both producers even when its display output is constant.
        g.mark_external_effect(first)?;
        g.mark_external_effect(second)?;
        g.passes[second].queue = second_queue;
        if order {
            g.passes[second].after.push(first);
        }
        if late_read {
            let read = g.pass(
                "read A",
                Op::Compute {
                    program: Dispatch {
                        shader: Shader::Compute(&[]),
                        bindings: vec![(a, Access::ComputeRead)],
                        push: vec![],
                    },
                    groups: [1; 3],
                },
            );
            g.mark_external_effect(read)?;
            g.passes[read].after.push(second);
        }
        let output = g.input("output", vec![0; 16]);
        g.set_output(output);
        g.compile()
    }

    #[test]
    fn scratch_has_a_private_allocation_and_requires_a_gpu_write() -> Result<()> {
        let mut plan = graph(Queue::Compute, false, false)?;
        plan.commands.graph.scratch_owners.insert(Resource(0), 7);
        assert!(Reuse::plan(&mut plan, 1).groups.is_empty());
        let mut graph = FrameGraph::new([1, 1]);
        let scratch = graph.scratch_buffer("test rays", 7, 64);
        assert!(!graph.host_readable_buffer(scratch));
        let image = graph.resource(
            "image",
            Storage::Image(ImageDesc {
                extent: [1, 1],
                format: ImageFormat::Rgba32Float,
            }),
        );
        graph.pass(
            "premature read",
            Op::WritePixels {
                image,
                buffer: scratch,
            },
        );
        let output = graph.input("output", vec![0; 16]);
        graph.set_output(output);
        assert!(
            graph
                .compile()
                .err()
                .unwrap()
                .to_string()
                .contains("uninitialized")
        );
        Ok(())
    }

    #[test]
    fn persistent_cluster_version_table_cannot_back_a_temporary() -> Result<()> {
        let mut plan = graph(Queue::Compute, false, false)?;
        plan.commands.graph.cluster_caches.insert(
            Resource(0),
            ClusterCache {
                output: Resource(2),
                parameters: Resource(2),
                sources: vec![],
                force_rebuild: false,
            },
        );
        assert!(Reuse::plan(&mut plan, 1).groups.is_empty());
        Ok(())
    }

    #[test]
    fn device_owned_versions_cannot_be_selected_for_a_host_read() {
        for counter in [false, true] {
            let mut frame = FrameGraph::new([1, 1]);
            let versions = frame.resource(
                "versions",
                Storage::Buffer {
                    bytes: 32,
                    initial: None,
                },
            );
            let out = frame.input("output", vec![0; 16]);
            frame.cluster_caches.insert(
                versions,
                ClusterCache {
                    output: out,
                    parameters: out,
                    sources: vec![],
                    force_rebuild: false,
                },
            );
            if counter {
                frame.pass(
                    "probe",
                    Op::Counters {
                        buffer: versions,
                        first: 0,
                        labels: vec!["version"],
                    },
                );
                frame.set_output(out);
            } else {
                frame.set_output(versions);
            }
            let err = frame.compile().err().expect("owned readback required");
            assert!(format!("{err:#}").contains(if counter {
                "frame-local"
            } else {
                "owned output"
            }));
        }
    }

    #[test]
    fn reuse_requires_all_prior_queue_uses_to_finish() -> Result<()> {
        for (q, count, dependency, late_read, bytes) in [
            (Queue::Graphics, 2, false, false, 0),
            (Queue::Graphics, 1, false, false, 4096),
            (Queue::Graphics, 2, true, false, 4096),
            (Queue::Graphics, 2, true, true, 0),
            (Queue::Compute, 2, false, true, 0),
        ] {
            let mut plan = graph(q, dependency, late_read)?;
            let reuse = Reuse::plan(&mut plan, count);
            assert_eq!(
                reuse.saved_bytes, bytes,
                "queues={count} queue={q:?} ordered={dependency} late_read={late_read}"
            );
            if bytes > 0 {
                assert_eq!(reuse.groups[&Resource(0)], reuse.groups[&Resource(1)]);
                assert!(
                    plan.steps[1].barriers.iter().any(
                        |b| b.resource == Resource(1) && b.from.contains(&Access::ComputeWrite)
                    )
                );
            }
        }
        Ok(())
    }
}
