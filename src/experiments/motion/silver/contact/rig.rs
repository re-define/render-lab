//! FK on the imported hierarchy and analytic, fixed-length limb IK.
use super::*;
use std::collections::BTreeMap;

// Most distal fully weighted index-finger vertex in the Silver Wolf asset,
// expressed in IndexFinger3_R_069 bind space (mesh "2", vertex 8702).
#[cfg(test)]
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
    pub palm_offset: Vec3,
    pub palm_frame: glam::Mat3,
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
        let semantics = state.rig.as_deref().unwrap_or(&profile::DEFAULT.rig);
        semantics.validate(state.bones.len())?;
        let hip = joint(state.bones[semantics.pelvis])?;
        let mut legs = [[0; 4]; 2];
        for (chain, slots) in legs
            .iter_mut()
            .zip([semantics.right_leg, semantics.left_leg])
        {
            for (out, slot) in chain.iter_mut().zip(slots) {
                *out = joint(state.bones[slot])?;
            }
        }
        let mut arm = [0; 3];
        for (out, slot) in arm.iter_mut().zip(semantics.reach_arm) {
            *out = joint(state.bones[slot])?;
        }
        let wrist = state.bones[semantics.reach_arm[2]];
        let mut subtree = vec![wrist];
        let mut candidates = Vec::new();
        while let Some(id) = subtree.pop() {
            if marker.as_ref().map_or_else(
                || w.entities[&id].name == semantics.fingertip_name,
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
        let fingertip = marker.map_or(semantics.fingertip_offset, |m| m.offset);
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
        for chain in [&legs[0][..], &legs[1][..], &arm[..]] {
            ensure!(
                chain
                    .windows(2)
                    .all(|pair| descendants[pair[0]].contains(&pair[1])),
                "semantic limb joints must follow the skeleton hierarchy"
            );
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
            palm_offset: semantics.palm.offset,
            palm_frame: semantics.palm.frame()?,
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
    pub(super) fn aim(&mut self, i: usize, child: usize, goal: Vec3) {
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
    fn changed_wrist_bind_axes_preserve_sampled_orientation_and_reach_on_alternate_rig()
    -> Result<()> {
        let source = scene()?;
        let actor = actor(&source).unwrap();
        let mut alternative = source.clone();
        let (spacer, marker) = population::alternate_rig(&mut alternative, actor)?;
        let changed = Rig::bind(&alternative, actor, &children(&alternative))?;
        let original = Rig::bind(&source, actor, &children(&source))?;
        let wrist = changed.ids[changed.arm[2]];
        assert_eq!(changed.ids[changed.finger], marker.joint);
        let bank = SilverMatcher::load()?;
        let mut a = player(&source, actor)?.into_owned();
        let mut b = player(&alternative, actor)?.into_owned();
        let rotation = alternative.entities[&spacer]
            .local
            .to_scale_rotation_translation()
            .1;
        assert!(!rotation.abs_diff_eq(Quat::IDENTITY, 1e-3));
        let slot = a.bones.iter().position(|id| *id == wrist).unwrap();
        let sample = bank
            .clips
            .iter()
            .find(|(_, name)| name == "walk_41")
            .unwrap()
            .0
            .start
            + 19;
        a.sample = sample;
        b.sample = sample;
        let (_, left) = bank.sample_pose(&mut a, Mat4::IDENTITY, 0., |_, id| {
            Ok(source.entities[&id].local)
        })?;
        let (_, right) = bank.sample_pose(&mut b, Mat4::IDENTITY, 0., |_, id| {
            Ok(alternative.entities[&id].local)
        })?;
        let expected = left[slot].to_scale_rotation_translation().1;
        let corrected = right[slot].to_scale_rotation_translation().1;
        assert!((rotation * corrected).dot(expected).abs() > 0.99999);
        assert!(corrected.dot(expected).abs() < 0.99);
        assert_eq!(original.ids.len() + 1, changed.ids.len());
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        let mut schedule = Schedule::default();
        install(&alternative, &mut schedule)?;
        let mut world = Arc::new(alternative);
        for _ in 0..720 {
            schedule.tick(&mut world, 1. / 60., &pool)?;
        }
        let c = world.typed_component::<Contact>(actor, SETTING)?.unwrap();
        let transforms = world.transforms()?;
        let tip = transforms[&marker.joint].transform_point3(marker.offset);
        let target = target_point(transforms[&c.target]);
        let miss = tip.distance(target);
        eprintln!("alternate bind-space fingertip distance: {miss:.7} m");
        assert!(miss < 0.001 && c.reach > 0.9);
        Ok(())
    }

    #[test]
    fn binding_rejects_disconnected_semantic_limb_chains() -> Result<()> {
        let mut w = scene()?;
        let actor = actor(&w).unwrap();
        let mut player = player(&w, actor)?.into_owned();
        let mut map = profile::DEFAULT.rig.clone();
        std::mem::swap(&mut map.right_leg[2], &mut map.left_leg[2]);
        player.rig = Some(Arc::new(map));
        w.set_typed_component(actor, PLAYER, player);
        assert!(Rig::bind(&w, actor, &children(&w)).is_err());
        Ok(())
    }

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
