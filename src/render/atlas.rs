//! Word-aligned texture payloads and per-scene or per-technique residency keys.
use super::*;

mod gi;
pub(crate) struct Atlas {
    hit_table: std::sync::Mutex<Option<gi::HitTable>>,
    pub(super) materials: std::sync::Mutex<Option<super::tables::MaterialTable>>,
    pub(super) locations: BTreeMap<Id, [u32; 3]>,
    pub(super) pixels: Arc<[u8]>,
    sources: Vec<(Id, Arc<Texture>)>,
    pub(super) label: &'static str,
}
#[derive(Default)]
pub(crate) struct AtlasCache(std::sync::Mutex<Option<Arc<Atlas>>>);
impl AtlasCache {
    /// Encoded payloads belong to the owning technique. Changing the encoding
    /// creates a new technique and cache; scene texture edits invalidate the entry here.
    pub(crate) fn get(
        &self,
        scene: &RenderScene,
        encoded: &BTreeMap<Id, &[u8]>,
        label: &'static str,
    ) -> Result<Arc<Atlas>> {
        let mut cache = self.0.lock().unwrap();
        if let Some(atlas) = cache.as_ref().filter(|atlas| atlas.matches(scene)) {
            return Ok(atlas.clone());
        }
        let atlas = Arc::new(Atlas::with_payloads(scene, encoded, label)?);
        *cache = Some(atlas.clone());
        Ok(atlas)
    }
}
impl Atlas {
    pub(crate) fn cached(scene: &RenderScene) -> Result<Arc<Self>> {
        scene
            .atlas_cache
            .get(scene, &BTreeMap::new(), "Base color texels")
    }
    fn matches(&self, scene: &RenderScene) -> bool {
        self.sources
            .iter()
            .map(|(id, t)| (*id, Arc::as_ptr(t)))
            .eq(scene
                .textures
                .iter()
                .map(|(&id, texture)| (id, Arc::as_ptr(texture))))
    }
    /// Material kernels can sample their own word-aligned representation from the same atlas.
    fn with_payloads(
        scene: &RenderScene,
        encoded: &BTreeMap<Id, &[u8]>,
        label: &'static str,
    ) -> Result<Self> {
        let mut locations = BTreeMap::new();
        let mut pixels = vec![255; 4];
        let mut sources = vec![];
        for (&id, t) in scene.textures.iter() {
            locations.insert(id, [(pixels.len() / 4).try_into()?, t.width, t.height]);
            let data = encoded.get(&id).copied().unwrap_or(&t.rgba);
            ensure!(data.len().is_multiple_of(4), "unaligned material payload");
            pixels.extend_from_slice(data);
            sources.push((id, t.clone()));
        }
        Ok(Self {
            hit_table: Default::default(),
            materials: Default::default(),
            locations,
            pixels: pixels.into(),
            sources,
            label: if encoded.is_empty() {
                "Base color texels"
            } else {
                label
            },
        })
    }
}
