use super::*;
/// Shader resource order after the shared visibility-resolve bindings.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum LightingInput {
    Environment,
    EnvironmentControls,
    Direct,
    DirectControls,
    IndirectDiffuse,
}
impl LightingInput {
    pub const DIRECT_DIFFUSE: &'static [Self] =
        &[Self::Direct, Self::DirectControls, Self::IndirectDiffuse];
    pub const IBL: &'static [Self] = &[Self::Environment, Self::EnvironmentControls];
    const MIXED: &'static [Self] = &[
        Self::Environment,
        Self::EnvironmentControls,
        Self::Direct,
        Self::DirectControls,
        Self::IndirectDiffuse,
    ];
}

/// Each entry is a compute kernel with the per-pixel resolve ABI in shaders/resolve.glsl.
/// A frame snapshot retains the registry used to assign its GPU bin indices.
#[derive(Clone)]
pub struct MaterialPrograms {
    pub(super) kernels: Vec<MaterialKernel>,
}

#[derive(Clone)]
pub(super) struct MaterialKernel {
    key: Shading,
    lighting: &'static [LightingInput],
    pub label: &'static str,
    pub shader: &'static [u8],
    pub scene_lit: Option<&'static [u8]>,
}
impl Default for MaterialPrograms {
    fn default() -> Self {
        Self {
            kernels: vec![
                MaterialKernel {
                    key: Shading::Pbr,
                    lighting: &[],
                    label: "PBR pixels",
                    shader: include_bytes!(concat!(env!("OUT_DIR"), "/pbr.comp.spv")),
                    scene_lit: Some(include_bytes!(concat!(
                        env!("OUT_DIR"),
                        "/pbr_scene.comp.spv"
                    ))),
                },
                MaterialKernel {
                    key: Shading::Toon,
                    lighting: &[],
                    label: "Toon pixels",
                    shader: include_bytes!(concat!(env!("OUT_DIR"), "/toon.comp.spv")),
                    scene_lit: Some(include_bytes!(concat!(
                        env!("OUT_DIR"),
                        "/toon_scene.comp.spv"
                    ))),
                },
            ],
        }
    }
}
impl MaterialPrograms {
    pub(crate) fn replace_builtin(&mut self, pbr: &'static [u8], toon: &'static [u8]) {
        for (kernel, code) in self.kernels.iter_mut().zip([pbr, toon]) {
            kernel.shader = code;
            kernel.scene_lit = None;
            kernel.lighting = LightingInput::MIXED;
        }
    }
    /// Returns the stable identifier to store in Material::shading. The GPU bin number
    /// belongs to this registry and is never serialized into the scene.
    pub fn register(&mut self, name: &'static str, spirv: &'static [u8]) -> Result<Shading> {
        self.register_kernel(name, spirv, None, &[])
    }

    pub(crate) fn register_scene_lit(
        &mut self,
        name: &'static str,
        shader: &'static [u8],
        scene_lit: &'static [u8],
    ) -> Result<Shading> {
        self.register_kernel(name, shader, Some(scene_lit), &[])
    }

    pub fn register_lighting(
        &mut self,
        name: &'static str,
        shader: &'static [u8],
        inputs: &'static [LightingInput],
    ) -> Result<Shading> {
        ensure!(
            inputs
                .iter()
                .enumerate()
                .all(|(i, input)| !inputs[..i].contains(input)),
            "duplicate lighting input for {name}"
        );
        self.register_kernel(name, shader, None, inputs)
    }

    pub(crate) fn lighting(&self) -> impl Iterator<Item = (u32, &str, &[LightingInput], bool)> {
        self.kernels
            .iter()
            .enumerate()
            .filter(|(_, kernel)| !kernel.lighting.is_empty())
            .map(|(bin, kernel)| {
                (
                    bin as u32,
                    kernel.label,
                    kernel.lighting,
                    matches!(kernel.key, Shading::Pbr | Shading::Toon),
                )
            })
    }

    fn register_kernel(
        &mut self,
        name: &'static str,
        shader: &'static [u8],
        scene_lit: Option<&'static [u8]>,
        lighting: &'static [LightingInput],
    ) -> Result<Shading> {
        ensure!(
            !name.trim().is_empty() && !self.kernels.iter().any(|k| k.label == name),
            "duplicate or empty shading program name"
        );
        ensure!(
            std::iter::once(shader)
                .chain(scene_lit)
                .all(|spirv| spirv.len() >= 20
                    && spirv.len().is_multiple_of(4)
                    && spirv[..4] == 0x07230203u32.to_le_bytes()),
            "invalid material SPIR-V"
        );
        let id = Shading::Custom(name.into());
        self.kernels.push(MaterialKernel {
            key: id.clone(),
            lighting,
            label: name,
            shader,
            scene_lit,
        });
        Ok(id)
    }
    pub(crate) fn bin(&self, id: &Shading) -> Result<u32> {
        self.kernels
            .iter()
            .position(|k| &k.key == id)
            .ok_or_else(|| anyhow::anyhow!("unregistered material program {id:?}"))?
            .try_into()
            .map_err(Into::into)
    }
}
