use super::*;
pub(super) struct HitTable {
    layout: std::sync::Weak<scene::GeometryRoutes>,
    sources: Arc<BTreeMap<MaterialId, Arc<Material>>>,
    data: Rows,
    program_starts: Vec<usize>,
    revision: u64,
}
impl Atlas {
    /// Stable scene material slots, shared by CPU-packed and GPU-deformed triangles.
    pub(crate) fn gi_hit_table(scene: &RenderScene, g: &mut FrameGraph) -> Result<(Resource, u64)> {
        use crate::scene::{GiHitModel, GiTexture};
        use std::hash::{Hash, Hasher};
        let atlas = Self::cached(scene)?;
        let mut table = atlas.hit_table.lock().unwrap();
        let layout = Arc::downgrade(&scene.routes);
        let signature = |material_revision: u64| {
            let mut key = std::hash::DefaultHasher::new();
            (atlas.revision, material_revision).hash(&mut key);
            key.finish()
        };
        if let Some(c) = table
            .as_ref()
            .filter(|c| c.layout.ptr_eq(&layout) && Arc::ptr_eq(&c.sources, &scene.materials))
        {
            return Ok((
                g.asset_table("GI hit material table", &c.data, false),
                signature(c.revision),
            ));
        }
        let fallback = Material::default();
        // Four uvec4 header words per material, followed by one uvec4 per program node.
        let prior = table.as_ref().filter(|t| t.layout.ptr_eq(&layout));
        let mut data = prior.map_or_else(Rows::words, |t| t.data.clone());
        let mut end = scene.routes.materials.len() * 4;
        let mut starts = Vec::with_capacity(scene.routes.materials.len());
        for id in &scene.routes.materials {
            starts.push(end);
            if let Some(GiHitModel::Program { nodes, .. }) = id
                .and_then(|id| scene.materials.get(&id))
                .map(|m| &m.gi_hit.model)
            {
                end += nodes.len();
            }
        }
        data.resize_words(end * 4);
        for (slot, id) in scene.routes.materials.iter().enumerate() {
            if let Some(old) = prior
                && old.program_starts[slot] == starts[slot]
                && id.is_none_or(|id| {
                    old.sources
                        .get(&id)
                        .zip(scene.materials.get(&id))
                        .is_some_and(|(a, b)| Arc::ptr_eq(a, b))
                })
            {
                continue;
            }
            let mut rows = vec![[0u32; 4]; 4];
            let m = id
                .and_then(|id| scene.materials.get(&id))
                .map_or(&fallback, Arc::as_ref);
            let region = |source| {
                match source {
                    GiTexture::Inherit => m.texture,
                    GiTexture::Constant => None,
                    GiTexture::Texture(id) => Some(id),
                }
                .map_or([0, 1, 1], |id| atlas.locations[&id])
            };
            let color = m.gi_hit.reflectance.unwrap_or_else(|| {
                let scale = if matches!(
                    m.gi_hit.model,
                    GiHitModel::Diffuse | GiHitModel::ThinFoliage
                ) {
                    1. - m.metallic
                } else {
                    1.
                };
                std::array::from_fn(|i| (m.base_color[i] * scale).clamp(0., 1.))
            });
            let base = 0;
            rows[base][..3].copy_from_slice(&color.map(f32::to_bits));
            rows[base + 1][..3].copy_from_slice(&m.gi_hit.emission.map(f32::to_bits));
            rows[base + 2][..3].copy_from_slice(&region(m.gi_hit.texture));
            rows[base + 2][3] = u32::from(m.gi_hit.two_sided);
            match &m.gi_hit.model {
                GiHitModel::Diffuse => {}
                GiHitModel::ThinFoliage => rows[base][3] = 3,
                GiHitModel::Unlit => rows[base][3] = 2,
                GiHitModel::Program {
                    nodes,
                    diffuse,
                    emission,
                } => {
                    rows[base][3] = 1;
                    rows[base + 3] = [
                        starts[slot].try_into()?,
                        nodes.len().try_into()?,
                        u32::from(*diffuse),
                        u32::from(*emission),
                    ];
                    for node in nodes {
                        use crate::scene::GiNode::*;
                        let (op, args) = match *node {
                            Constant(rgb) => (0, rgb.map(f32::to_bits)),
                            Texture(t) => (
                                1,
                                region(if t == GiTexture::Inherit {
                                    m.gi_hit.texture
                                } else {
                                    t
                                }),
                            ),
                            Luminance(x) => (2, [x.into(), 0, 0]),
                            Multiply([x, y]) => (3, [x.into(), y.into(), 0]),
                            Add([x, y]) => (4, [x.into(), y.into(), 0]),
                            Mix(x) => (5, x.map(u32::from)),
                        };
                        rows.push([op, args[0], args[1], args[2]]);
                    }
                }
            }
            data.write_words(slot * 16, bytemuck::cast_slice(&rows[..4]));
            data.write_words(starts[slot] * 4, bytemuck::cast_slice(&rows[4..]));
        }
        let revision = prior.filter(|t| t.data.identical(&data)).map_or_else(
            || NEXT_REVISION.fetch_add(1, Ordering::Relaxed),
            |t| t.revision,
        );
        let slot = g.asset_table("GI hit material table", &data, false);
        *table = Some(HitTable {
            layout,
            sources: scene.materials.clone(),
            data,
            program_starts: starts,
            revision,
        });
        Ok((slot, signature(revision)))
    }
}
