//! Rest-pose tree prototypes. Every shoot starts on a woody branch and inherits its wind joint.
use super::{Anchor, Mesh, PlantBone, Random, TAU, Vec3, bone, empty, tube};

mod shoots;
use shoots::{broad_leaves, feather, needles};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::experiments::vegetation) enum Species {
    Spruce,
    Pine,
    Birch,
    Oak,
}

impl Species {
    pub(in crate::experiments::vegetation) fn broadleaf(self) -> bool {
        matches!(self, Self::Birch | Self::Oak)
    }
}

#[derive(Clone, Copy)]
struct Branch {
    root: Vec3,
    elbow: Vec3,
    tip: Vec3,
    joint: u16,
}
impl Branch {
    fn point(self, t: f32) -> Vec3 {
        if t < 0.52 {
            self.root.lerp(self.elbow, t / 0.52)
        } else {
            self.elbow.lerp(self.tip, (t - 0.52) / 0.48)
        }
    }

    fn grow(self, wood: &mut Mesh, root_joint: u16, radius: f32, sides: u32) {
        let a = Anchor {
            point: self.root,
            joint: root_joint,
        };
        let b = Anchor {
            point: self.elbow,
            joint: self.joint,
        };
        let c = Anchor {
            point: self.tip,
            joint: self.joint,
        };
        tube(wood, a, b, [radius, radius * 0.52], sides);
        tube(wood, b, c, [radius * 0.52, radius * 0.10], sides);
    }
}

struct Tree {
    wood: Mesh,
    leaves: Mesh,
    bones: Vec<PlantBone>,
    rng: Random,
    species: Species,
}
impl Tree {
    fn limb(&mut self, parent: Anchor, dir: Vec3, reach: f32, t: f32) {
        let rng = &mut self.rng;
        let species = self.species;
        let side = dir.cross(Vec3::Y);
        let (rise, bend, width) = match species {
            Species::Spruce => (-0.12 + 0.35 * t, -0.16, 0.041),
            Species::Pine => (0.44 + 0.16 * t, -0.13, 0.053),
            Species::Birch => (0.79, 0.23, 0.051),
            Species::Oak => (0.46 + 0.60 * rng.next(), 0.24 * rng.signed(), 0.078),
        };
        let root = parent.point;
        let elbow = root
            + (dir * 0.52 + Vec3::Y * (rise * 0.48 + bend) + side * rng.signed() * 0.13) * reach;
        let tip = root
            + (dir + Vec3::Y * (rise + rng.signed() * 0.18) + side * rng.signed() * 0.29) * reach;
        let joint = bone(
            &mut self.bones,
            Some(parent.joint),
            root,
            if species.broadleaf() { 0.042 } else { 0.052 },
        );
        let branch = Branch {
            root,
            elbow,
            tip,
            joint,
        };
        branch.grow(&mut self.wood, parent.joint, 0.013 + reach * width, 6);
        let count = match species {
            Species::Spruce => 8,
            Species::Pine => 7,
            _ => 7,
        };
        for k in 0..count {
            let u = 0.12 + 0.83 * (k as f32 + rng.next() * 0.55) / count as f32;
            let sign = if k % 2 == 0 { -1. } else { 1. };
            let base = branch.point(u);
            let h = (dir * (0.28 + rng.next() * 0.62) + side * sign * (0.71 + rng.next() * 0.46))
                .normalize();
            let len = (0.22 + reach * (0.46 + 0.13 * rng.next())) * (1.16 - u * 0.53);
            let slope = match species {
                Species::Spruce => -0.32,
                Species::Pine => 0.37,
                Species::Birch => -0.46,
                Species::Oak => 0.33,
            } + rng.signed() * 0.35;
            let fork = Branch {
                root: base,
                elbow: base + (h * 0.52 + Vec3::Y * (slope * 0.32 + 0.13)) * len,
                tip: base + (h + Vec3::Y * slope) * len,
                joint,
            };
            fork.grow(&mut self.wood, joint, 0.009 + len * 0.017, 3);
            if species.broadleaf() {
                // Alternate secondary twigs turn each fork into a contoured crown, with gaps between limbs.
                for n in 0..3 {
                    let f = 0.20 + n as f32 * 0.36;
                    let lateral = h.cross(Vec3::Y) * if n % 2 == 0 { -1. } else { 1. };
                    let start = fork.point(f);
                    let heading = (h * 0.69 + lateral * (0.45 + rng.next() * 0.38)).normalize();
                    let size = (0.27 + 0.43 * len) * (1.13 - 0.34 * f);
                    let end = start + (heading + Vec3::Y * (slope - 0.12)) * size;
                    let twig = Branch {
                        root: start,
                        elbow: start.lerp(end, 0.52),
                        tip: end,
                        joint,
                    };
                    tube(
                        &mut self.wood,
                        Anchor {
                            point: start,
                            joint,
                        },
                        Anchor { point: end, joint },
                        [0.012, 0.003],
                        3,
                    );
                    broad_leaves(&mut self.leaves, twig, heading, species, rng);
                }
            } else {
                needles(&mut self.leaves, fork, h, species, rng);
            }
        }
    }
}

pub(in crate::experiments::vegetation) fn tree(
    seed: u32,
    species: Species,
) -> (Mesh, Mesh, Vec<PlantBone>) {
    let mut tree = Tree {
        wood: empty(),
        leaves: empty(),
        bones: vec![],
        rng: Random::new(seed),
        species,
    };
    let height = match species {
        Species::Spruce => 7.75,
        Species::Pine => 7.15,
        Species::Birch => 7.10,
        Species::Oak => 5.90,
    } + tree.rng.next() * 0.68;
    let lean = Vec3::new(tree.rng.signed(), 0., tree.rng.signed())
        * if species.broadleaf() { 0.82 } else { 0.32 };
    let girth = match species {
        Species::Oak => 0.32,
        Species::Birch => 0.145,
        _ => 0.23,
    };
    let radius = |t: f32| 0.012 + girth * (1. - t).powf(1.10) + 0.11 * (-31. * t).exp();
    let center = |t: f32| Vec3::Y * (height * t - 0.10) + lean * t * t;
    let mut prev = Anchor {
        point: center(0.),
        joint: bone(&mut tree.bones, None, Vec3::ZERO, 0.),
    };
    let levels = if species == Species::Spruce { 12 } else { 9 };
    for i in 1..=levels {
        let t = i as f32 / levels as f32;
        let last = (i - 1) as f32 / levels as f32;
        let next = Anchor {
            point: center(t),
            joint: bone(
                &mut tree.bones,
                Some(prev.joint),
                center(t),
                0.007 + t * 0.006,
            ),
        };
        tube(&mut tree.wood, prev, next, [radius(last), radius(t)], 10);
        let (start, count, spread) = match species {
            Species::Spruce => (0.14, 5 - i % 2, 2.87 * (1. - t * 0.96).powf(0.74)),
            Species::Pine => (0.39, 4, 2.45 * (1. - ((t - 0.63) * 2.10).powi(2)).max(0.17)),
            Species::Birch => (0.23, 3, 1.82 * (1. - ((t - 0.51) * 1.91).powi(2)).max(0.18)),
            Species::Oak => (0.18, 3, 2.65 * (1. - ((t - 0.49) * 1.66).powi(2)).max(0.27)),
        };
        if t > start {
            let phase = tree.rng.next() * TAU;
            for n in 0..count {
                let angle = phase + TAU * n as f32 / count as f32 + tree.rng.signed() * 0.28;
                let dir = Vec3::new(angle.cos(), 0., angle.sin());
                let u = t - (0.10 + 0.80 * tree.rng.next()) / levels as f32;
                let root = Anchor {
                    point: center(u),
                    joint: prev.joint,
                };
                let reach = spread * (0.76 + 0.47 * tree.rng.next());
                tree.limb(root, dir, reach, u);
            }
        }
        prev = next;
    }
    if species == Species::Spruce {
        // New leader growth fills the last internode and ends in an upright bud.
        for k in 0..35 {
            let t = k as f32 / 35.;
            let a = k as f32 * 2.40;
            let outward = Vec3::new(a.cos(), 0., a.sin());
            let len = 0.12 + 0.28 * (1. - t);
            feather(
                &mut tree.leaves,
                center(0.86 + t * 0.14),
                (outward + Vec3::Y * (0.26 + t)) * len,
                len * 0.21,
                tree.rng.signed() * 0.65,
                prev.joint,
                0.76,
            );
        }
    }
    if species == Species::Birch {
        for uv in &mut tree.wood.uv {
            uv[0] += 2.;
        }
    }
    super::exposure::bake(&mut tree.leaves);
    (tree.wood, tree.leaves, tree.bones)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tree_prototypes_have_distinct_crowns_and_bounded_skin_data() {
        let mut lowest = vec![];
        for (i, species) in [Species::Spruce, Species::Pine, Species::Birch, Species::Oak]
            .into_iter()
            .enumerate()
        {
            let (wood, leaves, bones) = tree(0x71b9 + i as u32 * 157, species);
            assert!(bones.len() <= 64);
            for mesh in [&wood, &leaves] {
                assert_eq!(mesh.positions.len(), mesh.normals.len());
                assert!(
                    mesh.normals
                        .iter()
                        .all(|&n| (Vec3::from(n).length_squared() - 1.).abs() < 1e-4)
                );
                assert!(mesh.indices.chunks_exact(3).all(|f| {
                    let [a, b, c] = [0, 1, 2].map(|k| Vec3::from(mesh.positions[f[k] as usize]));
                    (b - a).cross(c - a).length_squared() > 1.0e-16
                }));
                assert!(
                    mesh.joints
                        .iter()
                        .flatten()
                        .all(|j| usize::from(*j) < bones.len())
                );
            }
            let range = leaves
                .uv
                .iter()
                .map(|uv| uv[0])
                .fold((3.0_f32, 2.0_f32), |(lo, hi), x| (lo.min(x), hi.max(x)));
            assert!(
                range.0 >= 2. && range.1 <= 3. && range.1 - range.0 > 0.25,
                "sky exposure range {range:?}"
            );
            let tris = (wood.indices.len() + leaves.indices.len()) / 3;
            assert!(tris < 70_000, "prototype has {tris} triangles");
            lowest.push(
                leaves
                    .positions
                    .iter()
                    .map(|p| p[1])
                    .fold(f32::MAX, f32::min),
            );
        }
        assert!(
            lowest[1] - lowest[0] > 1.5,
            "pine and spruce have the same crown base"
        );
    }
}
