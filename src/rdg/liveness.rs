//! Keep pass IDs dense for the execution plan; resource handles stay stable for frame products.
use super::*;

impl Graph {
    /// Keep an operation that changes externally observable state even if no frame output reads it.
    pub fn mark_external_effect(&mut self, pass: usize) -> Result<()> {
        ensure!(pass < self.passes.len(), "unknown external-effect pass");
        self.external_effects.insert(pass);
        Ok(())
    }

    pub(crate) fn compile_frame(self) -> Result<Plan> {
        // Keep the original whole-resource plan available for differential replay.
        if std::env::var_os("RENDER_LAB_REFERENCE_RDG").is_some_and(|v| v == "1") {
            let mut graph = self;
            graph.subranges = Default::default();
            return graph.compile();
        }
        let mut plan = self.compile()?;
        let started = std::time::Instant::now();
        let graph = &mut plan.graph;
        let mut live = vec![false; graph.passes.len()];
        for step in plan.steps.iter().rev() {
            let i = step.index;
            live[i] |= graph.external_effects.contains(&i)
                || matches!(
                    graph.passes[i].op,
                    Op::HostRead(_) | Op::Counters { .. } | Op::Present(_) | Op::Dlss(_)
                )
                || plan.pass_uses[i].iter().any(|(r, access)| {
                    access.writes()
                        && (graph.histories.contains_key(r) || graph.cluster_caches.contains_key(r))
                });
            if live[i] {
                for &parent in &step.parents {
                    live[parent] = true;
                }
            }
        }
        plan.culled_passes = live.iter().filter(|&&alive| !alive).count();
        if plan.culled_passes != 0 {
            let mut next = 0;
            let remap: Vec<_> = live
                .iter()
                .map(|&alive| {
                    let i = next;
                    next += usize::from(alive);
                    i
                })
                .collect();
            graph.external_effects = graph.external_effects.iter().map(|&i| remap[i]).collect();
            let mut alive = live.iter();
            graph.passes.retain_mut(|pass| {
                for parent in &mut pass.after {
                    *parent = remap[*parent];
                }
                *alive.next().unwrap()
            });
            plan.steps.retain_mut(|step| {
                if !live[step.index] {
                    return false;
                }
                step.index = remap[step.index];
                for parent in &mut step.parents {
                    *parent = remap[*parent];
                }
                true
            });
            let mut alive = live.iter();
            plan.pass_uses.retain(|_| *alive.next().unwrap());
            // Range descriptors refer to the construction order; the backend only needs the set of split resources.
            graph.subranges.remap_passes(&live, &remap);
        }

        let mut used = vec![false; graph.resources.len()];
        for pass in &plan.pass_uses {
            for &(r, _) in pass {
                used[r.0] = true;
            }
        }
        // The driver consumes topology and device addresses indirectly, even for address-copy passes.
        for pass in &graph.passes {
            if let Op::BuildBlas { topology, .. } = pass.op {
                used[topology.0] = true;
            }
        }
        for (version, cache) in &graph.cluster_caches {
            if used[version.0] || used[cache.output.0] {
                for r in [*version, cache.output, cache.parameters] {
                    used[r.0] = true;
                }
            }
        }
        for (table, refs) in graph.addresses.iter().rev() {
            if used[table.0] {
                for r in refs {
                    used[r.0] = true;
                }
            }
        }
        graph.addresses.retain(|r, _| used[r.0]);
        graph.cluster_caches.retain(|r, _| used[r.0]);
        plan.unused.extend(
            used.iter()
                .enumerate()
                .filter_map(|(i, &v)| (!v).then_some(Resource(i))),
        );
        plan.compile_ms += started.elapsed().as_secs_f64() * 1000.;
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compute(graph: &mut Graph, label: &str, resource: Resource, access: Access) -> usize {
        graph.pass(
            label,
            Op::Compute {
                program: Dispatch {
                    shader: Shader::Compute(&[]),
                    bindings: vec![(resource, access)],
                    push: vec![],
                },
                groups: [1; 3],
            },
        )
    }

    #[test]
    fn frame_keeps_effects_history_and_explicit_parents_and_skips_dead_storage() -> Result<()> {
        let mut g = Graph::new([1, 1]);
        let dead = g.input("unused", vec![0; 16]);
        let out = g.input("out", vec![0; 4]);
        let history = g.history_buffer("next frame", 7, 16);
        let side = g.input("side effect", vec![0; 4]);
        compute(&mut g, "unused pass", dead, Access::ComputeWrite);
        let setup = compute(&mut g, "external setup", side, Access::ComputeRead);
        let effect = compute(&mut g, "external consumer", side, Access::ComputeRead);
        g.passes[effect].after.push(setup);
        g.mark_external_effect(effect)?;
        compute(&mut g, "persistent producer", history, Access::ComputeWrite);
        g.pass("export", Op::HostRead(out));
        let p = g.compile_frame()?;
        assert_eq!(p.culled_passes, 1);
        assert_eq!(p.unused, BTreeSet::from([dead]));
        assert_eq!(p.graph.passes[1].after, [0]);
        assert!(
            p.steps
                .iter()
                .find(|s| s.index == 1)
                .unwrap()
                .parents
                .contains(&0)
        );
        Ok(())
    }

    #[test]
    fn separate_ranges_run_independently_and_full_reader_joins_writers() -> Result<()> {
        let mut g = Graph::new([1, 1]);
        let r = g.resource(
            "mips",
            Storage::Buffer {
                bytes: 16,
                initial: None,
            },
        );
        let a = compute(&mut g, "first", r, Access::ComputeWrite);
        let b = compute(&mut g, "second", r, Access::ComputeWrite);
        assert!(
            g.buffer_ranges(a, r, vec![(0..17, Access::ComputeWrite)])
                .is_err()
        );
        assert!(
            g.buffer_ranges(
                a,
                r,
                vec![(0..9, Access::ComputeWrite), (8..10, Access::ComputeWrite)]
            )
            .is_err()
        );
        g.buffer_ranges(a, r, vec![(0..8, Access::ComputeWrite)])?;
        g.buffer_ranges(b, r, vec![(8..16, Access::ComputeWrite)])?;
        let c = compute(&mut g, "mix", r, Access::ComputeReadWrite);
        g.buffer_ranges(
            c,
            r,
            vec![(0..4, Access::ComputeRead), (4..8, Access::ComputeWrite)],
        )?;
        let out = g.pass("export", Op::HostRead(r));
        let plan = g.compile_frame()?;
        let parents = |i| &plan.steps.iter().find(|s| s.index == i).unwrap().parents;
        assert!(parents(a).is_empty() && parents(b).is_empty());
        assert_eq!(parents(c), &[a]);
        assert!(parents(out).contains(&b) && parents(out).contains(&c));
        Ok(())
    }

    #[test]
    fn unwritten_interval_is_rejected_even_if_another_interval_is_defined() -> Result<()> {
        let mut g = Graph::new([1, 1]);
        let r = g.resource(
            "partial",
            Storage::Buffer {
                bytes: 16,
                initial: None,
            },
        );
        let write = compute(&mut g, "low only", r, Access::ComputeWrite);
        g.buffer_ranges(write, r, vec![(0..8, Access::ComputeWrite)])?;
        g.pass("all", Op::HostRead(r));
        assert!(
            g.compile_frame()
                .err()
                .unwrap()
                .to_string()
                .contains("uninitialized")
        );
        Ok(())
    }
}
