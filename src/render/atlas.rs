//! Stable word ranges in an immutable, paged texture payload.
use super::*;
use crate::rdg::rows::Rows;
use anyhow::Context;
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

mod gi;
static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);
pub(crate) struct Atlas {
    hit_table: Arc<Mutex<Option<gi::HitTable>>>,
    pub(super) materials: Arc<Mutex<Option<super::tables::MaterialTable>>>,
    pub(super) locations: BTreeMap<Id, [u32; 3]>,
    pub(super) pixels: Rows,
    pub(super) editable: bool,
    sources: Arc<BTreeMap<Id, Arc<Texture>>>,
    spans: BTreeMap<Id, std::ops::Range<usize>>,
    pub(super) revision: u64,
    pub(super) repacked: usize,
    pub(super) label: &'static str,
}
#[derive(Default)]
pub(crate) struct AtlasCache(Mutex<Option<Arc<Atlas>>>);
impl AtlasCache {
    /// A technique owns one immutable encoding. Texture replacement keeps its ECS identity.
    pub(crate) fn get(
        &self,
        scene: &RenderScene,
        encoded: &BTreeMap<Id, &[u8]>,
        label: &'static str,
    ) -> Result<Arc<Atlas>> {
        let mut cache = self.0.lock().unwrap();
        let old = cache.as_deref();
        if let Some(atlas) = cache
            .as_ref()
            .filter(|a| Arc::ptr_eq(&a.sources, &scene.textures))
        {
            return Ok(atlas.clone());
        }
        let mut spans = BTreeMap::new();
        let mut pending = Vec::new();
        for (&id, texture) in scene.textures.iter() {
            let data = encoded.get(&id).copied().unwrap_or(&texture.rgba);
            ensure!(
                !data.is_empty() && data.len().is_multiple_of(4),
                "invalid material payload"
            );
            let words = data.len() / 4;
            if let Some(range) = old
                .and_then(|a| a.spans.get(&id))
                .filter(|r| words <= r.len())
            {
                spans.insert(id, range.start..range.start + words);
            } else {
                pending.push((id, words));
            }
        }
        // Keep every surviving offset. Reuse vacated intervals before extending the buffer.
        let mut occupied: Vec<_> = spans.values().cloned().collect();
        occupied.sort_unstable_by_key(|r| r.start);
        let mut end = 1;
        let mut holes = Vec::new();
        for range in occupied {
            if end < range.start {
                holes.push(end..range.start);
            }
            end = range.end;
        }
        for (id, words) in pending {
            let start = if let Some(hole) = holes.iter_mut().find(|h| h.len() >= words) {
                let offset = hole.start;
                hole.start += words;
                offset
            } else {
                let offset = end;
                end = end.checked_add(words).context("texture atlas overflow")?;
                offset
            };
            spans.insert(id, start..start + words);
        }
        let mut pixels = old.map_or_else(Rows::words, |a| a.pixels.clone());
        pixels.resize_words(end);
        let mut repacked = pixels.write_words(0, &[255; 4]);
        let mut locations = BTreeMap::new();
        for (&id, t) in scene.textures.iter() {
            let range = &spans[&id];
            locations.insert(id, [range.start.try_into()?, t.width, t.height]);
            if old.is_some_and(|a| {
                a.spans.get(&id) == Some(range)
                    && a.sources.get(&id).is_some_and(|p| Arc::ptr_eq(p, t))
            }) {
                continue;
            }
            repacked +=
                pixels.write_words(range.start, encoded.get(&id).copied().unwrap_or(&t.rgba));
        }
        let same_layout = old.filter(|a| a.locations == locations);
        let revision = if let Some(a) = same_layout.filter(|a| a.pixels.identical(&pixels)) {
            a.revision
        } else {
            NEXT_REVISION.fetch_add(1, Ordering::Relaxed)
        };
        let atlas = Arc::new(Atlas {
            hit_table: same_layout.map(|a| a.hit_table.clone()).unwrap_or_default(),
            materials: same_layout.map(|a| a.materials.clone()).unwrap_or_default(),
            repacked,
            sources: scene.textures.clone(),
            locations,
            pixels,
            spans,
            revision,
            editable: encoded.is_empty(),
            label: if encoded.is_empty() {
                "Base color texels"
            } else {
                label
            },
        });
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
}

#[cfg(test)]
mod tests;
