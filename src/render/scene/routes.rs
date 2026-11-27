use super::*;

/// Stable for the lifetime of one structural layout. Transform and material edits share it.
#[derive(Clone)]
pub(crate) struct GeometryRoutes {
    free_models: BTreeSet<usize>,
    pub(super) model_capacity: usize,
    material_uses: Vec<usize>,
    pub model_slots: ModelSlots,
    pub(crate) material_slots: BTreeMap<MaterialId, usize>,
    pub ordinary: Vec<usize>,
    pub groups: Arc<BTreeMap<MeshId, Vec<usize>>>,
    pub bindings: Arc<[u8]>,
    pub materials: Vec<Option<MaterialId>>,
}
/// The main table and clustered bindings can span the whole forest. Tail edits copy
/// only deviations from the immutable table, which is compacted as the edits grow.
#[derive(Clone)]
pub(crate) struct ModelSlots {
    base: Arc<BTreeMap<Id, usize>>,
    edits: BTreeMap<Id, Option<usize>>,
}
impl ModelSlots {
    fn table(&self) -> std::borrow::Cow<'_, BTreeMap<Id, usize>> {
        if self.edits.is_empty() {
            return std::borrow::Cow::Borrowed(&self.base);
        }
        let mut table = self.base.as_ref().clone();
        for (&id, &value) in &self.edits {
            if let Some(slot) = value {
                table.insert(id, slot);
            } else {
                table.remove(&id);
            }
        }
        std::borrow::Cow::Owned(table)
    }
    fn set(&mut self, id: Id, value: Option<usize>) {
        if self.base.get(&id).copied() == value {
            self.edits.remove(&id);
        } else {
            self.edits.insert(id, value);
        }
        if self.edits.len() > 256.max(self.base.len() / 4) {
            self.base = Arc::new(self.table().into_owned());
            self.edits.clear();
        }
    }
}
impl std::ops::Index<&Id> for ModelSlots {
    type Output = usize;
    fn index(&self, id: &Id) -> &usize {
        match self.edits.get(id) {
            Some(value) => value.as_ref().expect("live model slot"),
            None => &self.base[id],
        }
    }
}
impl RenderInstance {
    pub(crate) fn gpu_model(&self) -> [u32; 20] {
        let mut words = [0; 20];
        words[..16].copy_from_slice(&self.model.to_cols_array().map(f32::to_bits));
        words[16] = meshoptimizer_rs::clusterlod::cut::max_scale(
            glam::Mat3::from_mat4(self.model)
                .transpose()
                .to_cols_array_2d(),
        )
        .to_bits();
        words[19] = u32::from(self.outline);
        words[17] = self.transform_version as u32;
        words[18] = (self.transform_version >> 32) as u32;
        words
    }
}
/// Retain live IDs, reuse holes only in the new immutable layout. An in-flight frame retains
/// both its old layout and its own Vulkan buffer/version until its timeline retires.
fn stable_slots<K: Copy + Ord>(
    keys: impl Iterator<Item = K>,
    prior: Option<&BTreeMap<K, usize>>,
    first: usize,
) -> BTreeMap<K, usize> {
    let keys: BTreeSet<_> = keys.collect();
    let mut slots: BTreeMap<_, _> = prior
        .into_iter()
        .flat_map(|p| p.iter())
        .filter(|(k, _)| keys.contains(k))
        .map(|(&k, &v)| (k, v))
        .collect();
    let occupied: BTreeSet<_> = slots.values().copied().collect();
    let mut free = (first..).filter(|slot| !occupied.contains(slot));
    for k in keys {
        slots
            .entry(k)
            .or_insert_with(|| free.next().expect("bounded by instance memory"));
    }
    slots
}
impl GeometryRoutes {
    pub(super) fn push_ordinary(&mut self, index: usize, row: &RenderInstance) -> usize {
        let slot = self.free_models.pop_first().unwrap_or_else(|| {
            let slot = self.model_capacity;
            self.model_capacity += 1;
            slot
        });
        self.model_slots.set(row.id, Some(slot));
        self.ordinary.push(index);
        if let Some(id) = row.material {
            let m = self.material_slots[&id];
            self.material_uses[m] += 1;
            self.materials[m] = Some(id);
        }
        slot
    }
    pub(super) fn pop_ordinary(&mut self, row: &RenderInstance) {
        self.ordinary.pop();
        self.free_models.insert(self.model_slots[&row.id]);
        self.model_slots.set(row.id, None);
        if let Some(id) = row.material {
            let m = self.material_slots[&id];
            self.material_uses[m] -= 1;
            if self.material_uses[m] == 0 {
                self.materials[m] = None;
            }
        }
    }
    pub(super) fn new(
        instances: &[Arc<RenderInstance>],
        meshes: &BTreeMap<MeshId, Arc<MeshAsset>>,
        materials: &BTreeMap<MaterialId, Arc<Material>>,
        prior: Option<&Self>,
    ) -> Result<Self> {
        let mut ordinary = vec![];
        let mut groups = BTreeMap::<MeshId, Vec<usize>>::new();
        for (slot, i) in instances.iter().enumerate() {
            if i.geometry_path(&meshes[&i.mesh], true) == GeometryPath::ClusterLod {
                groups.entry(i.mesh).or_default().push(slot);
            } else {
                ordinary.push(slot);
            }
        }
        let model_slots = stable_slots(
            instances.iter().map(|i| i.id),
            prior.map(|p| p.model_slots.table()).as_deref(),
            0,
        );
        let material_slots = stable_slots(
            materials.keys().copied(),
            prior.map(|p| &p.material_slots),
            1,
        );
        let mut materials = vec![None; 1 + material_slots.values().max().copied().unwrap_or(0)];
        let mut material_uses = vec![0; materials.len()];
        for i in instances {
            if let Some(id) = i.material {
                material_uses[material_slots[&id]] += 1;
                materials[material_slots[&id]] = Some(id);
            }
        }
        let mut bindings = Vec::new();
        let mut bone = 0u32;
        for (asset, (id, slots)) in groups.iter().enumerate() {
            let bones = meshes[id]
                .plant
                .as_ref()
                .map_or(0, |p| p.bones.len() as u32);
            for &slot in slots {
                let i = &instances[slot];
                let m = i.material.map_or(0, |id| material_slots[&id]);
                bindings.push([
                    u32::try_from(model_slots[&i.id])?,
                    asset.try_into()?,
                    m.try_into()?,
                    bone,
                    u32::try_from(slot + 1)?
                        .checked_mul(4)
                        .context("motion slot overflow")?,
                    0,
                    0,
                    0,
                ]);
                bone = bone
                    .checked_add(bones)
                    .context("plant joint index overflow")?;
            }
        }
        let model_capacity = model_slots.values().max().map_or(0, |slot| slot + 1);
        let mut occupied = vec![false; model_capacity];
        for &slot in model_slots.values() {
            occupied[slot] = true;
        }
        let free_models = occupied
            .into_iter()
            .enumerate()
            .filter_map(|(i, used)| (!used).then_some(i))
            .collect();
        let model_slots = ModelSlots {
            base: Arc::new(model_slots),
            edits: BTreeMap::new(),
        };
        Ok(Self {
            free_models,
            model_capacity,
            material_uses,
            model_slots,
            material_slots,
            ordinary,
            groups: Arc::new(groups),
            bindings: bytemuck::cast_slice(&bindings).into(),
            materials,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remove_and_rebind_preserves_surviving_slots_and_old_snapshot() {
        let a = stable_slots([2, 7, 8].into_iter(), None, 1);
        let b = stable_slots([3, 7, 8, 9].into_iter(), Some(&a), 1);
        assert_eq!(a[&2], b[&3]);
        assert_eq!((a[&7], a[&8]), (b[&7], b[&8]));
        assert_eq!(b[&9], 4);
        assert!(!a.contains_key(&3));
    }
}
