//! Per-view reuse of dependency order and barriers. Frame data and operations stay in the graph.
use super::*;
use std::{collections::BTreeMap, mem::Discriminant};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Order {
    Local,
    Critical,
}

pub(crate) struct Compiler {
    entry: Option<Entry>,
    pub(super) order: Order,
    enabled: bool,
}
impl Compiler {
    pub(crate) fn new(order: Order, enabled: bool) -> Self {
        Self {
            entry: None,
            order,
            enabled,
        }
    }
    pub(crate) fn for_view() -> Self {
        let critical = std::env::var_os("RENDER_LAB_RDG_ORDER").is_some_and(|v| v == "critical");
        let enabled = std::env::var_os("RENDER_LAB_RDG_PLAN_CACHE").is_none_or(|v| v != "0");
        Self::new(
            if critical {
                Order::Critical
            } else {
                Order::Local
            },
            enabled,
        )
    }

    pub(super) fn lookup(&self, graph: &Graph) -> Option<&Template> {
        let entry = self.entry.as_ref()?;
        entry.matches(graph).then_some(&entry.template)
    }

    pub(super) fn remember(&mut self, plan: &Plan) {
        let graph = &plan.graph;
        // Keep at most one recently used shape per view. Bound the metadata retained between frames.
        if !self.enabled
            || graph.passes.len() > 4096
            || graph.resources.len() > 32768
            || graph.passes.iter().map(|p| p.after.len()).sum::<usize>() > 65536
            || graph.subranges.slice_count() > 65536
            || plan.pass_uses.iter().map(Vec::len).sum::<usize>() > 65536
            || plan
                .steps
                .iter()
                .map(|s| s.parents.len() + s.barriers.len())
                .sum::<usize>()
                > 131072
            || graph.addresses.values().map(Vec::len).sum::<usize>() > 65536
        {
            self.entry = None;
            return;
        }
        self.entry = Some(Entry {
            extent: graph.extent,
            output_extent: graph.output_extent(),
            resources: graph
                .resources
                .iter()
                .map(|(_, s)| ResourceShape::from(s))
                .collect(),
            passes: graph
                .passes
                .iter()
                .map(|p| (p.queue, std::mem::discriminant(&p.op), p.after.clone()))
                .collect(),
            bindings: graph.passes.iter().map(|p| p.op.uses()).collect(),
            addresses: graph.addresses.clone(),
            address_overrides: graph.address_overrides.clone(),
            children: graph.cluster_children.clone(),
            ranges: graph.subranges.clone(),
            range_sizes: graph.range_sizes().collect(),
            histories: graph.histories.keys().copied().collect(),
            versions: graph
                .cluster_caches
                .iter()
                .map(|(&v, c)| (v, c.output))
                .collect(),
            tables: graph.tables.keys().copied().collect(),
            resident: graph.resident_inputs.clone(),
            template: Template {
                uses: plan.pass_uses.clone(),
                steps: plan.steps.clone(),
                written: plan.written.clone(),
                images: plan.image_usage.clone(),
            },
        });
    }
}

pub(super) struct Template {
    pub uses: Vec<Vec<(Resource, Access)>>,
    pub steps: Vec<Step>,
    pub written: Vec<bool>,
    pub images: Vec<ImageUsage>,
}

#[derive(PartialEq, Eq)]
enum ResourceShape {
    Buffer(bool),
    Color,
    Depth,
    Image(ImageDesc),
    Acceleration(AccelerationShape),
}
impl From<&Storage> for ResourceShape {
    fn from(storage: &Storage) -> Self {
        match storage {
            Storage::Buffer { initial, .. } => Self::Buffer(initial.is_some()),
            Storage::Color => Self::Color,
            Storage::Depth => Self::Depth,
            Storage::Image(desc) => Self::Image(*desc),
            Storage::Acceleration(shape) => Self::Acceleration(*shape),
        }
    }
}
struct Entry {
    extent: [u32; 2],
    output_extent: [u32; 2],
    resources: Vec<ResourceShape>,
    passes: Vec<(Queue, Discriminant<Op>, Vec<usize>)>,
    bindings: Vec<Vec<(Resource, Access)>>,
    addresses: BTreeMap<Resource, Vec<Resource>>,
    address_overrides: BTreeMap<Resource, Option<Access>>,
    children: BTreeMap<Resource, Resource>,
    ranges: ranges::Declarations,
    range_sizes: Vec<usize>,
    histories: Vec<Resource>,
    versions: Vec<(Resource, Resource)>,
    tables: Vec<Resource>,
    resident: BTreeSet<Resource>,
    template: Template,
}
impl Entry {
    fn matches(&self, g: &Graph) -> bool {
        self.extent == g.extent
            && self.output_extent == g.output_extent()
            && self.passes.len() == g.passes.len()
            && self.resources.len() == g.resources.len()
            && self.ranges == g.subranges
            && self.range_sizes.iter().copied().eq(g.range_sizes())
            && self.addresses == g.addresses
            && self.address_overrides == g.address_overrides
            && self.children == g.cluster_children
            && self.resident == g.resident_inputs
            && self.histories.iter().eq(g.histories.keys())
            && self.tables.iter().eq(g.tables.keys())
            && self
                .versions
                .iter()
                .copied()
                .eq(g.cluster_caches.iter().map(|(&v, c)| (v, c.output)))
            && self
                .resources
                .iter()
                .zip(&g.resources)
                .all(|(a, (_, b))| *a == ResourceShape::from(b))
            && self
                .passes
                .iter()
                .zip(&g.passes)
                .all(|((queue, op, after), p)| {
                    *queue == p.queue && *op == std::mem::discriminant(&p.op) && *after == p.after
                })
            && self
                .bindings
                .iter()
                .zip(&g.passes)
                .all(|(uses, p)| *uses == p.op.uses())
    }
}

/// Longest downstream dependency path in pass units. A reverse traversal also handles
/// explicit forward references. The compiler's normal topological check rejects cycles.
pub(super) fn heights(parents: &[BTreeSet<usize>], children: &[Vec<usize>]) -> Vec<usize> {
    let mut count: Vec<_> = children.iter().map(Vec::len).collect();
    let mut ends: Vec<_> = (0..count.len()).filter(|&i| count[i] == 0).collect();
    let mut height = vec![1; count.len()];
    while let Some(i) = ends.pop() {
        for &p in &parents[i] {
            height[p] = height[p].max(height[i] + 1);
            count[p] -= 1;
            if count[p] == 0 {
                ends.push(p);
            }
        }
    }
    height
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dispatch_graph(value: u8, offset: u64, end: usize, queue: Queue) -> Result<Graph> {
        let mut g = Graph::new([1, 1]);
        let args = g.input("arguments", vec![value; 16]);
        let dst = g.input("result", vec![0; 16]);
        let pass = g.pass(
            "producer",
            Op::ComputeIndirect {
                program: Dispatch {
                    shader: Shader::Compute(&[]),
                    bindings: vec![(dst, Access::ComputeWrite)],
                    push: vec![value; 4],
                },
                args,
                offset,
            },
        );
        g.buffer_ranges(pass, dst, vec![(0..end, Access::ComputeWrite)])?;
        g.passes[pass].queue = queue;
        g.pass("read", Op::HostRead(dst));
        Ok(g)
    }

    #[test]
    fn template_reuses_dependencies_but_checks_current_commands_and_ranges() -> Result<()> {
        let mut compiler = Compiler::new(Order::Local, true);
        let first = dispatch_graph(1, 0, 8, Queue::Compute)?.compile_with(Some(&mut compiler))?;
        assert!(!first.template_hit);
        let hit = dispatch_graph(3, 4, 8, Queue::Compute)?.compile_with(Some(&mut compiler))?;
        let uncached = dispatch_graph(3, 4, 8, Queue::Compute)?.compile()?;
        assert!(hit.template_hit);
        assert_eq!(hit.steps, uncached.steps);
        assert_eq!(hit.written, uncached.written);
        assert!(
            matches!(&hit.graph.passes[0].op, Op::ComputeIndirect { program, offset: 4, .. } if program.push == [3; 4])
        );
        assert!(
            dispatch_graph(5, 8, 8, Queue::Compute)?
                .compile_with(Some(&mut compiler))
                .err()
                .unwrap()
                .to_string()
                .contains("indirect argument range")
        );
        let mut resized = dispatch_graph(1, 0, 8, Queue::Compute)?;
        resized.resources[0].1 = Storage::Buffer {
            bytes: 12,
            initial: Some(vec![0; 12].into()),
        };
        assert!(resized.compile_with(Some(&mut compiler))?.template_hit);
        assert!(
            !dispatch_graph(7, 0, 12, Queue::Compute)?
                .compile_with(Some(&mut compiler))?
                .template_hit
        );
        assert!(
            !dispatch_graph(7, 0, 12, Queue::Graphics)?
                .compile_with(Some(&mut compiler))?
                .template_hit
        );
        let mut cycle = dispatch_graph(7, 0, 12, Queue::Graphics)?;
        cycle.passes[0].after.push(1);
        assert!(cycle.compile_with(Some(&mut compiler)).is_err());
        Ok(())
    }

    #[test]
    fn pointer_table_changes_and_buffer_initialization_invalidate_templates() -> Result<()> {
        let graph = |swap: bool, initialized: bool| {
            let mut g = Graph::new([1, 1]);
            let a = g.resource(
                "a",
                Storage::Buffer {
                    bytes: 16,
                    initial: initialized.then(|| vec![0; 16].into()),
                },
            );
            let b = g.input("b", vec![0; 16]);
            let table = g.address_table(vec![if swap { b } else { a }]);
            g.pass(
                "read pointers",
                Op::Compute {
                    program: Dispatch {
                        shader: Shader::Compute(&[]),
                        bindings: vec![(table, Access::ComputeRead)],
                        push: vec![],
                    },
                    groups: [1; 3],
                },
            );
            g
        };
        let mut compiler = Compiler::new(Order::Local, true);
        assert!(
            !graph(false, true)
                .compile_with(Some(&mut compiler))?
                .template_hit
        );
        assert!(
            graph(false, true)
                .compile_with(Some(&mut compiler))?
                .template_hit
        );
        let swap = graph(true, true).compile_with(Some(&mut compiler))?;
        assert!(!swap.template_hit);
        assert_eq!(swap.pass_uses, graph(true, true).compile()?.pass_uses);
        assert!(
            graph(false, false)
                .compile_with(Some(&mut compiler))
                .is_err()
        );
        Ok(())
    }

    fn branches() -> Graph {
        let mut g = Graph::new([1, 1]);
        for name in ["side effect", "last", "first", "middle"] {
            g.pass(
                name,
                Op::Compute {
                    program: Dispatch {
                        shader: Shader::Compute(&[]),
                        bindings: vec![],
                        push: vec![],
                    },
                    groups: [1; 3],
                },
            );
        }
        g.passes[1].after.push(3);
        g.passes[3].after.push(2);
        g
    }

    #[test]
    fn stable_schedule_rechecks_liveness_and_critical_order_supports_forward_edges() -> Result<()> {
        let mut compiler = Compiler::new(Order::Critical, true);
        let plan = branches().compile_with(Some(&mut compiler))?;
        let order = plan.steps.iter().map(|s| s.index).collect::<Vec<_>>();
        assert_eq!(&order[..2], &[2, 3]);
        let mut g = branches();
        g.mark_external_effect(1)?;
        let live = g.compile_frame_with(Some(&mut compiler))?;
        assert!(live.template_hit);
        assert_eq!(live.culled_passes, 1);
        let mut g = branches();
        g.mark_external_effect(0)?;
        let live = g.compile_frame_with(Some(&mut compiler))?;
        assert!(live.template_hit);
        assert_eq!(live.culled_passes, 3);
        assert_eq!(live.graph.passes[0].name, "side effect");
        Ok(())
    }
}
