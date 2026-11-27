//! FFI ownership for DirectStorage and Windows external-memory imports.
use anyhow::{Context, Result, ensure};
use std::{
    ffi::c_void,
    os::windows::ffi::OsStrExt,
    path::Path,
    ptr::NonNull,
    sync::{Arc, Mutex, Weak},
};
use vulkanalia::vk::{self, Handle};
unsafe extern "C" {
    fn lab_storage_budget(adapter: u64, limit: u64) -> u64;
    fn lab_storage_open(dll: *const u16, file: *const u16, luid: u64, out: *mut *mut c_void)
    -> i32;
    fn lab_storage_submit(
        reader: *mut c_void,
        offsets: *const u64,
        sizes: *const u32,
        count: u32,
        out: *mut *mut c_void,
        point: *mut u64,
        capacity: *mut u64,
    ) -> i32;
    fn lab_storage_poll(batch: *mut c_void) -> i32;
    fn lab_storage_trim(reader: *mut c_void);
    fn lab_storage_close(reader: *mut c_void);
    fn lab_storage_drop(page: *mut c_void);
    fn lab_storage_import(
        page: *mut c_void,
        device: u64,
        out: *mut *mut c_void,
        buffer: *mut u64,
        sem: *mut u64,
    ) -> i32;
    fn lab_storage_unimport(import: *mut c_void);
}
pub(crate) fn staging_budget(adapter: [u8; 8], limit: u64) -> u64 {
    unsafe { lab_storage_budget(u64::from_ne_bytes(adapter), limit) }
}
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}
pub(super) struct Reader(NonNull<c_void>);
impl Reader {
    pub fn open(path: &Path, luid: [u8; 8]) -> Result<Self> {
        let dll = std::env::current_exe()?.with_file_name("dstorage.dll");
        let mut p = std::ptr::null_mut();
        let rc = unsafe {
            lab_storage_open(
                wide(&dll.canonicalize()?).as_ptr(),
                wide(&path.canonicalize()?).as_ptr(),
                u64::from_ne_bytes(luid),
                &mut p,
            )
        };
        ensure!(rc >= 0, "DirectStorage initialization HRESULT {rc:#x}");
        Ok(Self(NonNull::new(p).context("empty storage reader")?))
    }
    pub fn submit(&mut self, ranges: &[super::Level]) -> Result<Arc<Batch>> {
        let offsets: Vec<_> = ranges.iter().map(|r| r.offset).collect();
        let sizes: Vec<u32> = ranges
            .iter()
            .map(|r| r.bytes.try_into())
            .collect::<Result<_, _>>()?;
        let (mut p, mut point, mut capacity) = (std::ptr::null_mut(), 0, 0);
        let rc = unsafe {
            lab_storage_submit(
                self.0.as_ptr(),
                offsets.as_ptr(),
                sizes.as_ptr(),
                offsets.len().try_into()?,
                &mut p,
                &mut point,
                &mut capacity,
            )
        };
        ensure!(rc >= 0, "DirectStorage batch submission HRESULT {rc:#x}");
        Ok(Arc::new(Batch {
            handle: NonNull::new(p).context("empty storage batch")?,
            point,
            capacity,
            imported: Mutex::default(),
        }))
    }
    pub fn trim(&mut self) {
        unsafe { lab_storage_trim(self.0.as_ptr()) }
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        unsafe { lab_storage_close(self.0.as_ptr()) }
    }
}
pub(crate) struct Batch {
    handle: NonNull<c_void>,
    point: u64,
    capacity: u64,
    imported: Mutex<Option<(vk::Device, Weak<SharedImport>)>>,
}
// DirectStorage keeps the batch alive through its completion. The RHI imports immutable shared handles.
unsafe impl Send for Batch {}
unsafe impl Sync for Batch {}
impl Batch {
    pub fn complete(&self) -> Result<bool> {
        let rc = unsafe { lab_storage_poll(self.handle.as_ptr()) };
        ensure!(rc >= 0, "DirectStorage batch completion HRESULT {rc:#x}");
        Ok(rc == 0)
    }
}
impl Drop for Batch {
    fn drop(&mut self) {
        unsafe { lab_storage_drop(self.handle.as_ptr()) }
    }
}
pub(crate) struct Page {
    pub batch: Arc<Batch>,
    pub offset: u64,
    pub bytes: usize,
}
struct SharedImport {
    handle: NonNull<c_void>,
    buffer: vk::Buffer,
    semaphore: vk::Semaphore,
}
// Concurrent command recorders only borrow the handles. Destruction follows slot retirement.
unsafe impl Send for SharedImport {}
unsafe impl Sync for SharedImport {}
pub(crate) struct Imported {
    _resource: Arc<SharedImport>,
    _batch: Arc<Batch>,
    pub buffer: vk::Buffer,
    pub semaphore: vk::Semaphore,
    pub point: u64,
    pub offset: u64,
    pub capacity: u64,
}
impl Page {
    pub fn import(&self, device: vk::Device) -> Result<Imported> {
        let mut cache = self.batch.imported.lock().unwrap();
        let resource = if let Some((d, w)) = &*cache
            && *d == device
            && let Some(imp) = w.upgrade()
        {
            imp
        } else {
            let v = Arc::new(self.batch.import(device)?);
            *cache = Some((device, Arc::downgrade(&v)));
            v
        };
        Ok(Imported {
            buffer: resource.buffer,
            semaphore: resource.semaphore,
            point: self.batch.point,
            offset: self.offset,
            capacity: self.batch.capacity,
            _batch: self.batch.clone(),
            _resource: resource,
        })
    }
}
impl Batch {
    fn import(&self, device: vk::Device) -> Result<SharedImport> {
        let (mut handle, mut buf, mut sem) = (std::ptr::null_mut(), 0, 0);
        let rc = unsafe {
            lab_storage_import(
                self.handle.as_ptr(),
                device.as_raw() as u64,
                &mut handle,
                &mut buf,
                &mut sem,
            )
        };
        ensure!(
            rc == 0,
            "Vulkan import of DirectStorage buffer or fence failed ({rc})"
        );
        Ok(SharedImport {
            handle: NonNull::new(handle).context("empty storage import")?,
            buffer: vk::Buffer::from_raw(buf),
            semaphore: vk::Semaphore::from_raw(sem),
        })
    }
}
impl Drop for SharedImport {
    fn drop(&mut self) {
        unsafe { lab_storage_unimport(self.handle.as_ptr()) }
    }
}
