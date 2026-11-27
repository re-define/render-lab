//! Skin-local forward kinematics for joints sharing one independently moving ancestor.
//! Matrix multiplication follows hierarchy order without inverting the root (zero scale is valid).
use super::*;
use std::collections::HashMap;

struct Node {
    row: usize,
    parent: Option<usize>,
    end: usize,
    relative: Mat4,
    joints: std::ops::Range<usize>,
}

pub(super) struct PoseSpace {
    pub root: usize,
    pub root_version: u64,
    pub pending: Vec<usize>,
    nodes: Vec<Node>,
    joints: Vec<usize>,
}

impl PoseSpace {
    pub fn bind(world: &World, cache: &TransformCache, slots: &[usize]) -> Option<Self> {
        let mut anchor = *slots.first()?;
        while !world.entities[&cache.rows[anchor].id]
            .components
            .contains_key(MOTION_POLICY)
        {
            anchor = cache.rows[anchor].parent_slot?;
        }
        let interval = anchor..cache.rows[anchor].end;
        if !slots.iter().all(|slot| interval.contains(slot)) {
            return None;
        }
        let mut used = BTreeMap::<usize, Vec<usize>>::new();
        for (joint, &slot) in slots.iter().enumerate() {
            if slot == anchor {
                continue;
            }
            used.entry(slot).or_default().push(joint);
            let mut at = cache.rows[slot].parent_slot?;
            while at != anchor {
                if used.contains_key(&at) {
                    break;
                }
                used.insert(at, Vec::new());
                at = cache.rows[at].parent_slot?;
            }
        }
        // A skin spanning independently movable actors has no common character space.
        if used.keys().any(|&row| {
            world.entities[&cache.rows[row].id]
                .components
                .contains_key(MOTION_POLICY)
        }) {
            return None;
        }
        let mut lookup = HashMap::with_capacity(used.len());
        let mut nodes: Vec<Node> = Vec::with_capacity(used.len());
        let mut joints = Vec::with_capacity(slots.len());
        for (row, outputs) in used {
            let transform = &cache.rows[row];
            let parent = transform.parent_slot.and_then(|p| lookup.get(&p).copied());
            let relative = parent.map_or(transform.local, |i: usize| {
                nodes[i].relative * transform.local
            });
            let i = nodes.len();
            lookup.insert(row, i);
            let start = joints.len();
            joints.extend(outputs);
            nodes.push(Node {
                row,
                parent,
                end: i + 1,
                relative,
                joints: start..joints.len(),
            });
        }
        for i in (0..nodes.len()).rev() {
            if let Some(parent) = nodes[i].parent {
                nodes[parent].end = nodes[parent].end.max(nodes[i].end);
            }
        }
        Some(Self {
            root: anchor,
            root_version: cache.rows[anchor].version,
            nodes,
            joints,
            pending: Vec::new(),
        })
    }

    pub fn initial(&self, inverse: &[Mat4]) -> Arc<[Mat4]> {
        // Joints on the anchor itself retain their inverse-bind matrix in this space.
        let mut result: Arc<[Mat4]> = inverse.into();
        let matrices = Arc::get_mut(&mut result).unwrap();
        for n in &self.nodes {
            for &j in &self.joints[n.joints.clone()] {
                matrices[j] = n.relative * inverse[j];
            }
        }
        result
    }

    pub fn rows(&self) -> impl Iterator<Item = usize> + '_ {
        self.nodes.iter().map(|n| n.row)
    }

    pub fn update(
        &mut self,
        cache: &TransformCache,
        dirty: &[usize],
        inverse: &[Mat4],
        matrices: &mut Arc<[Mat4]>,
    ) -> usize {
        if dirty.is_empty() {
            return 0;
        }
        let mut writable = None;
        let mut destination = Some(matrices);
        let mut end = 0;
        let mut inspected = 0;
        // Local edit slots and their pose node indices are both in transform preorder.
        for &start in dirty {
            if start < end {
                continue;
            }
            end = self.nodes[start].end;
            for i in start..end {
                let n = &self.nodes[i];
                let local = cache.rows[n.row].local;
                let next = n.parent.map_or(local, |p| self.nodes[p].relative * local);
                if next == n.relative {
                    continue;
                }
                for &j in &self.joints[n.joints.clone()] {
                    inspected += 1;
                    let output =
                        writable.get_or_insert_with(|| Arc::make_mut(destination.take().unwrap()));
                    output[j] = next * inverse[j];
                }
                self.nodes[i].relative = next;
            }
        }
        inspected
    }
}

/// A round-robin cap on changed cosmetic actors per extracted snapshot. All skins of an actor advance together.
pub(super) struct VisualBudget {
    pub limit: std::num::NonZeroUsize,
    pub eligible: fn(&World, Id) -> bool,
    last: Option<Id>,
}
impl VisualBudget {
    pub fn defer(&mut self, mut candidates: BTreeSet<Id>) -> BTreeSet<Id> {
        let mut slots = self.limit.get().min(candidates.len());
        while slots > 0 {
            let id = self
                .last
                .and_then(|last| {
                    candidates
                        .range((std::ops::Bound::Excluded(last), std::ops::Bound::Unbounded))
                        .next()
                        .copied()
                })
                .unwrap_or_else(|| *candidates.first().unwrap());
            self.last = Some(id);
            candidates.remove(&id);
            slots -= 1;
        }
        candidates
    }
}
impl SceneExtractor {
    /// Declare the explicitly cosmetic actors eligible for reduced visual pose frequency.
    /// All unlisted roots, including precision contacts, use each new pose immediately.
    pub fn set_visual_pose_budget(
        &mut self,
        limit: Option<std::num::NonZeroUsize>,
        eligible: fn(&World, Id) -> bool,
    ) {
        self.pose_budget = limit.map(|limit| VisualBudget {
            limit,
            eligible,
            last: None,
        });
    }
    pub fn visual_pose_budget(&self) -> Option<usize> {
        self.pose_budget.as_ref().map(|v| v.limit.get())
    }
    pub fn held_visual_actors(&self) -> usize {
        self.previous
            .as_ref()
            .map_or(0, |s| s.stats.held_visual_actors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Quat, Vec3};
    #[test]
    fn posed_skin_tracks_root_wrist_full_pose_and_independent_roots() -> Result<()> {
        let mut w = World::demo();
        let mut part = w
            .entities
            .values()
            .find_map(|e| e.instance.clone())
            .unwrap();
        let mut mesh = w.entities[&part.mesh].mesh.as_deref().unwrap().clone();
        mesh.joints = vec![[0; 4]; mesh.positions.len()];
        mesh.weights = vec![[1., 0., 0., 0.]; mesh.positions.len()];
        part.mesh = w.spawn(Entity {
            mesh: Some(Arc::new(mesh)),
            ..Default::default()
        });
        let child = |w: &mut World, parent| {
            w.spawn(Entity {
                parent,
                ..Default::default()
            })
        };
        let actor = child(&mut w, None);
        w.set_motion_policy(actor, MotionPolicy::KINEMATIC)?;
        let hip = child(&mut w, Some(actor));
        let wrist = child(&mut w, Some(hip));
        let finger = child(&mut w, Some(wrist));
        let foot = child(&mut w, Some(hip));
        let joints = vec![actor, hip, wrist, finger, foot];
        let inverse = vec![Mat4::from_translation(Vec3::Z * 0.25); joints.len()];
        let skin = w.spawn(Entity {
            skin: Some(Skin {
                joints: joints.clone(),
                inverse_bind: inverse.clone(),
            }),
            ..Default::default()
        });
        let part_id = w.spawn(Entity {
            skin_binding: Some(skin),
            instance: Some(part),
            ..Default::default()
        });
        let mut r = SceneExtractor::default();
        let extract = |r: &mut SceneExtractor, w: &World| -> Result<_> {
            let s = r.snapshot(w, Camera::default(), [16, 16], Default::default())?;
            let i = s
                .scene
                .instances()
                .iter()
                .find(|i| i.id == part_id)
                .unwrap();
            let palette = i.palette.as_ref().unwrap();
            let transforms = w.transforms()?;
            for (j, m) in palette.iter().enumerate() {
                assert!(
                    (i.palette_root * *m).abs_diff_eq(transforms[&joints[j]] * inverse[j], 1e-5)
                );
            }
            Ok((s.scene.stats, palette.clone(), i.palette_root))
        };
        let (_, initial, _) = extract(&mut r, &w)?;
        let root = Mat4::from_scale_rotation_translation(
            Vec3::new(1., 0., 2.),
            Quat::from_rotation_z(0.4),
            Vec3::Y,
        );
        w.set_local(actor, root)?;
        let (s, shifted, m) = extract(&mut r, &w)?;
        assert_eq!(m, root);
        assert!(Arc::ptr_eq(&initial, &shifted));
        assert_eq!((s.palette_joints_examined, s.palettes_updated), (0, 0));
        w.set_local(wrist, Mat4::from_translation(Vec3::X))?;
        let (s, _, _) = extract(&mut r, &w)?;
        assert_eq!(s.palette_joints_examined, 2);
        w.set_local(hip, Mat4::from_rotation_x(0.6))?;
        let (s, p, _) = extract(&mut r, &w)?;
        assert_eq!(s.palette_joints_examined, 4);
        assert_eq!(p, extract(&mut SceneExtractor::default(), &w)?.1);
        for &joint in &joints {
            assert!(
                r.transforms
                    .world_matrix(r.transforms.slots[&joint])
                    .abs_diff_eq(w.transforms()?[&joint], 1e-5)
            );
        }
        let revision = r.transforms.world_version(r.transforms.slots[&finger]);
        w.set_local(finger, Mat4::from_rotation_z(0.3))?;
        extract(&mut r, &w)?;
        assert_ne!(
            r.transforms.world_version(r.transforms.slots[&finger]),
            revision
        );
        // The nested actor moves independently. The whole binding switches back to world space.
        w.set_motion_policy(wrist, MotionPolicy::KINEMATIC)?;
        let (_, _, m) = extract(&mut r, &w)?;
        assert_eq!(m, Mat4::IDENTITY);
        w.set_local(actor, Mat4::from_translation(Vec3::Z))?;
        let (s, _, _) = extract(&mut r, &w)?;
        assert_eq!(s.palette_joints_examined, joints.len());
        Ok(())
    }
}
