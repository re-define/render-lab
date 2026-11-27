use super::{Key, ReadJob, Staged};
use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
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
#[derive(Default)]
struct Reader {
    file: Option<File>,
    #[cfg(lab_directstorage)]
    direct: Option<super::directstorage::Reader>,
    #[cfg(lab_directstorage)]
    attempted: bool,
}
type Output = mpsc::Sender<(Key, Result<Staged, String>)>;

impl Reader {
    fn read_file(&mut self, path: &Path, jobs: Vec<ReadJob>, send: &Output) {
        let mut work = jobs.into_iter().peekable();
        while let Some(first) = work.next() {
            let offset = first.1.offset;
            let mut total = first.1.bytes;
            let mut run = vec![first];
            while work
                .peek()
                .is_some_and(|next| offset.checked_add(total as u64) == Some(next.1.offset))
            {
                let next = work.next().unwrap();
                total += next.1.bytes;
                run.push(next);
            }
            let read = (|| -> Result<Vec<u8>> {
                if self.file.is_none() {
                    self.file = Some(File::open(path).context("open streaming package")?);
                }
                let file = self.file.as_mut().unwrap();
                file.seek(SeekFrom::Start(offset))?;
                let mut data = vec![0; total];
                file.read_exact(&mut data)?;
                Ok(data)
            })();
            let mut start = 0;
            for (key, r, _, permit) in run {
                let end = start + r.bytes;
                let result = read
                    .as_ref()
                    .map(|v| Staged {
                        upload: Upload::Host(Arc::from(&v[start..end])),
                        permit,
                    })
                    .map_err(|e| format!("{e:#}"));
                let _ = send.send((key, result));
                start = end;
            }
        }
    }
}
#[cfg(lab_directstorage)]
struct Pending {
    batch: Arc<super::directstorage::Batch>,
    jobs: Vec<ReadJob>,
    file: usize,
}

/// A primary frame posts one bounded group. Native batches stay in flight while more arrive.
pub(super) fn run(
    paths: Vec<PathBuf>,
    files: Vec<usize>,
    work: mpsc::Receiver<Vec<ReadJob>>,
    send: Output,
    stop: Arc<AtomicBool>,
) {
    let mut readers: Vec<_> = (0..paths.len()).map(|_| Reader::default()).collect();
    #[cfg(lab_directstorage)]
    let mut pending = Vec::<Pending>::new();
    #[cfg(lab_directstorage)]
    let mut trim = false;
    loop {
        let stopping = stop.load(Ordering::Acquire);
        #[cfg(lab_directstorage)]
        {
            let mut i = 0;
            while i < pending.len() {
                match pending[i].batch.complete() {
                    Ok(false) => {
                        i += 1;
                        continue;
                    }
                    Ok(true) => {
                        let p = pending.swap_remove(i);
                        if !stopping {
                            for (idx, (key, r, _, permit)) in p.jobs.into_iter().enumerate() {
                                let page = super::directstorage::Page {
                                    batch: p.batch.clone(),
                                    offset: (idx * super::PAGE_BYTES) as u64,
                                    bytes: r.bytes,
                                };
                                let _ = send.send((
                                    key,
                                    Ok(Staged {
                                        upload: Upload::Direct(Arc::new(page)),
                                        permit,
                                    }),
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        let p = pending.swap_remove(i);
                        eprintln!("DirectStorage batch failed, using file staging: {e:#}");
                        if !stopping {
                            readers[p.file].read_file(&paths[p.file], p.jobs, &send);
                        }
                    }
                }
            }
            if stopping && !pending.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
        }
        if stopping {
            break;
        }
        #[cfg(lab_directstorage)]
        let frame = if !pending.is_empty() || trim {
            let active = !pending.is_empty();
            match work.recv_timeout(std::time::Duration::from_millis(if active {
                1
            } else {
                100
            })) {
                Ok(j) => Some(j),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if !active {
                        for reader in &mut readers {
                            if let Some(r) = &mut reader.direct {
                                r.trim();
                            }
                        }
                        trim = false;
                    }
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if active {
                        stop.store(true, Ordering::Release);
                        continue;
                    }
                    None
                }
            }
        } else {
            work.recv().ok()
        };
        #[cfg(not(lab_directstorage))]
        let frame = work.recv().ok();
        let Some(jobs) = frame else { break };
        let mut groups = BTreeMap::<_, Vec<ReadJob>>::new();
        for job in jobs {
            groups.entry((files[job.0.0], job.2)).or_default().push(job);
        }
        for ((file, _adapter), mut group) in groups {
            group.sort_by_key(|job| job.1.offset);
            let mut iter = group.into_iter();
            loop {
                let jobs: Vec<_> = iter.by_ref().take(64).collect();
                if jobs.is_empty() {
                    break;
                }
                let reader = &mut readers[file];
                #[cfg(lab_directstorage)]
                if let Some(adapter) = _adapter {
                    if !reader.attempted {
                        reader.attempted = true;
                        match super::directstorage::Reader::open(&paths[file], adapter) {
                            Ok(r) => reader.direct = Some(r),
                            Err(e) => eprintln!("DirectStorage unavailable: {e:#}"),
                        }
                    }
                    if let Some(direct) = &mut reader.direct {
                        let ranges: Vec<_> = jobs.iter().map(|j| j.1).collect();
                        match direct.submit(&ranges) {
                            Ok(batch) => {
                                pending.push(Pending { batch, jobs, file });
                                trim = true;
                                continue;
                            }
                            Err(e) => {
                                eprintln!("DirectStorage submit failed, using file staging: {e:#}");
                            }
                        }
                    }
                }
                reader.read_file(&paths[file], jobs, &send);
            }
        }
    }
}
