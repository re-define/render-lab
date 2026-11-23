use super::*;
pub(super) struct HitTable {
    layout: std::sync::Weak<scene::GeometryRoutes>,
    sources: std::sync::Weak<BTreeMap<MaterialId, Arc<Material>>>,
    data: Arc<[u8]>,
    signature: u64,
}
impl Atlas {
    /// Stable scene material slots, shared by CPU-packed and GPU-deformed triangles.
    pub(crate) fn gi_hit_table(scene: &RenderScene, g: &mut FrameGraph) -> Result<(Resource, u64)> {
        use crate::scene::{GiHitModel, GiTexture};
        use std::hash::{Hash, Hasher};
        let atlas = Self::cached(scene)?;
        let mut table = atlas.hit_table.lock().unwrap();
        let layout = Arc::downgrade(&scene.routes);
        let sources = Arc::downgrade(&scene.materials);
        if let Some(c) = table
            .as_ref()
            .filter(|c| c.layout.ptr_eq(&layout) && c.sources.ptr_eq(&sources))
        {
            return Ok((
                g.resident_input("GI hit material table", c.data.clone()),
                c.signature,
            ));
        }
        let fallback = Material::default();
        // Four uvec4 header words per material, followed by one uvec4 per program node.
        let mut rows = vec![[0u32; 4]; scene.routes.materials.len() * 4];
        for (slot, id) in scene.routes.materials.iter().enumerate() {
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
            let base = slot * 4;
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
                        rows.len().try_into()?,
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
        }
        let mut signature = std::hash::DefaultHasher::new();
        rows.hash(&mut signature);
        Arc::as_ptr(&atlas).hash(&mut signature);
        let signature = signature.finish();
        let data: Arc<[u8]> = bytemuck::cast_slice(&rows).into();
        let slot = g.resident_input("GI hit material table", data.clone());
        *table = Some(HitTable {
            layout,
            sources,
            data,
            signature,
        });
        Ok((slot, signature))
    }
}
