use super::*;
use vk::{AccessFlags2 as A, ImageLayout as L, PipelineStageFlags2 as Stage};
use vk::{
    KhrSurfaceExtensionInstanceCommands, KhrSwapchainExtensionDeviceCommands,
    KhrWin32SurfaceExtensionInstanceCommands,
};

#[derive(Clone, Copy)]
pub(super) struct Acquired {
    image: vk::Image,
    index: u32,
    pub available: vk::Semaphore,
    pub finished: vk::Semaphore,
}
impl Acquired {
    pub unsafe fn copy(
        self,
        d: &vulkanalia::Device,
        cmd: vk::CommandBuffer,
        src: vk::Buffer,
        [width, height]: [u32; 2],
    ) {
        let range = vk::ImageSubresourceRange::builder()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);
        let barrier = |old, new, src_stage, dst_stage, src_access, dst_access| {
            let image = vk::ImageMemoryBarrier2::builder()
                .image(self.image)
                .subresource_range(*range)
                .old_layout(old)
                .new_layout(new)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .src_stage_mask(src_stage)
                .dst_stage_mask(dst_stage)
                .src_access_mask(src_access)
                .dst_access_mask(dst_access)
                .build();
            d.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::builder().image_memory_barriers(&[image]),
            );
        };
        barrier(
            L::UNDEFINED,
            L::TRANSFER_DST_OPTIMAL,
            Stage::empty(),
            Stage::COPY,
            A::empty(),
            A::TRANSFER_WRITE,
        );
        let region = vk::BufferImageCopy::builder()
            .image_subresource(
                vk::ImageSubresourceLayers::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .layer_count(1),
            )
            .image_extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .build();
        d.cmd_copy_buffer_to_image(cmd, src, self.image, L::TRANSFER_DST_OPTIMAL, &[region]);
        barrier(
            L::TRANSFER_DST_OPTIMAL,
            L::PRESENT_SRC_KHR,
            Stage::COPY,
            Stage::empty(),
            A::TRANSFER_WRITE,
            A::empty(),
        );
    }
}
pub(super) struct Surface {
    gpu: Arc<Gpu>,
    pub window: isize,
    surface: vk::SurfaceKHR,
    chain: vk::SwapchainKHR,
    images: Vec<vk::Image>,
    finished: Vec<vk::Semaphore>,
    available: vk::Semaphore,
    extent: [u32; 2],
    pub mode: vk::PresentModeKHR,
    dirty: bool,
}
impl Surface {
    pub fn new(g: &Arc<Gpu>, window: isize) -> Result<Self> {
        unsafe {
            let info = vk::Win32SurfaceCreateInfoKHR {
                hwnd: window as _,
                hinstance: windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(
                    std::ptr::null(),
                ) as _,
                ..Default::default()
            };
            let surface = g.instance().create_win32_surface_khr(&info, None)?;
            let mut s = Self {
                gpu: g.clone(),
                window,
                surface,
                chain: vk::SwapchainKHR::null(),
                images: vec![],
                finished: vec![],
                available: vk::Semaphore::null(),
                extent: [0; 2],
                mode: vk::PresentModeKHR::FIFO,
                dirty: true,
            };
            ensure!(
                g.instance()
                    .get_physical_device_surface_support_khr(g.physical, g.family, surface)?,
                "queue family cannot present to viewport"
            );
            s.available = g
                .dev
                .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?;
            Ok(s)
        }
    }
    fn recreate(&mut self, extent: [u32; 2]) -> Result<bool> {
        unsafe {
            let g = &self.gpu;
            let ins = g.instance();
            let caps =
                ins.get_physical_device_surface_capabilities_khr(g.physical, self.surface)?;
            let actual = if caps.current_extent.width == u32::MAX {
                vk::Extent2D {
                    width: extent[0]
                        .clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                    height: extent[1]
                        .clamp(caps.min_image_extent.height, caps.max_image_extent.height),
                }
            } else {
                caps.current_extent
            };
            if [actual.width, actual.height] != extent {
                return Ok(false);
            }
            ensure!(
                caps.supported_usage_flags
                    .contains(vk::ImageUsageFlags::TRANSFER_DST),
                "surface does not support transfer destination"
            );
            let modes =
                ins.get_physical_device_surface_present_modes_khr(g.physical, self.surface)?;
            self.mode = if modes.contains(&vk::PresentModeKHR::MAILBOX) {
                vk::PresentModeKHR::MAILBOX
            } else {
                vk::PresentModeKHR::FIFO
            };
            let formats = ins.get_physical_device_surface_formats_khr(g.physical, self.surface)?;
            let format = formats
                .iter()
                .find(|f| {
                    matches!(
                        f.format,
                        vk::Format::B8G8R8A8_UNORM | vk::Format::B8G8R8A8_SRGB
                    ) && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
                })
                .context("surface lacks an sRGB BGRA8 presentation format")?;
            let mut n = caps.min_image_count.max(3);
            if caps.max_image_count != 0 {
                n = n.min(caps.max_image_count);
            }
            let alpha = [
                vk::CompositeAlphaFlagsKHR::OPAQUE,
                vk::CompositeAlphaFlagsKHR::INHERIT,
                vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
                vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED,
            ]
            .into_iter()
            .find(|&a| caps.supported_composite_alpha.contains(a))
            .context("unsupported composite alpha")?;
            if !self.chain.is_null() {
                g.dev.queue_wait_idle(g.queues[0])?;
            }
            let info = vk::SwapchainCreateInfoKHR::builder()
                .surface(self.surface)
                .min_image_count(n)
                .image_format(format.format)
                .image_color_space(format.color_space)
                .image_extent(actual)
                .image_array_layers(1)
                .image_usage(vk::ImageUsageFlags::TRANSFER_DST)
                .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                .pre_transform(caps.current_transform)
                .composite_alpha(alpha)
                .present_mode(self.mode)
                .clipped(true)
                .old_swapchain(self.chain);
            let chain = g.dev.create_swapchain_khr(&info, None)?;
            g.dev.destroy_swapchain_khr(self.chain, None);
            self.chain = chain;
            for sem in self.finished.drain(..) {
                g.dev.destroy_semaphore(sem, None);
            }
            self.images = g.dev.get_swapchain_images_khr(chain)?;
            for _ in &self.images {
                self.finished.push(
                    g.dev
                        .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?,
                );
            }
            self.extent = extent;
            self.dirty = false;
            Ok(true)
        }
    }
    pub fn acquire(&mut self, extent: [u32; 2]) -> Result<Option<Acquired>> {
        unsafe {
            if (self.dirty || self.extent != extent) && !self.recreate(extent)? {
                return Ok(None);
            }
            match self.gpu.dev.acquire_next_image_khr(
                self.chain,
                u64::MAX,
                self.available,
                vk::Fence::null(),
            ) {
                Ok((index, status)) => {
                    self.dirty = status == vk::SuccessCode::SUBOPTIMAL_KHR;
                    Ok(Some(Acquired {
                        image: self.images[index as usize],
                        index,
                        available: self.available,
                        finished: self.finished[index as usize],
                    }))
                }
                Err(vk::ErrorCode::OUT_OF_DATE_KHR) => {
                    self.dirty = true;
                    Ok(None)
                }
                Err(e) => Err(e.into()),
            }
        }
    }
    pub fn present(&mut self, f: Acquired) -> Result<()> {
        unsafe {
            let chains = [self.chain];
            let wait = [f.finished];
            let indices = [f.index];
            let info = vk::PresentInfoKHR::builder()
                .swapchains(&chains)
                .wait_semaphores(&wait)
                .image_indices(&indices);
            match self.gpu.dev.queue_present_khr(self.gpu.queues[0], &info) {
                Ok(status) => self.dirty |= status == vk::SuccessCode::SUBOPTIMAL_KHR,
                Err(vk::ErrorCode::OUT_OF_DATE_KHR) => self.dirty = true,
                Err(e) => return Err(e.into()),
            }
            Ok(())
        }
    }
}
impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            let g = &self.gpu;
            let _ = g.dev.device_wait_idle();
            for s in self.finished.drain(..) {
                g.dev.destroy_semaphore(s, None);
            }
            g.dev.destroy_semaphore(self.available, None);
            g.dev.destroy_swapchain_khr(self.chain, None);
            g.instance().destroy_surface_khr(self.surface, None);
        }
    }
}

#[cfg(test)]
mod tests;
