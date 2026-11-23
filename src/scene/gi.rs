use super::*;
/// Diffuse transport at a secondary hit, independent of the visible material program.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GiHitMaterial {
    /// Linear RGB tint. Diffuse also applies the nonmetallic fraction when inheriting base color.
    pub reflectance: Option<[f32; 3]>,
    pub model: GiHitModel,
    pub texture: GiTexture,
    pub emission: [f32; 3],
    pub two_sided: bool,
}

/// The hit program returns diffuse throughput and emitted radiance in linear RGB.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GiHitModel {
    #[default]
    Diffuse,
    /// Thin vegetation with diffuse reflection and transmission on opposite sides.
    ThinFoliage,
    /// Node indices reference preceding nodes. Outputs are multiplied by tint or added to emission.
    Program {
        nodes: Vec<GiNode>,
        diffuse: u8,
        emission: u8,
    },
    /// Treat the selected texture and tint as emitted radiance and terminate scattering.
    Unlit,
}
impl GiHitMaterial {
    pub(super) fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GiTexture {
    #[default]
    Inherit,
    Constant,
    Texture(Id),
}

/// A bounded acyclic expression evaluated at the secondary UV. Colors and weights are linear.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GiNode {
    Constant([f32; 3]),
    Texture(GiTexture),
    Luminance(u8),
    Multiply([u8; 2]),
    Add([u8; 2]),
    Mix([u8; 3]),
}
impl GiHitMaterial {
    pub(super) fn validate(&self, world: &World) -> Result<()> {
        ensure!(
            self.reflectance
                .iter()
                .flatten()
                .all(|v| (0.0..=1.).contains(v))
                && self.emission.iter().all(|v| v.is_finite() && *v >= 0.),
            "invalid GI response"
        );
        let texture = |source: GiTexture| -> Result<()> {
            if let GiTexture::Texture(id) = source {
                ensure!(
                    world.entities.get(&id).is_some_and(|e| e.texture.is_some()),
                    "missing GI texture {id}"
                );
            }
            Ok(())
        };
        texture(self.texture)?;
        if let GiHitModel::Program {
            nodes,
            diffuse,
            emission,
        } = &self.model
        {
            ensure!(
                nodes.len() <= 16
                    && usize::from(*diffuse) < nodes.len()
                    && usize::from(*emission) < nodes.len(),
                "invalid GI material program outputs or size"
            );
            for (i, node) in nodes.iter().enumerate() {
                let inputs: &[u8] = match node {
                    GiNode::Constant(v) => {
                        ensure!(
                            v.iter().all(|c| c.is_finite() && *c >= 0.),
                            "invalid GI constant"
                        );
                        &[]
                    }
                    GiNode::Texture(t) => {
                        texture(*t)?;
                        &[]
                    }
                    GiNode::Luminance(n) => std::slice::from_ref(n),
                    GiNode::Multiply(v) | GiNode::Add(v) => v,
                    GiNode::Mix(v) => v,
                };
                ensure!(
                    inputs.iter().all(|&r| usize::from(r) < i),
                    "GI material node {i} references a missing or later node"
                );
            }
        }
        Ok(())
    }
    pub(crate) fn remap_textures(&mut self, lookup: impl Fn(Id) -> Id) {
        let remap = |t: &mut GiTexture| {
            if let GiTexture::Texture(id) = t {
                *id = lookup(*id);
            }
        };
        remap(&mut self.texture);
        if let GiHitModel::Program { nodes, .. } = &mut self.model {
            for node in nodes {
                if let GiNode::Texture(t) = node {
                    remap(t);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_gi_programs_validate_topology_and_texture_references() -> Result<()> {
        let mut world = World::default();
        let tex = world.spawn(Entity {
            texture: Some(Arc::new(Texture {
                width: 1,
                height: 1,
                rgba: vec![255; 4],
            })),
            ..Default::default()
        });
        let material = GiHitMaterial {
            model: GiHitModel::Program {
                nodes: vec![
                    GiNode::Texture(GiTexture::Texture(tex)),
                    GiNode::Constant([0.5; 3]),
                    GiNode::Multiply([0, 1]),
                ],
                diffuse: 2,
                emission: 1,
            },
            ..Default::default()
        };
        material.validate(&world)?;
        let mut restored: GiHitMaterial = serde_json::from_value(serde_json::to_value(&material)?)?;
        assert_eq!(material, restored);
        restored.remap_textures(|_| u64::MAX);
        assert!(restored.validate(&world).is_err());
        let GiHitModel::Program { nodes, .. } = &mut restored.model else {
            unreachable!()
        };
        nodes[0] = GiNode::Luminance(2);
        assert!(restored.validate(&world).is_err());
        let mut legacy = serde_json::to_value(Material::default())?;
        legacy.as_object_mut().unwrap().remove("gi_hit");
        assert_eq!(
            serde_json::from_value::<Material>(legacy)?.gi_hit,
            GiHitMaterial::default()
        );
        Ok(())
    }
}
