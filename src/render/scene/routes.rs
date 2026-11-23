use super::*;

/// Stable for the lifetime of one structural layout. Transform and material edits share it.
pub(crate) struct GeometryRoutes {
    pub model_slots: BTreeMap<Id, usize>,
    pub(crate) material_slots: BTreeMap<MaterialId, usize>,
    pub ordinary: Vec<usize>,
    pub groups: BTreeMap<MeshId, Vec<usize>>,
    pub bindings: Arc<[u8]>,
    pub materials: Vec<Option<MaterialId>>,
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
            prior.map(|p| &p.model_slots),
            0,
        );
        let material_slots = stable_slots(
            materials.keys().copied(),
            prior.map(|p| &p.material_slots),
            1,
        );
        let mut materials = vec![None; 1 + material_slots.values().max().copied().unwrap_or(0)];
        for i in instances {
            if let Some(id) = i.material {
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
        Ok(Self {
            model_slots,
            material_slots,
            ordinary,
            groups,
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
