//! FFI ownership for DirectStorage and Windows external-memory imports.
use anyhow::{Context, Result, ensure};
use std::{ffi::c_void, os::windows::ffi::OsStrExt, path::Path, ptr::NonNull};
use vulkanalia::vk::{self, Handle};
unsafe extern "C" {
    fn lab_storage_open(dll: *const u16, file: *const u16, luid: u64, out: *mut *mut c_void)
    -> i32;
    fn lab_storage_read(reader: *mut c_void, offset: u64, bytes: u32, out: *mut *mut c_void)
    -> i32;
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
    pub fn read(&mut self, range: super::Level) -> Result<Page> {
        let mut p = std::ptr::null_mut();
        let rc = unsafe {
            lab_storage_read(
                self.0.as_ptr(),
                range.offset,
                range.bytes.try_into()?,
                &mut p,
            )
        };
        ensure!(rc >= 0, "DirectStorage read HRESULT {rc:#x}");
        Ok(Page {
            handle: NonNull::new(p).context("empty storage page")?,
            bytes: range.bytes,
        })
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        unsafe { lab_storage_close(self.0.as_ptr()) }
    }
}
pub(crate) struct Page {
    handle: NonNull<c_void>,
    pub bytes: usize,
}
// The worker publishes only completed pages. Imports borrow immutable shared HANDLEs.
unsafe impl Send for Page {}
unsafe impl Sync for Page {}
impl Drop for Page {
    fn drop(&mut self) {
        unsafe { lab_storage_drop(self.handle.as_ptr()) }
    }
}
pub(crate) struct Imported {
    handle: NonNull<c_void>,
    pub buffer: vk::Buffer,
    pub semaphore: vk::Semaphore,
}
// Recorders share immutable Vulkan handles. The RHI destroys the import after queue retirement.
unsafe impl Send for Imported {}
unsafe impl Sync for Imported {}
impl Page {
    pub fn import(&self, device: vk::Device) -> Result<Imported> {
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
        Ok(Imported {
            handle: NonNull::new(handle).context("empty storage import")?,
            buffer: vk::Buffer::from_raw(buf),
            semaphore: vk::Semaphore::from_raw(sem),
        })
    }
}
impl Drop for Imported {
    fn drop(&mut self) {
        unsafe { lab_storage_unimport(self.handle.as_ptr()) }
    }
}
