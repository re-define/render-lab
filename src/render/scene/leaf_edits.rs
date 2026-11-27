//! Journaled membership changes for ordinary draw leaves and lamps.
//! Only a non-tail change needs to rebuild indices into the ordered draw table.
use super::*;

impl SceneExtractor {
    pub(super) fn apply_leaf_edits(
        &mut self,
        world: &World,
        ids: &BTreeSet<Id>,
        scene: &mut RenderScene,
    ) -> Result<bool> {
        for &id in ids {
            if let Some(&slot) = self.transforms.slots.get(&id) {
                if !self.transforms.removable_leaf(slot)
                    || self.dependencies.pose_nodes.contains_key(&slot)
                {
                    return Ok(false);
                }
                if let Some(index) = self.transforms.rows[slot].instance {
                    let row = &scene.instances[index];
                    if row.skinned
                        || row.geometry_path(&scene.meshes[&row.mesh], true)
                            != GeometryPath::Ordinary
                    {
                        return Ok(false);
                    }
                }
            }
            if let Some(entity) = world.entities.get(&id) {
                if !Entities::plain_leaf(entity)
                    || entity
                        .parent
                        .is_some_and(|p| ids.contains(&p) || self.transforms.get(p).is_none())
                {
                    return Ok(false);
                }
                if let Some(i) = &entity.instance {
                    world.validate_geometry_binding(entity, i.geometry_assets)?;
                    let mesh = scene
                        .meshes
                        .get(&MeshId(i.mesh))
                        .context("missing render instance mesh")?;
                    if let Some(id) = i.material {
                        ensure!(
                            scene.materials.contains_key(&MaterialId(id)),
                            "missing render instance material {id}"
                        );
                    }
                    if mesh.clustered(i.outline) {
                        return Ok(false);
                    }
                }
            }
        }
        let mut lights = Vec::new();
        let mut rows = Vec::new();
        for &id in ids {
            let old = self.transforms.slots.get(&id).map(|&slot| {
                let t = &self.transforms.rows[slot];
                (slot, t.light, t.instance)
            });
            if let Some((slot, _, _)) = old {
                self.transforms.remove_leaf(slot);
            }
            let entity = world.entities.get(&id);
            let new = if let Some(e) = entity {
                let slot = self.transforms.insert_leaf(id, e)?;
                scene.stats.transforms_updated += 1;
                e.instance.as_ref().map(|_| {
                    Arc::new(RenderInstance::from_entity(
                        e,
                        &self.transforms.rows[slot],
                        world.entities.incarnation(id),
                        None,
                        Mat4::IDENTITY,
                    ))
                })
            } else {
                None
            };
            let old_light = old.is_some_and(|(_, light, _)| light);
            let light = entity.and_then(|e| e.light);
            if old_light || light.is_some() {
                self.dependencies.light_count -= usize::from(old_light);
                self.dependencies.light_count += usize::from(light.is_some());
                lights.push((id, light.map(|l| self.light(id, l)).transpose()?.flatten()));
            }
            let index = old.and_then(|(_, _, index)| index);
            if index.is_some() || new.is_some() {
                scene.stats.instances_updated += usize::from(new.is_some());
                rows.push((id, index, new));
            }
        }
        if !lights.is_empty() {
            let table = Arc::make_mut(&mut scene.lights);
            table.present = self.dependencies.light_count != 0;
            table.update(lights);
        }
        if rows.is_empty() {
            return Ok(true);
        }
        let original = scene.instances.clone();
        let tail_only = rows.iter().all(|(id, old, _)| match old {
            Some(index) => index + 1 == original.len(),
            None => original.len() == 0 || *id > original[original.len() - 1].id,
        });
        if tail_only {
            let routes = Arc::make_mut(&mut scene.routes);
            let pages = Arc::make_mut(&mut scene.instances);
            for (_, old, row) in &rows {
                if let Some(index) = old {
                    routes.pop_ordinary(&original[*index]);
                    pages.pop();
                }
                if let Some(row) = row {
                    let index = pages.len();
                    self.transforms.row_mut(row.id).instance = Some(index);
                    routes.push_ordinary(index, row);
                    pages.push(row.clone());
                }
            }
        } else {
            // Sorted draw indices appear in skin subscribers and the cluster motion ABI.
            // Reindex the render snapshot once, retaining immutable rows and stable model slots.
            let mut table: BTreeMap<_, _> = original.iter().map(|r| (r.id, r.clone())).collect();
            for (id, _, row) in &rows {
                if let Some(row) = row {
                    table.insert(*id, row.clone());
                } else {
                    table.remove(id);
                }
            }
            let sorted: Vec<_> = table.into_values().collect();
            for (i, row) in sorted.iter().enumerate() {
                self.transforms.row_mut(row.id).instance = Some(i);
            }
            for skin in &mut self.dependencies.skins {
                for index in &mut skin.instances {
                    *index = self
                        .transforms
                        .get(original[*index].id)
                        .unwrap()
                        .instance
                        .unwrap();
                }
            }
            scene.routes = Arc::new(GeometryRoutes::new(
                &sorted,
                &scene.meshes,
                &scene.materials,
                Some(&scene.routes),
            )?);
            scene.instances = Arc::new(InstancePages::new(sorted));
        }
        scene.geometry_revision = next_geometry_revision();
        scene.models.resize_records(scene.routes.model_capacity);
        for (_, _, row) in rows {
            if let Some(row) = row {
                scene
                    .models
                    .set(scene.routes.model_slots[&row.id], &row.gpu_model());
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    fn snap(x: &mut SceneExtractor, w: &World) -> Result<Arc<RenderScene>> {
        Ok(
            x.snapshot(w, Camera::default(), [80, 64], Default::default())?
                .scene,
        )
    }
    #[test]
    fn stable_slots_parent_propagation_and_recycled_ids_match_across_readers() -> Result<()> {
        let mut w = World::demo();
        let template = w
            .entities
            .values()
            .find_map(|e| e.instance.clone())
            .unwrap();
        let part = |parent| Entity {
            parent,
            instance: Some(template.clone()),
            ..Default::default()
        };
        let anchor = w.spawn(Entity::default());
        let middle = w.spawn(part(Some(anchor)));
        let survivor = w.spawn(part(Some(anchor)));
        let (mut fast, mut slow) = (SceneExtractor::default(), SceneExtractor::default());
        let first = snap(&mut fast, &w)?;
        snap(&mut slow, &w)?;
        w.entities.remove(&middle);
        let removed = snap(&mut fast, &w)?;
        assert!(!removed.stats.full_rebuild);
        assert_eq!(removed.stats.entities_visited, 1);
        assert_eq!(
            removed.routes.model_slots[&survivor],
            first.routes.model_slots[&survivor]
        );

        let appended = w.spawn(part(Some(anchor)));
        let inserted = snap(&mut fast, &w)?;
        assert!(!inserted.stats.full_rebuild);
        assert_eq!(
            inserted.routes.model_slots[&appended],
            first.routes.model_slots[&middle]
        );
        let branch = snap(&mut slow, &w)?;
        assert_eq!(branch.stats.entities_visited, 2);
        assert!(!branch.stats.full_rebuild);
        for wip in [&branch, &inserted] {
            assert_eq!(
                wip.routes.model_slots[&survivor],
                first.routes.model_slots[&survivor]
            );
            assert_eq!(
                wip.routes.model_slots[&appended],
                first.routes.model_slots[&middle]
            );
        }
        let p = Mat4::from_translation(Vec3::Z);
        w.set_local(anchor, p)?;
        let moved = snap(&mut fast, &w)?;
        assert_eq!(
            (moved.stats.entities_visited, moved.stats.instances_updated),
            (3, 2)
        );
        for id in [survivor, appended] {
            assert_eq!(moved.instances[moved.instance_index(id).unwrap()].model, p);
            assert_eq!(
                inserted.instances[inserted.instance_index(id).unwrap()].model,
                Mat4::IDENTITY
            );
            assert_eq!(
                bytemuck::bytes_of(&moved.instances[moved.instance_index(id).unwrap()].gpu_model()),
                moved.models.record(moved.routes.model_slots[&id])
            );
        }
        let old = moved.instances[moved.instance_index(appended).unwrap()].incarnation;
        w.entities.remove(&appended);
        assert_eq!(w.spawn(part(None)), appended);
        let replaced = snap(&mut fast, &w)?;
        assert_ne!(
            old,
            replaced.instances[replaced.instance_index(appended).unwrap()].incarnation
        );
        assert_eq!(
            first.instances[first.instance_index(middle).unwrap()].model,
            Mat4::IDENTITY
        );
        let cold = snap(&mut SceneExtractor::default(), &w)?;
        let geometry = |s: &RenderScene| {
            s.instances
                .iter()
                .map(|i| (i.id, i.mesh, i.model, i.material))
                .collect::<Vec<_>>()
        };
        assert_eq!(geometry(&replaced), geometry(&cold));
        Ok(())
    }
    #[test]
    fn authored_disabled_light_and_a_removed_parent_are_accounted_for() -> Result<()> {
        let mut w = World::default();
        let mut x = SceneExtractor::default();
        let empty = snap(&mut x, &w)?;
        let mut light = Light::point([1.; 3], 2., 12.);
        light.enabled = false;
        let lamp = w.spawn(Entity {
            light: Some(light),
            ..Default::default()
        });
        let disabled = snap(&mut x, &w)?;
        assert!(
            !empty.lights.present && disabled.lights.present && disabled.lights.entries.is_empty()
        );
        let light = Light::point([1.; 3], 4., 10.);
        let child = w.spawn(Entity {
            parent: Some(lamp),
            light: Some(light),
            ..Default::default()
        });
        let active = snap(&mut x, &w)?;
        assert_eq!(active.lights.entries[0].id, child);
        assert!(!active.stats.full_rebuild);
        w.entities.remove(&lamp);
        assert!(snap(&mut x, &w).is_err());
        w.entities.remove(&child);
        let cleared = snap(&mut x, &w)?;
        assert!(!cleared.lights.present && cleared.lights.entries.is_empty());
        assert_eq!(active.lights.entries[0].id, child);
        Ok(())
    }
}
