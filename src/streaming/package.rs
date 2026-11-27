use super::PAGE_BYTES;
use anyhow::{Result, ensure};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
/// Level 0 is pinned. Other levels are immutable aligned file ranges, coarse to fine.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Part {
    pub root: Arc<[u8]>,
    pub levels: Vec<Level>,
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Level {
    pub offset: u64,
    pub bytes: usize,
}
pub struct Package {
    pub path: PathBuf,
    pub parts: Vec<Part>,
}
impl Package {
    pub fn open(path: &Path) -> Result<Self> {
        let parts: Vec<Part> = serde_json::from_reader(std::io::BufReader::new(
            std::fs::File::open(path.with_extension("index"))?.take(4 << 20),
        ))?;
        let length = path.metadata()?.len();
        ensure!((1..=8).contains(&parts.len()), "invalid stream index");
        for p in &parts {
            ensure!(
                !p.root.is_empty()
                    && p.root.len() <= PAGE_BYTES
                    && p.root.len().is_multiple_of(4)
                    && p.levels.len() <= 4,
                "invalid stream root"
            );
            for l in &p.levels {
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
            (1..=8).contains(&parts.len()),
            "expected one to eight stream parts"
        );
        for levels in parts {
            ensure!(
                (1..=5).contains(&levels.len()),
                "expected a root and up to four detail levels"
            );
            let mut iter = levels.into_iter();
            let root = iter
                .next()
                .ok_or_else(|| anyhow::anyhow!("missing streaming root"))?;
            ensure!(
                !root.is_empty() && root.len() <= PAGE_BYTES && root.len().is_multiple_of(4),
                "invalid root page"
            );
            let mut ranges = Vec::new();
            for bytes in iter {
                ensure!(
                    !bytes.is_empty() && bytes.len().is_multiple_of(4),
                    "invalid stream level"
                );
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
            });
        }
        let mut index = tempfile::NamedTempFile::new_in(dir)?;
        serde_json::to_writer(&mut index, &manifest)?;
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
