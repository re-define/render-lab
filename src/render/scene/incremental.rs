use super::*;

/// Reverse edges needed to propagate a change without walking all scene entities.
#[derive(Default)]
pub(super) struct Dependencies {
    pub children: BTreeMap<Id, Vec<Id>>,
    pub joint_skins: BTreeMap<Id, Vec<Id>>,
    pub skin_users: BTreeMap<Id, Vec<Id>>,
    pub lights: BTreeSet<Id>,
    pub components: BTreeMap<&'static str, ComponentOwner>,
}

impl RenderScene {
    fn instance_index(&self, id: Id) -> Option<usize> {
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
                old.mesh == MeshId(instance.mesh)
                    && old.material == instance.material.map(MaterialId)
                    && old.outline == instance.outline
            }
            (None, None) => true,
            _ => false,
        }
    }
}

impl SceneExtractor {
    pub(super) fn incremental(
        &mut self,
        world: &World,
        edits: impl Iterator<Item = (Id, EntityWrite)>,
    ) -> Result<Option<Arc<RenderScene>>> {
        let previous = self.previous.as_ref().expect("cursor requires a snapshot");
        let edits: BTreeSet<_> = edits.collect();
        if edits.is_empty() && previous.stats == ExtractionStats::default() {
            return Ok(Some(previous.clone()));
        }
        let mut scene = previous.as_ref().clone();
        scene.stats = ExtractionStats::default();
        let mut visited = BTreeSet::new();
        let mut moved = BTreeSet::new();
        let mut dirty_components = BTreeSet::new();
        for (id, kind) in edits {
            let entity = &world.entities[&id];
            visited.insert(id);
            match kind {
                EntityWrite::Inspect => {
                    if !Entities::inspectable(entity)
                        || entity.parent != self.transforms.entries[&id].parent
                        || !scene.same_bindings(id, entity)
                    {
                        return Ok(None);
                    }
                }
                EntityWrite::Component(key) => {
                    for (&dependent_key, owner) in &self.dependencies.components {
                        if owner.watched_settings.contains_key(key) {
                            dirty_components.insert(dependent_key);
                        }
                    }
                    let Some(spec) = self.registry.get(key) else {
                        // Unregistered keys can also affect geometry routes or visibility.
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
                EntityWrite::Local | EntityWrite::Material => (),
            }
            if matches!(kind, EntityWrite::Inspect | EntityWrite::Local)
                && entity.local != self.transforms.entries[&id].local
            {
                moved.insert(id);
            }
            if matches!(kind, EntityWrite::Inspect | EntityWrite::Material)
                && let Some(material) = &entity.material
                && scene.materials[&MaterialId(id)].as_ref() != material
            {
                Arc::make_mut(&mut scene.materials)
                    .insert(MaterialId(id), Arc::new(material.clone()));
            }
        }

        self.propagate_transforms(world, &moved, &mut visited, &mut scene)?;
        for (&key, owner) in &mut self.dependencies.components {
            if dirty_components.contains(key) || owner.changed(&self.transforms) {
                visited.insert(owner.id);
                owner.refresh(
                    self.registry[key],
                    world,
                    &self.transforms,
                    Arc::make_mut(&mut scene.components),
                )?;
                visited.extend(owner.watched_settings.values().flatten());
            }
        }
        scene.stats.entities_visited = visited.len();
        Ok(Some(Arc::new(scene)))
    }

    fn propagate_transforms(
        &mut self,
        world: &World,
        moved: &BTreeSet<Id>,
        visited: &mut BTreeSet<Id>,
        scene: &mut RenderScene,
    ) -> Result<()> {
        // Start with the outermost dirty ancestors so each subtree is resolved parent first, once.
        let mut pending = Vec::new();
        for &id in moved {
            let mut parent = self.transforms.entries[&id].parent;
            while let Some(p) = parent {
                if moved.contains(&p) {
                    break;
                }
                parent = self.transforms.entries[&p].parent;
            }
            if parent.is_none() {
                pending.push(id);
            }
        }
        let mut changed_instances = BTreeMap::new();
        let mut dirty_skins = BTreeSet::new();
        let mut update_lights = false;
        while let Some(id) = pending.pop() {
            visited.insert(id);
            if self.transforms.update_one(id, &world.entities[&id])? {
                scene.stats.transforms_updated += 1;
                if let Some(index) = scene.instance_index(id) {
                    changed_instances.insert(index, None);
                }
                if let Some(skins) = self.dependencies.joint_skins.get(&id) {
                    dirty_skins.extend(skins.iter().copied());
                }
                update_lights |= self.dependencies.lights.contains(&id);
            }
            if let Some(children) = self.dependencies.children.get(&id) {
                pending.extend_from_slice(children);
            }
        }
        for skin in dirty_skins {
            visited.insert(skin);
            if self.update_palette(world, skin)? {
                scene.stats.palettes_updated += 1;
                for &user in &self.dependencies.skin_users[&skin] {
                    let index = scene.instance_index(user).expect("indexed skin user");
                    changed_instances.insert(index, Some(self.palettes[&skin].matrices.clone()));
                }
            }
        }
        scene.stats.instances_updated = changed_instances.len();
        if !changed_instances.is_empty() {
            let instances = Arc::make_mut(&mut scene.instances);
            for (index, palette) in changed_instances {
                let instance = Arc::make_mut(instances.edit(index));
                let transform = &self.transforms.entries[&instance.id];
                instance.model = transform.world;
                instance.transform_version = transform.version;
                if let Some(palette) = palette {
                    instance.palette = Some(palette);
                }
                scene.models.set(
                    scene.routes.model_slots[&instance.id],
                    &instance.gpu_model(),
                );
            }
        }
        if update_lights {
            let mut entries = Vec::new();
            for &id in &self.dependencies.lights {
                visited.insert(id);
                if let Some(light) =
                    self.light(id, world.entities[&id].light.expect("indexed light"))?
                {
                    entries.push(light);
                }
            }
            if scene.lights.entries != entries {
                scene.lights = Arc::new(SceneLights::new(true, entries));
            }
        }
        Ok(())
    }
}
