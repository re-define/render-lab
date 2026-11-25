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

/// A material explicitly declares the color buffers its SPIR-V can write.
#[derive(Clone, Copy)]
pub struct MaterialCode {
    pub display: &'static [u8],
    pub linear: Option<&'static [u8]>,
}
impl MaterialCode {
    pub(super) fn select(self, linear: bool, label: &str) -> Result<&'static [u8]> {
        if linear {
            self.linear
                .ok_or_else(|| anyhow::anyhow!("{label} has no linear HDR material kernel"))
        } else {
            Ok(self.display)
        }
    }
}
impl From<&'static [u8]> for MaterialCode {
    fn from(display: &'static [u8]) -> Self {
        Self {
            display,
            linear: None,
        }
    }
}
impl<const N: usize> From<&'static [u8; N]> for MaterialCode {
    fn from(bytes: &'static [u8; N]) -> Self {
        bytes.as_slice().into()
    }
}
macro_rules! material_code {
    ($name:literal) => {
        $crate::render::MaterialCode {
            display: include_bytes!(concat!(env!("OUT_DIR"), "/", $name, ".spv")),
            linear: Some(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/",
                $name,
                ".linear.spv"
            ))),
        }
    };
}
pub(crate) use material_code;

#[derive(Clone)]
pub(super) struct MaterialKernel {
    key: Shading,
    lighting: &'static [LightingInput],
    pub label: &'static str,
    pub shader: MaterialCode,
    pub scene_lit: Option<MaterialCode>,
}
impl Default for MaterialPrograms {
    fn default() -> Self {
        Self {
            kernels: vec![
                MaterialKernel {
                    key: Shading::Pbr,
                    lighting: &[],
                    label: "PBR pixels",
                    shader: material_code!("pbr.comp"),
                    scene_lit: Some(material_code!("pbr_scene.comp")),
                },
                MaterialKernel {
                    key: Shading::Toon,
                    lighting: &[],
                    label: "Toon pixels",
                    shader: material_code!("toon.comp"),
                    scene_lit: Some(material_code!("toon_scene.comp")),
                },
            ],
        }
    }
}
impl MaterialPrograms {
    pub(crate) fn replace_builtin(&mut self, pbr: MaterialCode, toon: MaterialCode) {
        for (kernel, code) in self.kernels.iter_mut().zip([pbr, toon]) {
            kernel.shader = code;
            kernel.scene_lit = None;
            kernel.lighting = LightingInput::MIXED;
        }
    }
    /// Returns the stable identifier to store in Material::shading. The GPU bin number
    /// belongs to this registry and is never serialized into the scene.
    pub fn register(
        &mut self,
        name: &'static str,
        spirv: impl Into<MaterialCode>,
    ) -> Result<Shading> {
        self.register_kernel(name, spirv.into(), None, &[])
    }

    pub(crate) fn register_scene_lit(
        &mut self,
        name: &'static str,
        shader: MaterialCode,
        scene_lit: MaterialCode,
    ) -> Result<Shading> {
        self.register_kernel(name, shader, Some(scene_lit), &[])
    }

    pub fn register_lighting(
        &mut self,
        name: &'static str,
        shader: impl Into<MaterialCode>,
        inputs: &'static [LightingInput],
    ) -> Result<Shading> {
        ensure!(
            inputs
                .iter()
                .enumerate()
                .all(|(i, input)| !inputs[..i].contains(input)),
            "duplicate lighting input for {name}"
        );
        self.register_kernel(name, shader.into(), None, inputs)
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
        shader: MaterialCode,
        scene_lit: Option<MaterialCode>,
        lighting: &'static [LightingInput],
    ) -> Result<Shading> {
        ensure!(
            !name.trim().is_empty() && !self.kernels.iter().any(|k| k.label == name),
            "duplicate or empty shading program name"
        );
        ensure!(
            std::iter::once(shader)
                .chain(scene_lit)
                .flat_map(|s| std::iter::once(s.display).chain(s.linear))
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
