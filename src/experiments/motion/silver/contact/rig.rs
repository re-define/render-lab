//! FK on the imported hierarchy and analytic, fixed-length limb IK.
use super::*;
use std::collections::BTreeMap;

// Most distal fully weighted index-finger vertex in the Silver Wolf asset,
// expressed in IndexFinger3_R_069 bind space (mesh "2", vertex 8702).
pub(super) const FINGERTIP: Vec3 = Vec3::new(-0.00573349, 0.01938251, 0.00494587);

pub(super) struct Rig {
    pub ids: Vec<Id>,
    parents: Vec<Option<usize>>,
    descendants: Vec<Vec<usize>>,
    pub hip: usize,
    pub legs: [[usize; 4]; 2],
    pub arm: [usize; 3],
    pub finger: usize,
    pub corrected: Vec<usize>,
    pub published_joints: crate::scene::PoseLayout,
    /// Motion bank slot -> index in the saved base pose.
    pub source_slots: Vec<usize>,
    source_for_joint: Vec<Option<usize>>,
    pub fingertip: Vec3,
}

/// Build once when installing a set of characters. Bone names are resolved below the actor's wrist.
pub(super) fn children(w: &World) -> BTreeMap<Id, Vec<Id>> {
    let mut out = BTreeMap::<Id, Vec<Id>>::new();
    for (&id, e) in &w.entities {
        if let Some(parent) = e.parent {
            out.entry(parent).or_default().push(id);
        }
    }
    out
}

impl Rig {
    pub fn bind(w: &World, actor: Id, children: &BTreeMap<Id, Vec<Id>>) -> Result<Self> {
        let state = player(w, actor)?;
        ensure!(
            state.bones.len() == 21,
            "unsupported locomotion bank layout"
        );
        let marker = w
            .typed_component::<Contact>(actor, SETTING)?
            .and_then(|c| c.finger.clone());
        let mut ids = Vec::new();
        let mut parents = Vec::new();
        let mut indices = BTreeMap::new();
        let mut joint = |id: Id| -> Result<usize> {
            let mut chain = vec![id];
            while *chain.last().unwrap() != actor {
                let parent = w
                    .entities
                    .get(chain.last().unwrap())
                    .context("missing motion bone")?
                    .parent
                    .context("contact joint outside actor")?;
                ensure!(!chain.contains(&parent), "cyclic contact hierarchy");
                chain.push(parent);
            }
            for id in chain.into_iter().rev() {
                if !indices.contains_key(&id) {
                    let parent = if id == actor {
                        None
                    } else {
                        let p = w.entities[&id].parent.context("missing rig parent")?;
                        Some(*indices.get(&p).context("unbound rig parent")?)
                    };
                    parents.push(parent);
                    indices.insert(id, ids.len());
                    ids.push(id);
                }
            }
            Ok(indices[&id])
        };
        // Semantic slots belong to the retargeted motion bank. Entity names may differ per rig.
        let hip = joint(state.bones[0])?;
        let legs = [
            [
                joint(state.bones[13])?,
                joint(state.bones[14])?,
                joint(state.bones[15])?,
                joint(state.bones[16])?,
            ],
            [
                joint(state.bones[17])?,
                joint(state.bones[18])?,
                joint(state.bones[19])?,
                joint(state.bones[20])?,
            ],
        ];
        let arm = [
            joint(state.bones[6])?,
            joint(state.bones[7])?,
            joint(state.bones[8])?,
        ];
        let wrist = state.bones[8];
        let mut subtree = vec![wrist];
        let mut candidates = Vec::new();
        while let Some(id) = subtree.pop() {
            if marker.as_ref().map_or_else(
                || w.entities[&id].name == "IndexFinger3_R_069",
                |m| id == m.joint,
            ) {
                candidates.push(id);
            }
            if let Some(next) = children.get(&id) {
                subtree.extend_from_slice(next);
            }
        }
        ensure!(
            candidates.len() == 1,
            "expected one fingertip below actor {actor}'s wrist"
        );
        let finger = joint(candidates[0])?;
        let fingertip = marker.map_or(FINGERTIP, |m| m.offset);
        ensure!(fingertip.is_finite(), "invalid fingertip offset");
        // The prefix preserves source-pose order in older contact saves.
        let mut corrected = [
            vec![hip],
            legs[0][..3].to_vec(),
            legs[1][..3].to_vec(),
            arm.to_vec(),
        ]
        .concat();
        let mut source_slots = Vec::with_capacity(state.bones.len());
        for &id in &state.bones {
            let index = joint(id)?;
            let slot = corrected
                .iter()
                .position(|&i| i == index)
                .unwrap_or_else(|| {
                    corrected.push(index);
                    corrected.len() - 1
                });
            source_slots.push(slot);
        }
        ensure!(
            corrected.len() == state.bones.len(),
            "duplicate motion bone binding"
        );
        let mut source_for_joint = vec![None; ids.len()];
        for (slot, &index) in corrected.iter().enumerate() {
            source_for_joint[index] = Some(slot);
        }
        // Parent-before-child order also holds within each joint's affected subtree.
        let mut descendants = vec![Vec::new(); ids.len()];
        for i in 0..ids.len() {
            let mut parent = Some(i);
            while let Some(p) = parent {
                descendants[p].push(i);
                parent = parents[p];
            }
        }
        Ok(Self {
            published_joints: crate::scene::PoseLayout::new(corrected.iter().map(|&i| ids[i]))?,
            ids,
            parents,
            descendants,
            hip,
            legs,
            arm,
            finger,
            corrected,
            source_slots,
            source_for_joint,
            fingertip,
        })
    }
    pub fn pose(&self, cx: &SystemContext<'_>, source: &[Mat4]) -> Result<Pose<'_>> {
        let local = self
            .ids
            .iter()
            .enumerate()
            .map(|(i, &id)| match self.source_for_joint[i] {
                Some(slot) => source
                    .get(slot)
                    .copied()
                    .context("missing sampled base pose"),
                None => cx.local(id)?.context("missing bound contact joint"),
            })
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
    fn refresh_subtree(&mut self, joint: usize) {
        for &i in &self.rig.descendants[joint] {
            self.world[i] =
                self.rig.parents[i].map_or(self.local[i], |p| self.world[p] * self.local[i]);
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
        self.refresh_subtree(i);
    }
    fn rotate_world(&mut self, i: usize, q: Quat) {
        let (scale, _, offset) = self.local[i].to_scale_rotation_translation();
        let parent = self.parent(i).to_scale_rotation_translation().1.normalize();
        self.local[i] = Mat4::from_scale_rotation_translation(
            scale,
            (parent.conjugate() * q).normalize(),
            offset,
        );
        self.refresh_subtree(i);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limb_updates_match_full_forward_kinematics() -> Result<()> {
        let world = scene()?;
        let rig = Rig::bind(&world, actor(&world).unwrap(), &children(&world))?;
        let mut pose = Pose {
            rig: &rig,
            local: rig.ids.iter().map(|id| world.entities[id].local).collect(),
            world: vec![Mat4::IDENTITY; rig.ids.len()],
        };
        pose.forward();
        let mut updated = 0;
        for chain in [
            [rig.legs[0][0], rig.legs[0][1], rig.legs[0][2]],
            [rig.legs[1][0], rig.legs[1][1], rig.legs[1][2]],
            rig.arm,
        ] {
            let tip = chain[2];
            pose.solve(
                chain,
                pose.position(tip) + Vec3::new(0.01, 0.02, 0.04),
                Vec3::Z,
                pose.rotation(tip),
            );
            let partial = pose.world.clone();
            pose.forward();
            assert_eq!(partial, pose.world);
            updated += chain
                .iter()
                .map(|&i| rig.descendants[i].len())
                .sum::<usize>();
        }
        eprintln!(
            "Both legs and one arm FK matrix updates: {updated} subtree vs {} full",
            9 * rig.ids.len()
        );
        assert!(updated < 9 * rig.ids.len() / 2);
        Ok(())
    }
}
