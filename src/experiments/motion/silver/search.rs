use std::ops::Range;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

pub(super) type Feature = [f32; 24];
type Point = [f32; 6];

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Mode {
    #[default]
    Kd,
    Scan,
}

#[derive(Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Candidate {
    Idle,
    Transition,
    Steady,
}

#[derive(Deserialize)]
struct Node {
    low: Point,
    high: Point,
    first: usize,
    count: usize,
}
impl Node {
    fn lower_bound(&self, point: &Point) -> f32 {
        (0..point.len())
            .map(|i| {
                let delta = (self.low[i] - point[i])
                    .max(point[i] - self.high[i])
                    .max(0.);
                delta * delta
            })
            .sum()
    }
}

#[derive(Deserialize)]
pub(super) struct Index {
    version: u32,
    mean: Feature,
    gain: Feature,
    axes: [Feature; 6],
    nodes: Vec<Node>,
    order: Vec<usize>,
    #[serde(skip)]
    samples: Vec<Feature>,
    #[serde(skip)]
    candidates: Vec<Candidate>,
}

pub(super) struct Hit {
    pub sample: usize,
    pub cost: f32,
    pub evaluated: usize,
}

struct Query<'a> {
    features: Feature,
    span: &'a Range<usize>,
    next: usize,
    class: Option<Candidate>,
}

#[derive(Deserialize)]
struct CookedIndex<'a> {
    format: u32,
    source_key: &'a str,
    tree_sha256: &'a str,
    #[serde(borrow)]
    tree: &'a serde_json::value::RawValue,
}

fn source_key() -> &'static str {
    use sha2::{Digest, Sha256};
    static KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        let mut hash = Sha256::new();
        hash.update(b"render-lab/motion-search/2\0");
        for text in [
            include_str!("../../../../tools/motion_index.py"),
            super::INDEX,
        ] {
            hash.update(text.replace("\r\n", "\n").as_bytes());
            hash.update([0]);
        }
        hash.update(super::DATA);
        format!("{:x}", hash.finalize())
    })
}

fn read_tree(text: &str, expected: &str) -> Result<Index> {
    use sha2::{Digest, Sha256};
    let cooked: CookedIndex<'_> = serde_json::from_str(text)?;
    ensure!(
        cooked.format == 2 && cooked.source_key == expected,
        "stale motion search index; run python tools/motion_index.py"
    );
    ensure!(
        format!("{:x}", Sha256::digest(cooked.tree.get().as_bytes())) == cooked.tree_sha256,
        "damaged motion search index; run python tools/motion_index.py"
    );
    Ok(serde_json::from_str(cooked.tree.get())?)
}

impl Index {
    pub fn load(raw: Vec<Feature>, clips: &[(Range<usize>, String)]) -> Result<Self> {
        let mut index = read_tree(
            include_str!("../../../../assets/motion/search.json"),
            source_key(),
        )?;
        ensure!(
            index.version == 1 && !index.nodes.is_empty(),
            "invalid motion index version or root"
        );
        let expected: Vec<_> = clips
            .iter()
            .flat_map(|(r, _)| r.start..r.end - super::LOOKAHEAD)
            .collect();
        let mut candidates = index.order.clone();
        candidates.sort_unstable();
        ensure!(
            expected == candidates,
            "motion index does not cover the pose bank"
        );
        for (i, node) in index.nodes.iter().enumerate() {
            let valid = if node.count == 0 {
                node.first > i && node.first < index.nodes.len() - 1
            } else {
                node.first
                    .checked_add(node.count)
                    .is_some_and(|end| end <= index.order.len())
            };
            ensure!(valid, "invalid motion tree node");
        }
        index.samples = raw.iter().map(|row| index.normalize(row)).collect();
        index.candidates = vec![Candidate::Transition; raw.len()];
        for (span, name) in clips {
            index.candidates[span.clone()].fill(
                *super::profile::DEFAULT
                    .clips
                    .get(name)
                    .with_context(|| format!("missing animation category for clip {name}"))?,
            );
        }
        Ok(index)
    }
    pub fn candidate(&self, sample: usize) -> Candidate {
        self.candidates[sample]
    }
    fn normalize(&self, raw: &Feature) -> Feature {
        std::array::from_fn(|i| (raw[i] - self.mean[i]) * self.gain[i])
    }
    fn score(&self, id: usize, query: &Query, hit: &mut Hit) {
        if query
            .class
            .is_some_and(|class| class != self.candidates[id])
        {
            return;
        }
        if id != query.next && id.abs_diff(query.next) < 8 && query.span.contains(&id) {
            return;
        }
        let mut cost = if id == query.next {
            0.
        } else if query.span.contains(&id) {
            0.28
        } else {
            0.42
        };
        for (a, b) in self.samples[id].iter().zip(query.features.iter()) {
            let delta = a - b;
            cost += delta * delta;
        }
        hit.evaluated += 1;
        if cost < hit.cost || (cost == hit.cost && id < hit.sample) {
            hit.cost = cost;
            hit.sample = id;
        }
    }
    fn visit(&self, node: usize, projected: &Point, query: &Query, hit: &mut Hit) {
        let node = &self.nodes[node];
        if node.count > 0 {
            for &id in &self.order[node.first..node.first + node.count] {
                if id != query.next {
                    self.score(id, query, hit);
                }
            }
            return;
        }
        let children = [node.first, node.first + 1];
        let mut bounds = children.map(|id| (id, self.nodes[id].lower_bound(projected)));
        if bounds[1].1 < bounds[0].1 {
            bounds.swap(0, 1);
        }
        for (child, lower) in bounds {
            // Orthonormal projection cannot increase a distance. Leave room
            // for rounding in the baked float32 bounds and runtime dot products.
            if lower <= hit.cost * 1.00001 + 0.0001 {
                self.visit(child, projected, query, hit);
            }
        }
    }
    pub fn nearest(
        &self,
        raw: &Feature,
        mode: Mode,
        next: usize,
        span: &Range<usize>,
        clips: &[(Range<usize>, String)],
        class: Option<Candidate>,
    ) -> Hit {
        let query = Query {
            features: self.normalize(raw),
            span,
            next,
            class,
        };
        let mut hit = Hit {
            sample: usize::MAX,
            cost: f32::INFINITY,
            evaluated: 0,
        };
        match mode {
            Mode::Scan => {
                for (r, _) in clips {
                    for id in r.start..r.end - super::LOOKAHEAD {
                        self.score(id, &query, &mut hit);
                    }
                }
            }
            Mode::Kd => {
                if next + super::LOOKAHEAD < span.end {
                    self.score(next, &query, &mut hit);
                }
                let projected = self.axes.map(|axis| {
                    axis.iter()
                        .zip(query.features.iter())
                        .map(|(a, b)| a * b)
                        .sum()
                });
                self.visit(0, &projected, &query, &mut hit);
            }
        }
        hit
    }
}

#[cfg(test)]
mod asset_tests {
    use super::*;
    #[test]
    fn refuses_stale_or_corrupt_cooked_tree() {
        let data = include_str!("../../../../assets/motion/search.json");
        assert!(read_tree(data, source_key()).is_ok());
        assert!(read_tree(data, "different bank").is_err());
        let corrupt = data.replace("\"mean\":", "\"other\":");
        assert!(
            format!("{:#}", read_tree(&corrupt, source_key()).err().unwrap()).contains("damaged")
        );
    }
}
