//! Immutable, reconstructible CPU products. Live users own memory; cooked bytes survive a restart.
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, Weak},
};
use tempfile::NamedTempFile;

pub struct SharedAsset<T: ?Sized>(Mutex<Option<Weak<T>>>);
impl<T: ?Sized> Default for SharedAsset<T> {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}
impl<T: ?Sized> SharedAsset<T> {
    pub fn acquire(&self, build: impl FnOnce() -> Result<Arc<T>>) -> Result<Arc<T>> {
        let mut weak = self.0.lock().unwrap();
        if let Some(asset) = weak.as_ref().and_then(Weak::upgrade) {
            return Ok(asset);
        }
        let asset = build()?;
        *weak = Some(Arc::downgrade(&asset));
        Ok(asset)
    }
}
pub(crate) fn cache_dir() -> PathBuf {
    std::env::var_os("RENDER_LAB_ASSET_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("render-lab-derived-v1"))
}
pub fn content_key(kind: &str, version: u32, source: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update((kind.len() as u64).to_le_bytes());
    h.update(kind.as_bytes());
    h.update(version.to_le_bytes());
    h.update(source);
    h.finalize().into()
}
pub(crate) fn key_name(key: &[u8; 32]) -> String {
    key.iter().map(|x| format!("{x:02x}")).collect()
}
type Slots = HashMap<[u8; 32], Arc<SharedAsset<[u8]>>>;
static BLOBS: LazyLock<Mutex<Slots>> = LazyLock::new(Default::default);
pub fn cooked_bytes(
    kind: &str,
    version: u32,
    source: &[u8],
    build: impl FnOnce() -> Result<Arc<[u8]>>,
) -> Result<Arc<[u8]>> {
    let key = content_key(kind, version, source);
    let slot = {
        let mut all = BLOBS.lock().unwrap();
        all.retain(|_, slot| {
            Arc::strong_count(slot) > 1
                || slot
                    .0
                    .try_lock()
                    .map_or(true, |w| w.as_ref().is_some_and(|w| w.strong_count() > 0))
        });
        all.entry(key).or_default().clone()
    };
    slot.acquire(|| {
        let dir = cache_dir();
        let path = dir.join(format!("{}.blob", key_name(&key)));
        if let Ok(data) = load_blob(&path, &key) {
            return Ok(data);
        }
        let data = build()?;
        if std::fs::create_dir_all(&dir).is_ok() {
            let save = || -> Result<()> {
                let mut f = NamedTempFile::new_in(&dir)?;
                f.write_all(&key)?;
                f.write_all(&Sha256::digest(&data))?;
                f.write_all(&data)?;
                f.persist(&path)?;
                Ok(())
            };
            let _ = save(); // Memory-only operation also works on read-only installations.
        }
        Ok(data)
    })
}
fn load_blob(path: &Path, key: &[u8; 32]) -> Result<Arc<[u8]>> {
    let mut f = File::open(path)?;
    ensure!(
        (64..=256 * 1024 * 1024 + 64).contains(&f.metadata()?.len()),
        "invalid cooked asset length"
    );
    let mut header = [0; 64];
    f.read_exact(&mut header)?;
    ensure!(&header[..32] == key, "asset key mismatch");
    let mut data = Vec::new();
    f.take(256 * 1024 * 1024 + 1).read_to_end(&mut data)?;
    ensure!(
        data.len() <= 256 * 1024 * 1024 && Sha256::digest(&data)[..] == header[32..],
        "asset checksum mismatch"
    );
    Ok(data.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn releases_last_owner_and_recovers_a_corrupt_derived_product() -> Result<()> {
        let unique = tempfile::NamedTempFile::new()?;
        let source = unique.path().as_os_str().as_encoded_bytes();
        let a = cooked_bytes("asset-test", 1, source, || Ok(Arc::from([1, 2, 3])))?;
        let b = cooked_bytes("asset-test", 1, source, || unreachable!())?;
        assert!(Arc::ptr_eq(&a, &b));
        let weak = Arc::downgrade(&a);
        drop((a, b));
        assert!(weak.upgrade().is_none());
        let key = content_key("asset-test", 1, source);
        let path = cache_dir().join(format!("{}.blob", key_name(&key)));
        let from_disk = cooked_bytes("asset-test", 1, source, || unreachable!())?;
        assert_eq!(&*from_disk, &[1, 2, 3]);
        drop(from_disk);
        std::fs::write(&path, b"truncated")?;
        let rebuilt = cooked_bytes("asset-test", 1, source, || Ok(Arc::from([1, 2, 3])))?;
        assert_eq!(&*rebuilt, &[1, 2, 3]);
        assert_ne!(key, content_key("asset-test", 2, source));
        std::fs::remove_file(path)?;
        Ok(())
    }
}
