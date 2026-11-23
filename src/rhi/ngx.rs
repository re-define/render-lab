use super::*;
use std::ffi::CString;

pub(super) struct Request {
    #[cfg(render_lab_ngx)]
    runtime: Vec<u16>,
    #[cfg(render_lab_ngx)]
    logs: Vec<u16>,
}

#[cfg(not(render_lab_ngx))]
impl Request {
    pub fn from_env() -> Result<Self> {
        anyhow::bail!("build with RENDER_LAB_NGX_SDK to enable NVIDIA NGX")
    }

    pub fn extensions(&self, _: vk::Instance, _: vk::PhysicalDevice) -> Result<Vec<CString>> {
        unreachable!()
    }

    pub fn open(
        self,
        _: vk::PFN_vkGetInstanceProcAddr,
        _: vk::PhysicalDevice,
        _: Arc<device::DeviceOwner>,
    ) -> Result<Context> {
        unreachable!()
    }
}

#[cfg(not(render_lab_ngx))]
pub(super) enum Context {}

#[cfg(not(render_lab_ngx))]
impl Context {
    pub fn evaluate(
        &self,
        _: &vulkanalia::Device,
        _: vk::CommandBuffer,
        _: [(&Image, ImageDesc); 4],
        _: crate::rdg::Dlss,
    ) -> Result<()> {
        match *self {}
    }
}

#[cfg(render_lab_ngx)]
#[path = "ngx_descriptors.rs"]
mod descriptors;

#[cfg(any(render_lab_ngx, test))]
#[path = "ngx_lifecycle.rs"]
mod lifecycle;

#[cfg(render_lab_ngx)]
mod native {
    use super::*;
    use anyhow::{Context as _, ensure};
    use std::{
        ffi::{CStr, c_void},
        os::windows::ffi::OsStrExt,
        path::PathBuf,
        ptr::{NonNull, null_mut},
        sync::Mutex,
    };

    use super::descriptors::VendorDescriptors;
    use super::lifecycle::{ApiCall, Retirement};

    #[repr(C)]
    struct NgxImage {
        image: vk::Image,
        view: vk::ImageView,
        format: u32,
        aspect: u32,
        width: u32,
        height: u32,
    }

    unsafe extern "C" {
        fn lab_ngx_extensions(
            instance: vk::Instance,
            physical: vk::PhysicalDevice,
            runtime: *const u16,
            logs: *const u16,
            count: *mut u32,
            properties: *mut *mut vk::ExtensionProperties,
        ) -> u32;
        fn lab_ngx_open(
            instance: vk::Instance,
            physical: vk::PhysicalDevice,
            device: vk::Device,
            get_instance: vk::PFN_vkGetInstanceProcAddr,
            get_device: vk::PFN_vkGetDeviceProcAddr,
            runtime: *const u16,
            logs: *const u16,
            context: *mut *mut c_void,
        ) -> u32;
        fn lab_ngx_evaluate(
            context: *mut c_void,
            cmd: vk::CommandBuffer,
            images: *const NgxImage,
            hdr: u32,
            reset: u32,
            jitter: *const f32,
        ) -> u32;
        fn lab_ngx_release_feature(context: *mut c_void);
        fn lab_ngx_close(context: *mut c_void);
    }

    fn status(code: u32, operation: &str) -> Result<()> {
        ensure!(code == 1, "NVIDIA NGX {operation} returned 0x{code:08x}");
        Ok(())
    }

    impl Request {
        pub fn from_env() -> Result<Self> {
            let runtime = std::env::var_os("RENDER_LAB_NGX_RUNTIME")
                .map(PathBuf::from)
                .context("set RENDER_LAB_NGX_RUNTIME to the NVSDK feature DLL directory")?;
            let runtime = std::path::absolute(runtime)?;
            ensure!(
                runtime.join("nvngx_dlss.dll").is_file(),
                "NGX runtime directory has no nvngx_dlss.dll"
            );
            // NGX may still write its logs during deferred shutdown. Use one stable
            // per-user cache directory even when Windows reaps a stalled vendor thread.
            let directory = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
                .join("RenderLab")
                .join("NGX");
            let directory = if std::fs::create_dir_all(&directory).is_ok() {
                directory
            } else {
                let exe = std::env::current_exe()?;
                let dir = exe
                    .parent()
                    .context("missing executable directory")?
                    .join("render-lab-ngx");
                std::fs::create_dir_all(&dir)?;
                dir
            };
            let wide = |path: &std::path::Path| path.as_os_str().encode_wide().chain([0]).collect();
            Ok(Self {
                runtime: wide(&runtime),
                logs: wide(&directory),
            })
        }

        pub fn extensions(
            &self,
            instance: vk::Instance,
            physical: vk::PhysicalDevice,
        ) -> Result<Vec<CString>> {
            let _guard = ApiCall::enter()?;
            let mut count = 0;
            let mut data = null_mut();
            unsafe {
                status(
                    lab_ngx_extensions(
                        instance,
                        physical,
                        self.runtime.as_ptr(),
                        self.logs.as_ptr(),
                        &mut count,
                        &mut data,
                    ),
                    "extension discovery",
                )?;
                if count == 0 {
                    return Ok(vec![]);
                }
                ensure!(
                    !data.is_null(),
                    "NGX extension discovery returned no properties"
                );
                Ok(std::slice::from_raw_parts(data, count as usize)
                    .iter()
                    .map(|extension| CStr::from_ptr(extension.extension_name.as_ptr()).to_owned())
                    .collect())
            }
        }

        pub fn open(
            self,
            get_instance: vk::PFN_vkGetInstanceProcAddr,
            physical: vk::PhysicalDevice,
            owner: Arc<device::DeviceOwner>,
        ) -> Result<Context> {
            let _guard = ApiCall::enter()?;
            let instance = owner.instance();
            let device = &owner.dev;
            let descriptors = VendorDescriptors::new(device)?;
            let retirement = Retirement::new()?;
            let mut pointer = null_mut();
            let code = unsafe {
                lab_ngx_open(
                    instance.handle(),
                    physical,
                    device.handle(),
                    get_instance,
                    instance.commands().get_device_proc_addr,
                    self.runtime.as_ptr(),
                    self.logs.as_ptr(),
                    &mut pointer,
                )
            };
            drop(_guard);
            let result = status(code, "initialization or capability query");
            let Some(pointer) = NonNull::new(pointer) else {
                result?;
                anyhow::bail!("NGX returned no context");
            };
            let context = Context {
                state: Mutex::new(State {
                    session: Some(Session {
                        pointer,
                        _request: self,
                        _device: owner,
                    }),
                    config: None,
                }),
                descriptors,
                retirement,
            };
            result?;
            Ok(context)
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Config {
        input: [u32; 2],
        output: [u32; 2],
        hdr: bool,
    }

    struct State {
        session: Option<Session>,
        config: Option<Config>,
    }

    pub(crate) struct Context {
        descriptors: VendorDescriptors,
        state: Mutex<State>,
        retirement: Retirement<Session>,
    }

    struct Session {
        pointer: NonNull<c_void>,
        _request: Request,
        _device: Arc<device::DeviceOwner>,
    }
    // Access to this SDK handle is serialized by ApiCall; retirement owns it exclusively.
    unsafe impl Send for Session {}
    impl super::lifecycle::Close for Session {
        fn close(self) {
            unsafe {
                lab_ngx_close(self.pointer.as_ptr());
            }
        }
    }

    impl Context {
        pub fn evaluate(
            &self,
            device: &vulkanalia::Device,
            cmd: vk::CommandBuffer,
            images: [(&Image, ImageDesc); 4],
            pass: crate::rdg::Dlss,
        ) -> Result<()> {
            let mut state = self.state.lock().unwrap();
            let pointer = state
                .session
                .as_ref()
                .expect("live NGX session")
                .pointer
                .as_ptr();
            let _guard = ApiCall::enter()?;
            let config = Config {
                input: images[0].1.extent,
                output: images[3].1.extent,
                hdr: pass.hdr,
            };
            let changed = state.config != Some(config);
            unsafe {
                if changed && state.config.is_some() {
                    device.device_wait_idle()?;
                    lab_ngx_release_feature(pointer);
                    state.config = None;
                }
                let images = images.map(|(image, desc)| NgxImage {
                    image: image.raw,
                    view: image.view,
                    format: image.format.as_raw() as u32,
                    aspect: image.range.aspect_mask.bits(),
                    width: desc.extent[0],
                    height: desc.extent[1],
                });
                self.descriptors.bind(cmd);
                let code = lab_ngx_evaluate(
                    pointer,
                    cmd,
                    images.as_ptr(),
                    u32::from(pass.hdr),
                    u32::from(changed || pass.reset),
                    pass.jitter.as_ptr(),
                );
                if code != 1 {
                    state.config = None;
                    device.device_wait_idle()?;
                    lab_ngx_release_feature(pointer);
                    status(code, "DLSS evaluation")?;
                }
                state.config = Some(config);
            }
            Ok(())
        }
    }

    impl Drop for Context {
        fn drop(&mut self) {
            if let Some(session) = self.state.get_mut().unwrap().session.take() {
                self.retirement.close(session);
            }
        }
    }
}

#[cfg(render_lab_ngx)]
pub(super) use native::Context;
