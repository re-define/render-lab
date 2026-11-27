//! Cache interval expansion on a schedule miss. The per-view schedule cache owns the compiled order.
use super::*;
use std::sync::Mutex;

type Bindings = Vec<(Resource, Access)>;

pub(super) struct Expansion {
    pub physical: Vec<Resource>,
    pub uses: Vec<Bindings>,
}

impl Expansion {
    fn build(graph: &Graph, raw: &[Bindings]) -> Self {
        let tracks = ranges::Tracks::new(graph);
        let uses = raw
            .iter()
            .enumerate()
            .map(|(pass, bindings)| tracks.uses(graph, pass, bindings))
            .collect();
        Self {
            physical: tracks.physical,
            uses,
        }
    }
}

struct Entry {
    resources: usize,
    // Buffer size determines whether a declaration ends at an interior interval boundary.
    sizes: Vec<usize>,
    declarations: ranges::Declarations,
    raw: Vec<Bindings>,
    result: Arc<Expansion>,
}

/// At most one graph shape is retained across views and threads. All comparisons are exact.
#[derive(Default)]
pub(super) struct Cache(Mutex<Option<Entry>>);
pub(super) static CACHE: Cache = Cache(Mutex::new(None));

pub(super) fn expand(
    graph: &Graph,
    raw: &[Bindings],
    cache: Option<&Cache>,
) -> Option<Arc<Expansion>> {
    if graph.subranges.split.is_empty() {
        return None;
    }
    let build = || Arc::new(Expansion::build(graph, raw));
    let Some(cache) = cache else {
        return Some(build());
    };
    let sizes: Vec<_> = graph.range_sizes().collect();
    {
        let slot = cache.0.lock().unwrap();
        if let Some(entry) = slot.as_ref()
            && entry.resources == graph.resources.len()
            && entry.sizes == sizes
            && entry.declarations == graph.subranges
            && entry.raw == raw
        {
            return Some(entry.result.clone());
        }
    }
    // Build outside the lock. Concurrent callers may safely publish equivalent or different shapes.
    let result = build();
    let entries = result.uses.iter().map(Vec::len).sum::<usize>();
    if entries <= 65536
        && raw.len() <= 8192
        && graph.resources.len() <= 65536
        && graph.subranges.slice_count() <= 65536
    {
        let next = Entry {
            resources: graph.resources.len(),
            sizes,
            declarations: graph.subranges.clone(),
            raw: raw.to_vec(),
            result: result.clone(),
        };
        *cache.0.lock().unwrap() = Some(next);
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workload(buffers: usize, words: usize, frame: u32) -> Result<Graph> {
        let mut g = Graph::new([64, 64]);
        for _ in 0..buffers {
            let r = g.input("levels", vec![0; words * 4]);
            for word in 0..words {
                let pass = g.pass(
                    "new level",
                    Op::Compute {
                        program: Dispatch {
                            shader: Shader::Compute(&[]),
                            bindings: vec![(r, Access::ComputeReadWrite)],
                            push: frame.to_ne_bytes().to_vec(),
                        },
                        groups: [1; 3],
                    },
                );
                let start = 4 * word;
                let mut ranges = vec![(start..start + 4, Access::ComputeWrite)];
                if word != 0 {
                    ranges.push((start - 4..start, Access::ComputeRead));
                }
                g.buffer_ranges(pass, r, ranges)?;
            }
            g.pass("read all levels", Op::HostRead(r));
        }
        Ok(g)
    }

    fn expanded(graph: &Graph, cache: &Cache) -> Arc<Expansion> {
        let uses: Vec<_> = graph.passes.iter().map(|p| graph.uses(&p.op)).collect();
        expand(graph, &uses, Some(cache)).unwrap()
    }

    #[test]
    fn exact_topology_hits_preserve_validation_and_change_with_the_input() -> Result<()> {
        let cache = Cache::default();
        let first = expanded(&workload(4, 8, 1)?, &cache);
        let mut next = workload(4, 8, 2)?;
        assert!(Arc::ptr_eq(&first, &expanded(&next, &cache)));
        let expected = workload(4, 8, 9)?.compile_with_ranges(None)?;
        let actual = workload(4, 8, 11)?.compile_with_ranges(Some(&cache))?;
        assert_eq!(expected.pass_uses, actual.pass_uses);
        assert_eq!(expected.written, actual.written);
        assert_eq!(expected.steps.len(), actual.steps.len());
        for (a, b) in expected.steps.iter().zip(&actual.steps) {
            assert_eq!((a.index, &a.parents), (b.index, &b.parents));
            assert_eq!(a.barriers.len(), b.barriers.len());
            for (a, b) in a.barriers.iter().zip(&b.barriers) {
                assert_eq!((a.resource, &a.from, a.to), (b.resource, &b.from, b.to));
            }
        }

        // The dispatch command is validated even with unchanged binding intervals.
        let pass_id = next.passes.len();
        next.passes[0].after.push(pass_id);
        assert!(next.compile_with_ranges(Some(&cache)).is_err());

        let mut resized = workload(4, 8, 1)?;
        if let Storage::Buffer { bytes, .. } = &mut resized.resources[0].1 {
            *bytes += 8;
        }
        let bigger = expanded(&resized, &cache);
        assert!(!Arc::ptr_eq(&first, &bigger));
        assert_eq!(bigger.physical.len(), first.physical.len() + 1);
        // An indirect consumer can change its bindings without changing any range declarations.
        let mut other_binding = workload(4, 8, 2)?;
        let old = expanded(&other_binding, &cache);
        other_binding.passes[8].op = Op::HostRead(Resource(1));
        assert!(!Arc::ptr_eq(&old, &expanded(&other_binding, &cache)));
        // Different interval boundaries with the same pass and resource counts.
        let mut other_ranges = workload(4, 8, 2)?;
        other_ranges.subranges = Default::default();
        other_ranges.buffer_ranges(0, Resource(0), vec![(4..16, Access::ComputeWrite)])?;
        assert!(!Arc::ptr_eq(&old, &expanded(&other_ranges, &cache)));
        Ok(())
    }

    #[test]
    #[ignore = "release CPU microbenchmark of interval expansion and complete RDG compilation"]
    fn range_topology_compile_profile() -> Result<()> {
        let cache = Cache::default();
        let mut times = [Vec::new(), Vec::new()];
        let mut expansions = [Vec::new(), Vec::new()];
        // 32 independent buffers, 8 overlapping level operations and one export each.
        for frame in 0..112u32 {
            for cached in [frame % 2 == 0, frame % 2 != 0] {
                let graph = workload(32, 8, frame)?;
                let uses: Vec<_> = graph.passes.iter().map(|p| graph.uses(&p.op)).collect();
                let cache = cached.then_some(&cache);
                let start = std::time::Instant::now();
                let result = expand(&graph, &uses, cache).unwrap();
                std::hint::black_box(&result);
                let us = start.elapsed().as_secs_f64() * 1e6;
                let plan = graph.compile_with_ranges(cache)?;
                std::hint::black_box(&plan.steps);
                if frame >= 16 {
                    times[usize::from(cached)].push(plan.compile_ms * 1000.);
                    expansions[usize::from(cached)].push(us);
                }
            }
        }
        for (label, mut series) in [("expanded bindings", expansions), ("full compile", times)] {
            for (mode, samples) in ["uncached", "cached"].into_iter().zip(&mut series) {
                samples.sort_by(f64::total_cmp);
                println!(
                    "{label} {mode}: P50 {:.2} P95 {:.2} us ({} samples)",
                    samples[47],
                    samples[91],
                    samples.len()
                );
            }
        }
        Ok(())
    }
}
