use super::*;
/// Each entry is a compute kernel with the per-pixel resolve ABI in shaders/resolve.glsl.
/// A frame snapshot retains the registry used to assign its GPU bin indices.
#[derive(Clone)]
pub struct MaterialPrograms {
    pub(super) kernels: Vec<(Shading, &'static str, &'static [u8])>,
}
impl Default for MaterialPrograms {
    fn default() -> Self {
        Self {
            kernels: vec![
                (
                    Shading::Pbr,
                    "PBR pixels",
                    include_bytes!(concat!(env!("OUT_DIR"), "/pbr.comp.spv")),
                ),
                (
                    Shading::Toon,
                    "Toon pixels",
                    include_bytes!(concat!(env!("OUT_DIR"), "/toon.comp.spv")),
                ),
            ],
        }
    }
}
impl MaterialPrograms {
    /// Returns the stable identifier to store in Material::shading. The GPU bin number
    /// belongs to this registry and is never serialized into the scene.
    pub fn register(&mut self, name: &'static str, spirv: &'static [u8]) -> Result<Shading> {
        ensure!(
            !name.trim().is_empty() && !self.kernels.iter().any(|(_, label, _)| *label == name),
            "duplicate or empty shading program name"
        );
        ensure!(
            spirv.len() >= 20
                && spirv.len().is_multiple_of(4)
                && spirv[..4] == 0x07230203u32.to_le_bytes(),
            "invalid material SPIR-V"
        );
        let id = Shading::Custom(name.into());
        self.kernels.push((id.clone(), name, spirv));
        Ok(id)
    }
    pub(crate) fn bin(&self, id: &Shading) -> Result<u32> {
        self.kernels
            .iter()
            .position(|(key, _, _)| key == id)
            .ok_or_else(|| anyhow::anyhow!("unregistered material program {id:?}"))?
            .try_into()
            .map_err(Into::into)
    }
}
