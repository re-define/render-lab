//! FK on the imported hierarchy and analytic, fixed-length limb IK.
use super::*;
use std::collections::BTreeMap;

// Most distal fully weighted index-finger vertex in the Silver Wolf asset,
// expressed in IndexFinger3_R_069 bind space (mesh "2", vertex 8702).
pub(super) const FINGERTIP: Vec3 = Vec3::new(-0.00573349, 0.01938251, 0.00494587);

#[cfg_attr(test, derive(Clone))]
pub(super) struct Rig {
    pub ids: Vec<Id>,
    parents: Vec<Option<usize>>,
    pub hip: usize,
    pub legs: [[usize; 4]; 2],
    pub arm: [usize; 3],
    pub finger: usize,
    pub corrected: Vec<usize>,
}

impl Rig {
    pub fn bind(w: &World, actor: Id, traversal: bool) -> Result<Self> {
        let mut ids = Vec::new();
        let mut parents = Vec::new();
        let mut indices = BTreeMap::new();
        let mut joint = |name: &str| -> Result<usize> {
            let id = w
                .entities
                .iter()
                .find_map(|(&id, e)| (e.name == name).then_some(id))
                .with_context(|| format!("missing contact joint {name}"))?;
            let mut chain = vec![id];
            while *chain.last().unwrap() != actor {
                let parent = w.entities[chain.last().unwrap()]
                    .parent
                    .context("contact joint outside actor")?;
                ensure!(!chain.contains(&parent), "cyclic contact hierarchy");
                chain.push(parent);
            }
            for id in chain.into_iter().rev() {
                if !indices.contains_key(&id) {
                    let parent = w.entities[&id]
                        .parent
                        .map(|p| {
                            indices
                                .get(&p)
                                .copied()
                                .context("actor must be a scene root")
                        })
                        .transpose()?;
                    parents.push(parent);
                    indices.insert(id, ids.len());
                    ids.push(id);
                }
            }
            Ok(indices[&id])
        };
        let hip = joint("Hips_04")?;
        let legs = [
            [
                joint("Right leg_0126")?,
                joint("Right knee_0127")?,
                joint("Right ankle_0128")?,
                joint("Right toe_0129")?,
            ],
            [
                joint("Left leg_0131")?,
                joint("Left knee_0132")?,
                joint("Left ankle_0133")?,
                joint("Left toe_0134")?,
            ],
        ];
        let arm = [
            joint("Right arm_059")?,
            joint("Right elbow_061")?,
            joint("Right wrist_063")?,
        ];
        let finger = joint("IndexFinger3_R_069")?;
        let mut corrected = [
            vec![hip],
            legs[0][..3].to_vec(),
            legs[1][..3].to_vec(),
            arm.to_vec(),
        ]
        .concat();
        if traversal {
            let state = player(w, actor)?;
            for id in &state.bones {
                let index = joint(&w.entities[id].name)?;
                if !corrected.contains(&index) {
                    corrected.push(index);
                }
            }
        }
        Ok(Self {
            ids,
            parents,
            hip,
            legs,
            arm,
            finger,
            corrected,
        })
    }
    pub fn pose(&self, cx: &SystemContext<'_>) -> Result<Pose<'_>> {
        let local = self
            .ids
            .iter()
            .map(|&id| cx.local(id)?.context("missing bound contact joint"))
            .collect::<Result<Vec<_>>>()?;
        let mut p = Pose {
            rig: self,
            local,
            world: vec![Mat4::IDENTITY; self.ids.len()],
        };
        p.forward();
        Ok(p)
    }
}

pub(super) struct Pose<'a> {
    rig: &'a Rig,
    pub local: Vec<Mat4>,
    pub world: Vec<Mat4>,
}
impl Pose<'_> {
    pub(super) fn forward(&mut self) {
        for (i, &parent) in self.rig.parents.iter().enumerate() {
            self.world[i] = parent.map_or(self.local[i], |p| self.world[p] * self.local[i]);
        }
    }
    fn parent(&self, i: usize) -> Mat4 {
        self.rig.parents[i].map_or(Mat4::IDENTITY, |p| self.world[p])
    }
    pub fn position(&self, i: usize) -> Vec3 {
        self.world[i].w_axis.truncate()
    }
    pub fn rotation(&self, i: usize) -> Quat {
        self.world[i].to_scale_rotation_translation().1.normalize()
    }
    pub fn translate_world(&mut self, i: usize, delta: Vec3) {
        let offset = self.parent(i).inverse().transform_vector3(delta);
        self.local[i].w_axis += offset.extend(0.);
        self.forward();
    }
    fn rotate_world(&mut self, i: usize, q: Quat) {
        let (scale, _, offset) = self.local[i].to_scale_rotation_translation();
        let parent = self.parent(i).to_scale_rotation_translation().1.normalize();
        self.local[i] = Mat4::from_scale_rotation_translation(
            scale,
            (parent.conjugate() * q).normalize(),
            offset,
        );
        self.forward();
    }
    fn aim(&mut self, i: usize, child: usize, goal: Vec3) {
        let from = (self.position(child) - self.position(i)).normalize_or_zero();
        let to = (goal - self.position(i)).normalize_or_zero();
        if from != Vec3::ZERO && to != Vec3::ZERO {
            self.rotate_world(i, Quat::from_rotation_arc(from, to) * self.rotation(i));
        }
    }
    // The pole is a world-space direction. Preserve twist by swinging each
    // animated segment, then restore the desired independent effector rotation.
    pub fn solve(&mut self, [a, b, c]: [usize; 3], target: Vec3, pole: Vec3, rotation: Quat) {
        let root = self.position(a);
        let joint = self.position(b);
        let end = self.position(c);
        let (l1, l2) = (root.distance(joint), joint.distance(end));
        if l1.min(l2) < 1e-5 {
            return;
        }
        let axis = (target - root)
            .try_normalize()
            .or_else(|| (end - root).try_normalize())
            .unwrap_or(Vec3::Z);
        let d = root
            .distance(target)
            .clamp((l1 - l2).abs() + 1e-6, l1 + l2 - 1e-6);
        let along = (l1 * l1 - l2 * l2 + d * d) / (2. * d);
        let bend = (pole - axis * pole.dot(axis))
            .try_normalize()
            .unwrap_or_else(|| axis.any_orthonormal_vector());
        let elbow = root + along * axis + (l1 * l1 - along * along).max(0.).sqrt() * bend;
        self.aim(a, b, elbow);
        self.aim(b, c, root + d * axis);
        self.rotate_world(c, rotation);
    }
}
