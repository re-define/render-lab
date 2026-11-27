//! Repeated saves reuse the content name of each unchanged cell payload.
use super::*;
use std::sync::{Mutex, Weak};
#[derive(Copy, Clone, PartialOrd, Ord, PartialEq, Eq)]
pub(super) enum Owner {
    Cell(Id),
    Mobile(Id),
}
#[derive(Default)]
pub(super) struct Cache {
    files: BTreeMap<Owner, (Revision, PathBuf)>,
    #[cfg(test)]
    pub writes: usize,
}
enum Revision {
    Source(PathBuf, u64, std::time::SystemTime),
    Rows(Weak<Vec<Actor>>),
    File(Weak<tempfile::TempPath>),
}
impl Revision {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Rows(a), Self::Rows(b)) => a.ptr_eq(b),
            (Self::File(a), Self::File(b)) => a.ptr_eq(b),
            (Self::Source(a, b, c), Self::Source(x, y, z)) => a == x && b == y && c == z,
            _ => false,
        }
    }
}
pub(super) fn export(
    cache: &Mutex<Cache>,
    owner: Owner,
    dir: &Path,
    base: &Path,
    file: Option<&Path>,
    checkpoint: Option<&Checkpoint>,
) -> Result<PathBuf> {
    let revision = match checkpoint {
        Some(Checkpoint::Memory(v)) => Revision::Rows(Arc::downgrade(v)),
        Some(Checkpoint::File(v, _)) => Revision::File(Arc::downgrade(v)),
        None => {
            let source = base.join(file.context("missing cell payload")?);
            let meta = source.metadata()?;
            Revision::Source(source, meta.len(), meta.modified()?)
        }
    };
    if let Some((previous, name)) = cache.lock().unwrap().files.get(&owner)
        && previous.same(&revision)
        && dir.join(name).is_file()
    {
        return Ok(name.clone());
    }
    let bytes = match checkpoint {
        Some(checkpoint) => checkpoint.bytes()?,
        None => read_bounded(&base.join(file.unwrap()))?,
    };
    ensure!(
        bytes.len() as u64 <= FILE_BYTES,
        "cell payload exceeds archive limit"
    );
    let file = cell::publish(dir, &bytes)?;
    let mut saved = cache.lock().unwrap();
    saved.files.insert(owner, (revision, file.clone()));
    #[cfg(test)]
    {
        saved.writes += 1;
    }
    Ok(file)
}
