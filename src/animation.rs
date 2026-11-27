//! A display pose has one mutable owner. Compile named pose links and read-only clip
//! inputs into a linear evaluation program before dispatching character workers.
mod program;
pub use program::{Operation, PoseProgram, RotationLayer};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
