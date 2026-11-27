use super::PAGE_BYTES;
use anyhow::{Result, ensure};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
/// Level 0 is pinned. Other levels are immutable aligned file ranges, coarse to fine.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Part {
    pub root: Arc<[u8]>,
    pub levels: Vec<Level>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    inline: Vec<Option<usize>>,
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Level {
    pub offset: u64,
    pub bytes: usize,
}
impl Part {
    pub(super) fn root_offset(&self, level: usize) -> Option<usize> {
        match level {
            0 => Some(0),
            n => self.inline.get(n - 1).copied().flatten(),
        }
    }
}
const INDEX_BYTES: u64 = 64 * 1024 * 1024;
pub struct Package {
    pub path: PathBuf,
    pub parts: Vec<Part>,
}
impl Package {
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path.with_extension("index"))?;
        ensure!(
            file.metadata()?.len() <= INDEX_BYTES,
            "oversized streaming index"
        );
        let parts: Vec<Part> =
            serde_json::from_reader(std::io::BufReader::new(file.take(INDEX_BYTES)))?;
        let length = path.metadata()?.len();
        ensure!(
            (1..=super::feedback::MAX_UNITS).contains(&parts.len()),
            "invalid stream index"
        );
        for p in &parts {
            ensure!(
                !p.root.is_empty()
                    && p.root.len() <= PAGE_BYTES
                    && p.root.len().is_multiple_of(4)
                    && p.levels.len() <= u8::MAX as usize,
                "invalid stream root"
            );
            ensure!(
                p.inline.is_empty() || p.inline.len() == p.levels.len(),
                "invalid inline level table"
            );
            for (i, l) in p.levels.iter().enumerate() {
                if let Some(word) = p.root_offset(i + 1) {
                    ensure!(
                        l.bytes > 0
                            && l.bytes.is_multiple_of(4)
                            && word
                                .checked_mul(4)
                                .and_then(|start| start.checked_add(l.bytes))
                                .is_some_and(|end| end <= p.root.len()),
                        "invalid inline payload"
                    );
                    continue;
                }
                ensure!(
                    l.offset.is_multiple_of(PAGE_BYTES as u64)
                        && l.bytes > 0
                        && l.bytes.is_multiple_of(4)
                        && l.offset
                            .checked_add(l.bytes as u64)
                            .is_some_and(|end| end <= length),
                    "invalid stream range"
                );
            }
        }
        Ok(Self {
            path: path.to_owned(),
            parts,
        })
    }
    /// Cook into a sibling temporary file before publishing the immutable package.
    pub fn cook(path: &Path, parts: Vec<Vec<Vec<u8>>>) -> Result<Self> {
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let mut out = tempfile::NamedTempFile::new_in(dir)?;
        let mut offset = 0;
        let mut manifest = Vec::new();
        ensure!(
            (1..=super::feedback::MAX_UNITS).contains(&parts.len()),
            "invalid number of streaming units"
        );
        for levels in parts {
            ensure!(
                (1..=256).contains(&levels.len()),
                "expected a root and up to 255 detail levels"
            );
            let mut iter = levels.into_iter();
            let mut root = iter
                .next()
                .ok_or_else(|| anyhow::anyhow!("missing streaming root"))?;
            ensure!(
                !root.is_empty() && root.len() <= PAGE_BYTES && root.len().is_multiple_of(4),
                "invalid root page"
            );
            let mut ranges = Vec::new();
            let mut inline = Vec::new();
            for bytes in iter {
                ensure!(
                    !bytes.is_empty() && bytes.len().is_multiple_of(4),
                    "invalid stream level"
                );
                if bytes.len() <= 256 && root.len() + bytes.len() <= PAGE_BYTES {
                    inline.push(Some(root.len() / 4));
                    ranges.push(Level {
                        offset: 0,
                        bytes: bytes.len(),
                    });
                    root.extend(bytes);
                    continue;
                }
                inline.push(None);
                ranges.push(Level {
                    offset,
                    bytes: bytes.len(),
                });
                out.write_all(&bytes)?;
                offset += (bytes.len().div_ceil(PAGE_BYTES) * PAGE_BYTES) as u64;
                out.as_file_mut().set_len(offset)?;
                out.seek(SeekFrom::Start(offset))?;
            }
            manifest.push(Part {
                root: root.into(),
                levels: ranges,
                inline,
            });
        }
        let mut index = tempfile::NamedTempFile::new_in(dir)?;
        serde_json::to_writer(&mut index, &manifest)?;
        ensure!(
            index.as_file().metadata()?.len() <= INDEX_BYTES,
            "oversized streaming index"
        );
        publish_immutable(out, path)?;
        publish_immutable(index, &path.with_extension("index"))?;
        Ok(Self {
            path: path.to_owned(),
            parts: manifest,
        })
    }
}
fn publish_immutable(file: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    if let Err(mut e) = file.persist_noclobber(path) {
        if e.error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(e.into());
        }
        let hash = |f: &mut std::fs::File| -> Result<_> {
            use sha2::{Digest, Sha256};
            f.rewind()?;
            let mut digest = Sha256::new();
            std::io::copy(f, &mut digest)?;
            Ok(digest.finalize())
        };
        ensure!(
            hash(e.file.as_file_mut())? == hash(&mut std::fs::File::open(path)?)?,
            "a different stream package already owns this filename"
        );
    }
    Ok(())
}
