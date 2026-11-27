//! Serializable stationary or moving box shapes and a refittable broad phase.
//! Queries use affine-transformed boxes in the caller's simulation frame.
use crate::scene::{Id, World};
use anyhow::{Result, ensure};
use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

pub const COLLIDER: &str = "engine.physics.box";

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoxSurface {
    Solid,
    Platform,
    Anchor,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BoxCollider {
    pub surface: BoxSurface,
    pub center: Vec3,
    pub half_extents: Vec3,
    pub layers: u32,
}
impl Default for BoxCollider {
    fn default() -> Self {
        Self {
            surface: BoxSurface::Solid,
            center: Vec3::ZERO,
            half_extents: Vec3::splat(0.5),
            layers: u32::MAX,
        }
    }
}
impl BoxCollider {
    pub fn validate(self) -> Result<()> {
        ensure!(
            self.center.is_finite()
                && self.half_extents.is_finite()
                && self.half_extents.min_element() > 0.,
            "invalid collider bounds"
        );
        Ok(())
    }
    pub(crate) fn box_transform(self, model: Mat4) -> Mat4 {
        model
            * Mat4::from_scale_rotation_translation(
                self.half_extents * 2.,
                glam::Quat::IDENTITY,
                self.center,
            )
    }
}
impl World {
    pub fn set_box_collider(&mut self, id: Id, collider: BoxCollider) -> Result<()> {
        ensure!(
            self.entities.contains_key(&id),
            "unknown collider owner {id}"
        );
        collider.validate()?;
        // Shape definitions are authored data, including when entities are cloned for another lane.
        self.set_setting(id, COLLIDER, serde_json::to_value(collider)?);
        Ok(())
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct Capsule {
    pub radius: f32,
    pub height: f32,
    pub mask: u32,
}
impl Default for Capsule {
    fn default() -> Self {
        Self {
            radius: 0.18,
            height: 1.45,
            mask: u32::MAX,
        }
    }
}
impl Capsule {
    pub fn validate(self) -> Result<()> {
        ensure!(
            self.radius.is_finite()
                && self.radius > 0.
                && self.height.is_finite()
                && self.height >= 2. * self.radius,
            "invalid upright capsule"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Bounds {
    pub lo: Vec3,
    pub hi: Vec3,
}
impl Bounds {
    pub fn for_box(m: Mat4) -> Self {
        let center = m.w_axis.truncate();
        let extent =
            (m.x_axis.truncate().abs() + m.y_axis.truncate().abs() + m.z_axis.truncate().abs())
                * 0.5;
        Self {
            lo: center - extent,
            hi: center + extent,
        }
    }
    pub fn transformed(self, m: Mat4) -> Self {
        Self::for_box(
            m * Mat4::from_scale_rotation_translation(
                self.hi - self.lo,
                glam::Quat::IDENTITY,
                (self.lo + self.hi) * 0.5,
            ),
        )
    }
    pub fn union(self, b: Self) -> Self {
        Self {
            lo: self.lo.min(b.lo),
            hi: self.hi.max(b.hi),
        }
    }
    fn overlaps(self, b: Self) -> bool {
        self.hi.cmpge(b.lo).all() && self.lo.cmple(b.hi).all()
    }
}
#[derive(Clone)]
enum Children {
    Leaf(usize),
    Pair(usize, usize),
}
#[derive(Clone, Default)]
pub(crate) struct BoxIndex {
    nodes: Vec<(Bounds, Children)>,
}
impl BoxIndex {
    pub fn build(bounds: &[Bounds]) -> Self {
        fn branch(
            nodes: &mut Vec<(Bounds, Children)>,
            bounds: &[Bounds],
            ids: &mut [usize],
        ) -> usize {
            let area = ids
                .iter()
                .map(|&i| bounds[i])
                .reduce(Bounds::union)
                .unwrap();
            let children = if ids.len() == 1 {
                Children::Leaf(ids[0])
            } else {
                let d = area.hi - area.lo;
                let axis = if d.x >= d.y && d.x >= d.z {
                    0
                } else if d.z >= d.y {
                    2
                } else {
                    1
                };
                let mid = ids.len() / 2;
                ids.select_nth_unstable_by(mid, |&a, &b| {
                    (bounds[a].lo[axis] + bounds[a].hi[axis])
                        .total_cmp(&(bounds[b].lo[axis] + bounds[b].hi[axis]))
                        .then(a.cmp(&b))
                });
                let (left, right) = ids.split_at_mut(mid);
                Children::Pair(branch(nodes, bounds, left), branch(nodes, bounds, right))
            };
            nodes.push((area, children));
            nodes.len() - 1
        }
        let mut nodes = Vec::with_capacity(bounds.len() * 2);
        if !bounds.is_empty() {
            branch(
                &mut nodes,
                bounds,
                &mut (0..bounds.len()).collect::<Vec<_>>(),
            );
        }
        Self { nodes }
    }
    pub fn refit(&mut self, bounds: &[Bounds]) {
        for i in 0..self.nodes.len() {
            self.nodes[i].0 = match self.nodes[i].1 {
                Children::Leaf(k) => bounds[k],
                Children::Pair(a, b) => self.nodes[a].0.union(self.nodes[b].0),
            };
        }
    }
    pub fn query(&self, area: Bounds, mut found: impl FnMut(usize)) {
        fn visit(nodes: &[(Bounds, Children)], i: usize, area: Bounds, f: &mut impl FnMut(usize)) {
            if !nodes[i].0.overlaps(area) {
                return;
            }
            match nodes[i].1 {
                Children::Leaf(k) => f(k),
                Children::Pair(a, b) => {
                    visit(nodes, a, area, f);
                    visit(nodes, b, area, f);
                }
            }
        }
        if let Some(i) = self.nodes.len().checked_sub(1) {
            visit(&self.nodes, i, area, &mut found);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refitted_hierarchy_finds_exactly_the_linear_scan_after_cell_crossings() {
        let mut bounds: Vec<_> = (0..1024)
            .map(|i| {
                Bounds::for_box(Mat4::from_translation(Vec3::new(
                    (i % 32) as f32 * 2.,
                    0.,
                    (i / 32) as f32 * 2.,
                )))
            })
            .collect();
        let mut index = BoxIndex::build(&bounds);
        bounds.swap(0, 1023);
        index.refit(&bounds);
        for i in 0..32 {
            let area = Bounds {
                lo: Vec3::new(i as f32, 0., 0.),
                hi: Vec3::new(i as f32 + 2., 2., 3.),
            };
            let expected: Vec<_> = bounds
                .iter()
                .enumerate()
                .filter(|(_, b)| b.overlaps(area))
                .map(|(i, _)| i)
                .collect();
            let mut actual = Vec::new();
            index.query(area, |id| actual.push(id));
            actual.sort_unstable();
            assert_eq!(actual, expected);
        }
    }
    #[test]
    #[ignore = "500 moving character queries against 3,000 boxes; CPU benchmark"]
    fn profile_shared_collision_queries() {
        let bounds: Vec<_> = (0..3000)
            .map(|i| {
                Bounds::for_box(Mat4::from_scale_rotation_translation(
                    Vec3::new(1.8, 0.6, 4.6),
                    glam::Quat::IDENTITY,
                    Vec3::new((i % 60) as f32 * 2.5, 0., (i / 60) as f32 * 4.0),
                ))
            })
            .collect();
        let index = BoxIndex::build(&bounds);
        let mut times = [Vec::new(), Vec::new()];
        let mut count = 0;
        for frame in 0..100 {
            let mut counts = [0, 0];
            for i in [frame % 2, 1 - frame % 2] {
                let now = std::time::Instant::now();
                for actor in 0..500 {
                    let p = Vec3::new(
                        (actor % 30) as f32 * 4.95 + frame as f32 * 0.013,
                        0.,
                        (actor / 30) as f32 * 10.1,
                    );
                    let area = Bounds {
                        lo: p - Vec3::ONE,
                        hi: p + Vec3::ONE,
                    };
                    if i == 0 {
                        counts[i] += bounds.iter().filter(|b| b.overlaps(area)).count();
                    } else {
                        index.query(area, |_| counts[i] += 1);
                    }
                }
                if frame >= 4 {
                    times[i].push(now.elapsed().as_secs_f64() * 1e3);
                }
            }
            assert_eq!(counts[0], counts[1]);
            count += counts[1];
        }
        for (i, t) in times.iter_mut().enumerate() {
            t.sort_by(f64::total_cmp);
            eprintln!(
                "500 queries, method {i}, query P50/P95 {:.4}/{:.4} ms",
                t[47], t[91]
            );
        }
        eprintln!(
            "candidates per character {:.2}, exhaustive 3000",
            count as f64 / 50000.
        );
    }
}
