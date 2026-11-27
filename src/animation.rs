//! A display pose has one mutable owner. Compile named pose links and read-only clip
//! inputs into a linear evaluation program before dispatching character workers.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoseGraph {
    pub output: String,
    pub nodes: BTreeMap<String, Node>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Node {
    Input,
    Clip {
        clip: String,
        #[serde(default = "one")]
        speed: f32,
    },
    MaskedBlend {
        input: String,
        overlay: String,
        mask: BTreeMap<String, f32>,
        #[serde(default = "one")]
        weight: f32,
        /// Seconds since graph activation: start, full weight, start of release, end.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<[f32; 4]>,
    },
    PhysicalLimbs {
        input: String,
    },
    FootPlacement {
        input: String,
    },
    HandIk {
        input: String,
    },
}
fn one() -> f32 {
    1.
}

#[derive(Debug)]
pub struct RotationLayer {
    /// Source clip frames in the bank; channels index the retargeted local rotation tracks.
    pub frames: Range<usize>,
    pub speed: f32,
    pub channels: Vec<(usize, f32)>,
    window: Option<[f32; 4]>,
}

impl RotationLayer {
    pub fn envelope(&self, seconds: f64) -> f32 {
        let Some([start, full, release, end]) = self.window else {
            return 1.;
        };
        let t = seconds as f32;
        if t < start || t >= end {
            return 0.;
        }
        let weight = if t < full {
            (t - start) / (full - start)
        } else if t > release {
            (end - t) / (end - release)
        } else {
            return 1.;
        };
        weight * weight * (3. - 2. * weight)
    }
}

#[derive(Debug)]
pub enum Operation {
    /// Adjacent local-space blends share one world-transform refresh.
    Blend(Vec<RotationLayer>),
    PhysicalLimbs,
    FootPlacement,
    HandIk,
}

#[derive(Debug)]
pub struct PoseProgram {
    pub(crate) ops: Vec<Operation>,
    pub(crate) uses_clips: bool,
    pub(crate) has_feet: bool,
    pub(crate) has_hand: bool,
    pub(crate) has_limbs: bool,
}
impl PoseProgram {
    pub fn operations(&self) -> &[Operation] {
        &self.ops
    }
}

impl PoseGraph {
    /// Links are resolved from the output back to the authored pose. A clip can feed
    /// several layers. Feedback operators own per-actor state and may occur once each.
    pub fn compile(
        &self,
        channels: &[String],
        clips: &[(Range<usize>, String)],
    ) -> Result<PoseProgram> {
        ensure!(
            !self.nodes.is_empty() && self.nodes.len() <= 256,
            "pose graph requires 1..=256 nodes"
        );
        let mut used = BTreeSet::new();
        let mut name = &self.output;
        let mut ops = Vec::new();
        let (mut feet, mut hand, mut limbs) = (false, false, false);
        let mut uses_clips = false;
        loop {
            ensure!(used.insert(name), "cycle or invalid pose link at {name}");
            let node = self
                .nodes
                .get(name)
                .with_context(|| format!("unknown pose input {name}"))?;
            let (next, op) = match node {
                Node::Input => break,
                Node::Clip { .. } => bail!("clip node {name} cannot supply a base pose"),
                Node::PhysicalLimbs { input } => {
                    ensure!(!limbs, "physical limbs have two writers");
                    limbs = true;
                    (input, Operation::PhysicalLimbs)
                }
                Node::FootPlacement { input } => {
                    ensure!(!feet, "foot placement has two writers");
                    feet = true;
                    (input, Operation::FootPlacement)
                }
                Node::HandIk { input } => {
                    ensure!(!hand, "hand IK has two writers");
                    hand = true;
                    (input, Operation::HandIk)
                }
                Node::MaskedBlend {
                    input,
                    overlay,
                    mask,
                    weight,
                    window,
                } => {
                    let Some(Node::Clip { clip, speed }) = self.nodes.get(overlay) else {
                        bail!("layer {name} needs a clip at {overlay}")
                    };
                    used.insert(overlay);
                    ensure!(
                        speed.is_finite() && (0.0..=16.).contains(speed),
                        "invalid clip speed at {overlay}"
                    );
                    ensure!(
                        (0.0..=1.).contains(weight) && !mask.is_empty(),
                        "invalid layer weight or empty mask at {name}"
                    );
                    if let Some(times) = window {
                        ensure!(
                            times.iter().all(|v| v.is_finite() && *v >= 0.)
                                && times.windows(2).all(|p| p[0] <= p[1])
                                && times[0] < times[3],
                            "invalid blend window at {name}"
                        );
                    }
                    let frames = clips
                        .iter()
                        .find(|(_, id)| id == clip)
                        .with_context(|| format!("unknown animation clip {clip}"))?
                        .0
                        .clone();
                    ensure!(!frames.is_empty(), "empty clip {clip}");
                    let mut bound = Vec::with_capacity(mask.len());
                    for (bone, w) in mask {
                        ensure!((0.0..=1.).contains(w), "invalid mask weight for {bone}");
                        let i = channels
                            .iter()
                            .position(|c| c == bone)
                            .with_context(|| format!("unknown mask channel {bone}"))?;
                        if weight * w > 0. {
                            bound.push((i, weight * w));
                        }
                    }
                    if bound.is_empty() {
                        name = input;
                        continue;
                    }
                    uses_clips = true;
                    (
                        input,
                        Operation::Blend(vec![RotationLayer {
                            frames,
                            speed: *speed,
                            channels: bound,
                            window: *window,
                        }]),
                    )
                }
            };
            match (ops.last_mut(), op) {
                (Some(Operation::Blend(existing)), Operation::Blend(layers)) => {
                    existing.extend(layers)
                }
                (_, op) => ops.push(op),
            }
            name = next;
        }
        ensure!(
            used.len() == self.nodes.len(),
            "unreachable pose graph nodes: {:?}",
            self.nodes
                .keys()
                .filter(|n| !used.contains(n))
                .collect::<Vec<_>>()
        );
        ops.reverse();
        for op in &mut ops {
            if let Operation::Blend(layers) = op {
                layers.reverse();
            }
        }
        Ok(PoseProgram {
            ops,
            uses_clips,
            has_feet: feet,
            has_hand: hand,
            has_limbs: limbs,
        })
    }
}
