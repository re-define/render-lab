//! Authoring values for an anchored two-link chain. Flexion is zero on a straight limb.
use anyhow::{Result, ensure};
use glam::Vec3;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BendRange {
    pub min_degrees: f32,
    pub max_degrees: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimbDrive {
    /// Positional drive compliance in m/N. Zero is a hard drive.
    pub compliance: f32,
    /// Exponential velocity damping in inverse seconds.
    pub damping: f32,
    pub bend: Option<BendRange>,
}
impl Default for LimbDrive {
    fn default() -> Self {
        Self {
            compliance: 0.008,
            damping: 5.,
            bend: None,
        }
    }
}
impl LimbDrive {
    pub fn validate(self) -> Result<()> {
        ensure!(
            self.compliance.is_finite()
                && self.compliance >= 0.
                && self.damping.is_finite()
                && self.damping >= 0.,
            "invalid limb compliance or damping"
        );
        if let Some(r) = self.bend {
            ensure!(
                (0. ..=180.).contains(&r.min_degrees)
                    && (r.min_degrees..=180.).contains(&r.max_degrees),
                "flexion limits require 0 <= minimum <= maximum <= 180 degrees"
            );
        }
        Ok(())
    }
}

pub(super) struct Projection {
    min: (f32, f32),
    max: (f32, f32),
}
impl BendRange {
    pub(super) fn projection(self) -> Projection {
        Projection {
            min: self.min_degrees.to_radians().sin_cos(),
            max: self.max_degrees.to_radians().sin_cos(),
        }
    }
}
impl Projection {
    pub(super) fn apply(&self, p: &mut [Vec3; 3], previous: [Vec3; 3], goals: [Vec3; 3]) {
        let Some(axis) = (p[1] - p[0]).try_normalize() else {
            return;
        };
        let distal = p[2] - p[1];
        let Some(direction) = distal.try_normalize() else {
            return;
        };
        let dot = axis.dot(direction).clamp(-1., 1.);
        let (sin, cos) = if dot > self.min.1 {
            self.min
        } else if dot < self.max.1 {
            self.max
        } else {
            return;
        };
        // A collinear arm has no bending plane. Prefer a previous or authored plane
        // before choosing a deterministic perpendicular for a perfectly straight rig.
        let tangent = [direction, previous[2] - previous[1], goals[2] - goals[1]]
            .into_iter()
            .find_map(|v| {
                let plane = v - axis * axis.dot(v);
                (plane.length_squared() > 1e-10 * v.length_squared()).then(|| plane.normalize())
            })
            .unwrap_or_else(|| axis.any_orthonormal_vector());
        p[2] = p[1] + distal.length() * (cos * axis + sin.max(0.) * tangent);
    }
}
