use super::*;

/// Reverse edges needed to propagate a change without walking all scene entities.
#[derive(Default)]
pub(super) struct Dependencies {
    pub light_count: usize,
    pub pose_nodes: std::collections::HashMap<usize, transforms::SkinUsers>,
    pub skins: Vec<SkinDependency>,
    pub components: BTreeMap<&'static str, ComponentOwner>,
}

/// Rebuilt together with the transform topology and the ordered instance table.
pub(super) struct SkinDependency {
    pub id: Id,
    pub joint_count: usize,
    pub instances: Vec<usize>,
}

impl RenderScene {
    pub(super) fn instance_index(&self, id: Id) -> Option<usize> {
        self.instances
            .binary_search_by_key(&id, |instance| instance.id)
            .ok()
    }

    fn same_bindings(&self, id: Id, entity: &Entity) -> bool {
        if self.materials.contains_key(&MaterialId(id)) != entity.material.is_some() {
            return false;
        }
        match (
            self.instance_index(id),
            entity.instance.as_ref().filter(|_| {
                !entity
                    .components
                    .contains_key(crate::experiments::composition::HIDDEN)
            }),
        ) {
            (Some(index), Some(instance)) => {
                let old = &self.instances[index];
                old.geometry_assets == instance.geometry_assets
                    && old.mesh == MeshId(instance.mesh)
                    && old.material == instance.material.map(MaterialId)
                    && old.outline == instance.outline
            }
            (None, None) => true,
            _ => false,
        }
    }
}

/// Subtree visits are already disjoint contiguous intervals. Counting their union
/// with the sparse entity edits avoids hashing every descendant of a moving actor.
#[derive(Default)]
struct Visits {
    rows: std::collections::HashSet<Id>,
    subtrees: Vec<std::ops::Range<usize>>,
}
impl Visits {
    fn insert(&mut self, id: Id) {
        self.rows.insert(id);
    }
    fn extend<'a>(&mut self, ids: impl IntoIterator<Item = &'a Id>) {
        self.rows.extend(ids);
    }
    fn count(&self, transforms: &TransformCache) -> usize {
        let subtree_count: usize = self.subtrees.iter().map(|r| transforms.live_count(r)).sum();
        subtree_count
            + self
                .rows
                .iter()
                .filter(|id| {
                    let Some(&slot) = transforms.slots.get(id) else {
                        return true;
                    };
                    let i = self.subtrees.partition_point(|r| r.end <= slot);
                    self.subtrees.get(i).is_none_or(|r| slot < r.start)
                })
                .count()
    }
}
/// World edits with transform-dependent render outputs.
#[derive(Default)]
struct TransformEdits {
    locals: Vec<(usize, u64, Mat4)>,
    lights: BTreeSet<Id>,
    rebound: Vec<usize>,
}

#[derive(Default)]
struct DirtyJoints {
    count: usize,
    indices: Vec<usize>,
}

impl SceneExtractor {
    pub(super) fn incremental<'a>(
        &mut self,
        world: &World,
        edits: impl Iterator<Item = (Id, EntityWrite)>,
        poses: impl Iterator<Item = &'a PublishedPose>,
    ) -> Result<Option<Arc<RenderScene>>> {
        let previous = self.previous.as_ref().expect("cursor requires a snapshot");
        let edits: BTreeSet<_> = edits.collect();
        let mut poses = poses.peekable();
        if !self.pending_visual
            && edits.is_empty()
            && poses.peek().is_none()
            && previous.stats == ExtractionStats::default()
        {
            return Ok(Some(previous.clone()));
        }
        let mut scene = previous.as_ref().clone();
        scene.stats = ExtractionStats::default();
        let mut visited = Visits::default();
        let structural: BTreeSet<_> = edits
            .iter()
            .filter_map(|&(id, k)| (k == EntityWrite::Structure).then_some(id))
            .collect();
        if !structural.is_empty() && !self.apply_leaf_edits(world, &structural, &mut scene)? {
            return Ok(None);
        }
        visited.extend(&structural);
        let mut updates = TransformEdits::default();
        for pose in poses {
            scene.stats.pose_blocks_read += 1;
            for (id, local) in pose.changes() {
                scene.stats.pose_locals_read += 1;
                if structural.contains(&id) {
                    continue;
                }
                updates
                    .locals
                    .push((self.transforms.slots[&id], pose.revision, local));
            }
        }
        let mut dirty_components = BTreeSet::new();
        for (id, kind) in edits {
            if structural.contains(&id) {
                continue;
            } // Leaf edits read the complete final entity.
            let entity = &world.entities[&id];
            visited.insert(id);
            match kind {
                EntityWrite::Structure => unreachable!(),
                EntityWrite::Inspect => {
                    if !Entities::inspectable(entity)
                        || entity.parent != self.transforms.get(id).unwrap().parent
                        || !scene.same_bindings(id, entity)
                    {
                        return Ok(None);
                    }
                }
                EntityWrite::Component(key)
                | EntityWrite::TypedMembership(key)
                | EntityWrite::TypedValue(key) => {
                    for (&dependent_key, owner) in &self.dependencies.components {
                        if owner.watched_settings.contains_key(key) {
                            dirty_components.insert(dependent_key);
                        }
                    }
                    let Some(spec) = self.registry.get(key) else {
                        // Existing typed state preserves entity membership and raw render settings.
                        // A new typed state on a non-render row only affects explicitly watched settings.
                        // MotionPolicy membership also establishes character-space skin anchors.
                        if matches!(kind, EntityWrite::TypedValue(_))
                            || matches!(kind, EntityWrite::TypedMembership(_))
                                && entity.instance.is_none()
                                && key != crate::scene::MOTION_POLICY
                        {
                            continue;
                        }
                        // Raw settings can also affect geometry routes or visibility.
                        return Ok(None);
                    };
                    match self.dependencies.components.get(spec.key) {
                        Some(owner) if owner.id == id => {
                            dirty_components.insert(spec.key);
                        }
                        // A newly introduced owner with a smaller ID takes precedence.
                        Some(owner) if key != spec.key || id > owner.id => (),
                        None if key != spec.key => (),
                        _ => return Ok(None),
                    }
                }
                EntityWrite::GeometryAssets => {
                    let products = entity
                        .geometry_assets
                        .as_ref()
                        .context("missing geometry assets")?;
                    Arc::make_mut(&mut scene.geometry_assets).insert(id, products.clone());
                    scene.geometry_revision = next_geometry_revision();
                }
                EntityWrite::GeometryBinding => {
                    if let Some(index) = scene.instance_index(id) {
                        let row = Arc::make_mut(&mut scene.instances).edit(index);
                        Arc::make_mut(row).geometry_assets = entity
                            .instance
                            .as_ref()
                            .context("missing render instance")?
                            .geometry_assets;
                        scene.geometry_revision = next_geometry_revision();
                        updates.rebound.push(index);
                    }
                }
                EntityWrite::Light => {
                    updates.lights.insert(id);
                }
                EntityWrite::Texture => {
                    Arc::make_mut(&mut scene.textures)
                        .insert(id, entity.texture.clone().context("missing texture asset")?);
                }
                EntityWrite::Local | EntityWrite::Material => (),
            }
            if matches!(kind, EntityWrite::Inspect | EntityWrite::Local) {
                // A direct edit can overlap a pose or undo it. The entity row
                // contains the final value after every writer, including rollback.
                updates
                    .locals
                    .push((self.transforms.slots[&id], u64::MAX, entity.local));
            }
            if matches!(kind, EntityWrite::Inspect | EntityWrite::Material)
                && let Some(material) = &entity.material
                && scene.materials[&MaterialId(id)].as_ref() != material
            {
                Arc::make_mut(&mut scene.materials)
                    .insert(MaterialId(id), Arc::new(material.clone()));
            }
        }

        self.propagate_transforms(world, &mut scene, &mut visited, updates, &structural)?;
        for (&key, owner) in &mut self.dependencies.components {
            if dirty_components.contains(key) || owner.changed(&self.transforms) {
                visited.insert(owner.id);
                owner.refresh(
                    self.registry[key],
                    world,
                    &self.transforms,
                    Arc::make_mut(&mut scene.components),
                )?;
                visited.extend(&owner.reads);
                visited.extend(owner.watched_settings.values().flatten());
            }
        }
        scene.stats.entities_visited = visited.count(&self.transforms);
        Ok(Some(Arc::new(scene)))
    }

    fn propagate_transforms(
        &mut self,
        world: &World,
        scene: &mut RenderScene,
        visited: &mut Visits,
        updates: TransformEdits,
        structural: &BTreeSet<Id>,
    ) -> Result<()> {
        let TransformEdits {
            locals: mut moved,
            lights: mut dirty_lights,
            rebound,
        } = updates;
        // Descendants occupy a contiguous range. Install every final local pose before
        // evaluating ranges so a changed child sees its parent's final world transform.
        moved.sort_by_key(|&(slot, version, _)| (slot, std::cmp::Reverse(version)));
        moved.dedup_by_key(|(slot, _, _)| *slot);
        moved.retain(|&(slot, _, local)| self.transforms.install_local(slot, local));
        let mut changed_instances: BTreeMap<_, _> = rebound.iter().map(|&i| (i, None)).collect();
        let mut dirty_skins = Vec::new();
        if !moved.is_empty() || self.pending_visual {
            dirty_skins.resize_with(self.dependencies.skins.len(), DirtyJoints::default);
            for &(slot, _, _) in moved.iter() {
                if let Some(users) = self.dependencies.pose_nodes.get(&slot) {
                    for &(skin, node) in users.entries() {
                        dirty_skins[skin].indices.push(node);
                    }
                }
            }
        }
        visited.subtrees = self.transforms.eager_spans(&moved);
        for range in &visited.subtrees {
            for slot in range.clone() {
                if !self.transforms.live(slot) {
                    continue;
                }
                let id = self.transforms.rows[slot].id;
                if self.transforms.resolve(slot) {
                    scene.stats.transforms_updated += 1;
                    let t = &self.transforms.rows[slot];
                    if let Some(index) = t.instance {
                        changed_instances.insert(index, None);
                    }
                    for &(skin, joint) in t.skins.entries() {
                        let dirty = &mut dirty_skins[skin];
                        dirty.count += 1;
                        if dirty.count * 2 <= self.dependencies.skins[skin].joint_count {
                            dirty.indices.push(joint);
                        }
                    }
                    if t.light {
                        dirty_lights.insert(id);
                    }
                }
            }
        }
        let deferred = if let Some(budget) = &mut self.pose_budget {
            let mut changed = BTreeSet::new();
            for (skin, dirty) in self.dependencies.skins.iter().zip(&dirty_skins) {
                if let Some(space) = &self.palettes[&skin.id].space
                    && (!space.pending.is_empty() || !dirty.indices.is_empty())
                {
                    let actor = self.transforms.rows[space.root].id;
                    if Some(actor) != self.focused_actor && (budget.eligible)(world, actor) {
                        changed.insert(actor);
                    }
                }
            }
            budget.defer(changed)
        } else {
            BTreeSet::new()
        };
        self.pending_visual = !deferred.is_empty();
        scene.stats.held_visual_actors = deferred.len();
        for (index, mut dirty) in dirty_skins.into_iter().enumerate() {
            let skin = &self.dependencies.skins[index];
            let len = skin.joint_count;
            let palette = self
                .palettes
                .get_mut(&skin.id)
                .expect("skin dependency must have a palette");
            if let Some(space) = &mut palette.space {
                let root = &self.transforms.rows[space.root];
                if !space.pending.is_empty() {
                    dirty.indices.append(&mut space.pending);
                    dirty.indices.sort_unstable();
                    dirty.indices.dedup();
                }
                let inspected = if deferred.contains(&root.id) {
                    space.pending = dirty.indices;
                    0
                } else {
                    space.update(
                        &self.transforms,
                        &dirty.indices,
                        &palette.inverse_bind,
                        &mut palette.matrices,
                    )
                };
                if inspected != 0 || root.version != space.root_version {
                    space.root_version = root.version;
                    visited.insert(skin.id);
                    scene.stats.palette_joints_examined += inspected;
                    scene.stats.palettes_updated += usize::from(inspected != 0);
                    for &index in &skin.instances {
                        changed_instances
                            .insert(index, Some((palette.matrices.clone(), root.world)));
                    }
                }
                continue;
            }
            if dirty.count == 0 {
                continue;
            }
            visited.insert(skin.id);
            let matrices = Arc::make_mut(&mut palette.matrices);
            let update = |i: usize| {
                let transform = &self.transforms.rows[palette.slots[i]];
                if transform.version != palette.joints[i].1 {
                    matrices[i] = transform.world * palette.inverse_bind[i];
                    palette.joints[i].1 = transform.version;
                }
            };
            if dirty.count * 2 > len {
                scene.stats.palette_joints_examined += len;
                (0..len).for_each(update);
            } else {
                scene.stats.palette_joints_examined += dirty.count;
                dirty.indices.into_iter().for_each(update);
            }
            scene.stats.palettes_updated += 1;
            for &index in &skin.instances {
                changed_instances.insert(index, Some((palette.matrices.clone(), Mat4::IDENTITY)));
            }
        }
        scene.stats.instances_updated += if structural.is_empty() {
            changed_instances.len()
        } else {
            changed_instances
                .keys()
                .filter(|&&i| !structural.contains(&scene.instances[i].id))
                .count()
        };
        if !changed_instances.is_empty() {
            let instances = Arc::make_mut(&mut scene.instances);
            for (index, palette) in changed_instances {
                let instance = Arc::make_mut(instances.edit(index));
                let transform = &self.transforms.get(instance.id).unwrap();
                if instance.transform_version != transform.version {
                    instance.model = transform.world;
                    instance.transform_version = transform.version;
                    scene.models.set(
                        scene.routes.model_slots[&instance.id],
                        &instance.gpu_model(),
                    );
                }
                if let Some((palette, root)) = palette {
                    instance.palette = Some(palette);
                    instance.palette_root = root;
                }
            }
        }
        if !dirty_lights.is_empty() {
            let mut edits = Vec::new();
            for &id in dirty_lights.iter() {
                visited.insert(id);
                let value = self.light(
                    id,
                    world.entities[&id].light.context("missing indexed light")?,
                )?;
                if scene.lights.entry(id) != value.as_ref() {
                    edits.push((id, value));
                }
            }
            if !edits.is_empty() {
                Arc::make_mut(&mut scene.lights).update(edits);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod pose_tests {
    use super::*;
    use glam::Vec3;

    #[test]
    fn sparse_dense_and_rebound_pose_blocks_match_full_extraction_for_every_reader() -> Result<()> {
        let mut world = World::demo();
        let mut part = world
            .entities
            .values()
            .find_map(|e| e.instance.clone())
            .unwrap();
        let mut weighted = world.entities[&part.mesh].mesh.as_deref().unwrap().clone();
        weighted.joints = vec![[0; 4]; weighted.positions.len()];
        weighted.weights = vec![[1., 0., 0., 0.]; weighted.positions.len()];
        part.mesh = world.spawn(Entity {
            mesh: Some(Arc::new(weighted)),
            ..Default::default()
        });
        let actor = world.spawn(Entity::default());
        let joints: Vec<_> = (0..130)
            .map(|_| {
                world.spawn(Entity {
                    parent: Some(actor),
                    ..Default::default()
                })
            })
            .collect();
        let skin = world.spawn(Entity {
            skin: Some(Skin {
                joints: joints.clone(),
                inverse_bind: vec![Mat4::IDENTITY; joints.len()],
            }),
            ..Default::default()
        });
        for _ in 0..2 {
            world.spawn(Entity {
                instance: Some(part.clone()),
                skin_binding: Some(skin),
                ..Default::default()
            });
        }
        let snap = |r: &mut SceneExtractor, w: &World| {
            r.snapshot(w, Camera::default(), [16, 16], Default::default())
        };
        let palettes = |s: &Snapshot| -> Vec<Arc<[Mat4]>> {
            s.scene
                .instances()
                .iter()
                .filter_map(|i| i.palette.clone())
                .collect()
        };
        let (mut fast, mut slow) = (SceneExtractor::default(), SceneExtractor::default());
        let still = snap(&mut fast, &world)?;
        snap(&mut slow, &world)?;
        let binding = PoseLayout::new(joints.iter().copied())?;
        assert!(PoseLayout::new([joints[2], joints[2]]).is_err());
        let mut locals = vec![Mat4::IDENTITY; joints.len()];
        let shift = |n| Mat4::from_translation(Vec3::X * n);
        locals[129] = shift(1.);
        world
            .entities
            .publish_pose(actor, binding.clone(), locals.clone());
        let sparse = snap(&mut fast, &world)?;
        assert_eq!(sparse.scene.stats.palette_joints_examined, 1);
        assert_eq!(
            (
                sparse.scene.stats.pose_blocks_read,
                sparse.scene.stats.pose_locals_read
            ),
            (1, 1)
        );
        assert_eq!(palettes(&sparse)[0][129], shift(1.));
        assert_eq!(palettes(&still)[0][129], Mat4::IDENTITY);
        let shared = palettes(&sparse);
        assert!(Arc::ptr_eq(&shared[0], &shared[1]));
        // Full-density publication switches to a linear scan. A direct author edit after
        // the pose is the final source even when it restores a joint to its original value.
        locals.fill(shift(2.));
        world.entities.publish_pose(actor, binding.clone(), locals);
        world.set_local(joints[4], Mat4::IDENTITY)?;
        let dense = snap(&mut fast, &world)?;
        assert_eq!(dense.scene.stats.palette_joints_examined, joints.len());
        assert_eq!(
            palettes(&dense)[0],
            palettes(&snap(&mut SceneExtractor::default(), &world)?)[0]
        );
        // One epoch can replace a rig and transfer some of its joints to another writer.
        let pair = PoseLayout::new([joints[4], joints[5]])?;
        world
            .entities
            .publish_pose(actor, pair.clone(), vec![shift(3.); 2]);
        world
            .entities
            .publish_pose(actor, PoseLayout::new([joints[6]])?, vec![shift(4.)]);
        world
            .entities
            .publish_pose(skin, pair, vec![shift(1.), shift(5.)]);
        let cold = snap(&mut SceneExtractor::default(), &world)?;
        for reader in [&mut fast, &mut slow] {
            assert_eq!(palettes(&snap(reader, &world)?), palettes(&cold));
        }
        let file = tempfile::NamedTempFile::new()?;
        let location = file.into_temp_path();
        world.save(&location)?;
        let reloaded = World::load(&location)?;
        assert_eq!(palettes(&snap(&mut fast, &reloaded)?), palettes(&cold));

        // Removing an earlier unskinned draw shifts both skin users to new instance slots.
        let first = world
            .entities
            .iter()
            .find_map(|(&id, e)| (e.instance.is_some() && e.skin_binding.is_none()).then_some(id))
            .unwrap();
        world.entities.get_mut(&first).unwrap().instance = None;
        assert!(snap(&mut slow, &world)?.scene.stats.full_rebuild);
        world.set_local(joints[9], shift(9.))?;
        let shifted = snap(&mut slow, &world)?;
        assert_eq!(shifted.scene.stats.instances_updated, 2);
        assert_eq!(palettes(&shifted), palettes(&snap(&mut fast, &world)?));
        Ok(())
    }
}
