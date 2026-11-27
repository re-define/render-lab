use super::Level;
use anyhow::{Context, Result};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
};
#[derive(Clone)]
pub(crate) enum Upload {
    Host(Arc<[u8]>),
    #[cfg(lab_directstorage)]
    Direct(Arc<super::directstorage::Page>),
}
impl Upload {
    pub fn bytes(&self) -> usize {
        match self {
            Self::Host(data) => data.len(),
            #[cfg(lab_directstorage)]
            Self::Direct(page) => page.bytes,
        }
    }
}
pub(crate) struct Reader {
    file: Option<File>,
    #[cfg(lab_directstorage)]
    direct: Option<super::directstorage::Reader>,
}
impl Reader {
    pub fn new(_path: &Path, _adapter: Option<[u8; 8]>) -> Self {
        #[cfg(lab_directstorage)]
        let direct = _adapter.and_then(|id| match super::directstorage::Reader::open(_path, id) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("DirectStorage unavailable: {e:#}");
                None
            }
        });
        Self {
            file: None,
            #[cfg(lab_directstorage)]
            direct,
        }
    }
    pub fn read(&mut self, path: &Path, range: Level, _direct: bool) -> Result<Upload> {
        #[cfg(lab_directstorage)]
        if _direct && let Some(r) = &mut self.direct {
            match r.read(range) {
                Ok(page) => return Ok(Upload::Direct(Arc::new(page))),
                Err(e) => {
                    eprintln!("DirectStorage page read failed, using file staging: {e:#}");
                    self.direct = None;
                }
            }
        }
        if self.file.is_none() {
            self.file = Some(File::open(path).context("open streaming package")?)
        }
        let file = self.file.as_mut().unwrap();
        file.seek(SeekFrom::Start(range.offset))?;
        let mut data = vec![0; range.bytes];
        file.read_exact(&mut data)?;
        Ok(Upload::Host(data.into()))
    }
}
