use super::*;
use anyhow::{anyhow, bail};
use vulkanalia::loader::{LIBRARY, LibloadingLoader};

struct InstanceOwner {
    instance: vulkanalia::Instance,
    _entry: vulkanalia::Entry,
}
impl Drop for InstanceOwner {
    fn drop(&mut self) {
        unsafe { self.instance.destroy_instance(None) }
    }
}
pub(super) struct Gpu {
    pub dev: vulkanalia::Device,
    pub queues: Vec<vk::Queue>,
    pub physical: vk::PhysicalDevice,
    pub family: u32,
    pub name: String,
    pub memory: vk::PhysicalDeviceMemoryProperties,
    pub limits: vk::PhysicalDeviceLimits,
    pub heap: vk::PhysicalDeviceDescriptorHeapPropertiesEXT,
    pub timestamp_bits: u32,
    pub common_clock: bool,
    owner: InstanceOwner,
}
impl Gpu {
    pub fn instance(&self) -> &vulkanalia::Instance {
        &self.owner.instance
    }
    pub fn open() -> Result<Arc<Self>> {
        unsafe {
            let loader = LibloadingLoader::new(LIBRARY).map_err(|e| anyhow!("{e}"))?;
            let entry = vulkanalia::Entry::new(loader).map_err(|e| anyhow!("{e}"))?;
            let version = vk::make_version(1, 4, 0);
            let app = vk::ApplicationInfo::builder()
                .application_name(b"Render Lab\0")
                .api_version(version);
            let wsi = [c"VK_KHR_surface".as_ptr(), c"VK_KHR_win32_surface".as_ptr()];
            let instance = entry.create_instance(
                &vk::InstanceCreateInfo::builder()
                    .application_info(&app)
                    .enabled_extension_names(&wsi),
                None,
            )?;
            let owner = InstanceOwner {
                instance,
                _entry: entry,
            };
            let ins = &owner.instance;
            for pd in ins.enumerate_physical_devices()? {
                let props = ins.get_physical_device_properties(pd);
                let extensions = ins.enumerate_device_extension_properties(pd, None)?;
                let supported = |name| extensions.iter().any(|e| e.extension_name == name);
                if props.api_version < version
                    || !supported(vk::EXT_DESCRIPTOR_HEAP_EXTENSION.name)
                    || !supported(vk::KHR_SWAPCHAIN_EXTENSION.name)
                    || !supported(vk::EXT_MESH_SHADER_EXTENSION.name)
                {
                    continue;
                }
                let mut mf = vk::PhysicalDeviceMeshShaderFeaturesEXT::default();
                let mut h = vk::PhysicalDeviceDescriptorHeapFeaturesEXT::default();
                let mut v12 = vk::PhysicalDeviceVulkan12Features::default();
                let mut v13 = vk::PhysicalDeviceVulkan13Features::default();
                let mut query = vk::PhysicalDeviceFeatures2::builder()
                    .push_next(&mut h)
                    .push_next(&mut v12)
                    .push_next(&mut v13)
                    .push_next(&mut mf);
                ins.get_physical_device_features2(pd, &mut query);

                if [
                    h.descriptor_heap,
                    mf.mesh_shader,
                    v12.buffer_device_address,
                    v12.timeline_semaphore,
                    v13.dynamic_rendering,
                    v13.synchronization2,
                ]
                .contains(&0)
                {
                    continue;
                }
                let families = ins.get_physical_device_queue_family_properties(pd);
                let Some((family, q)) = families.iter().enumerate().find(|(_, q)| {
                    q.queue_flags
                        .contains(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE)
                        && q.timestamp_valid_bits > 0
                }) else {
                    continue;
                };
                let mut heap = vk::PhysicalDeviceDescriptorHeapPropertiesEXT::default();
                ins.get_physical_device_properties2(
                    pd,
                    &mut vk::PhysicalDeviceProperties2::builder().push_next(&mut heap),
                );
                let priorities = vec![1.; q.queue_count.min(3) as usize];
                let queues = [vk::DeviceQueueCreateInfo::builder()
                    .queue_family_index(family as u32)
                    .queue_priorities(&priorities)
                    .build()];
                let mut exts = vec![
                    c"VK_EXT_descriptor_heap".as_ptr(),
                    c"VK_KHR_swapchain".as_ptr(),
                    c"VK_EXT_mesh_shader".as_ptr(),
                ];
                let clock = [
                    vk::KHR_CALIBRATED_TIMESTAMPS_EXTENSION.name,
                    vk::EXT_CALIBRATED_TIMESTAMPS_EXTENSION.name,
                ]
                .into_iter()
                .find(|name| supported(*name));
                if let Some(name) = &clock {
                    exts.push(name.as_ptr());
                }
                let mut h =
                    vk::PhysicalDeviceDescriptorHeapFeaturesEXT::builder().descriptor_heap(true);
                let mut v12 = vk::PhysicalDeviceVulkan12Features::builder()
                    .buffer_device_address(true)
                    .timeline_semaphore(true);
                let mut v13 = vk::PhysicalDeviceVulkan13Features::builder()
                    .dynamic_rendering(true)
                    .synchronization2(true);
                let mut mf = vk::PhysicalDeviceMeshShaderFeaturesEXT::builder().mesh_shader(true);
                let create = vk::DeviceCreateInfo::builder()
                    .queue_create_infos(&queues)
                    .enabled_extension_names(&exts)
                    .push_next(&mut h)
                    .push_next(&mut v12)
                    .push_next(&mut v13)
                    .push_next(&mut mf);
                let dev = ins.create_device(pd, &create, None)?;
                let queues = (0..priorities.len())
                    .map(|i| dev.get_device_queue(family as u32, i as u32))
                    .collect();
                return Ok(Arc::new(Self {
                    dev,
                    queues,
                    physical: pd,
                    family: family as u32,
                    name: props.device_name.to_string(),
                    memory: ins.get_physical_device_memory_properties(pd),
                    limits: props.limits,
                    heap,
                    timestamp_bits: q.timestamp_valid_bits,
                    common_clock: clock.is_some(),
                    owner,
                }));
            }
            bail!(
                "requires Vulkan 1.4, descriptor heap, mesh shaders, swapchain, timeline semaphores and a timestamp-capable graphics+compute queue"
            )
        }
    }
    pub fn allocate(
        &self,
        req: vk::MemoryRequirements,
        flags: vk::MemoryPropertyFlags,
        address: bool,
        prefer_local: bool,
    ) -> Result<(vk::DeviceMemory, AllocationInfo)> {
        let compatible = (0..self.memory.memory_type_count).filter(|&i| {
            req.memory_type_bits & (1 << i) != 0
                && self.memory.memory_types[i as usize]
                    .property_flags
                    .contains(flags)
        });
        let local = |i: u32| {
            self.memory.memory_types[i as usize]
                .property_flags
                .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
        };
        let mut addr =
            vk::MemoryAllocateFlagsInfo::builder().flags(vk::MemoryAllocateFlags::DEVICE_ADDRESS);
        // A mapped device-local heap can have limited BAR capacity. Try the regular
        // coherent types as well when that heap cannot serve the atlas.
        let mut error = None;
        for ty in compatible
            .clone()
            .filter(|&i| prefer_local && local(i))
            .chain(compatible.filter(|&i| !prefer_local || !local(i)))
        {
            let mut info = vk::MemoryAllocateInfo::builder()
                .allocation_size(req.size)
                .memory_type_index(ty);
            if address {
                info = info.push_next(&mut addr);
            }
            match unsafe { self.dev.allocate_memory(&info, None) } {
                Ok(mem) => {
                    let heap_index = self.memory.memory_types[ty as usize].heap_index;
                    let device_local = self.memory.memory_heaps[heap_index as usize]
                        .flags
                        .contains(vk::MemoryHeapFlags::DEVICE_LOCAL);
                    return Ok((
                        mem,
                        AllocationInfo {
                            allocated_bytes: req.size,
                            memory_type: ty,
                            heap_index,
                            device_local,
                        },
                    ));
                }
                Err(e @ vk::ErrorCode::OUT_OF_DEVICE_MEMORY) => error = Some(e),
                Err(e) => return Err(e.into()),
            }
        }
        if let Some(e) = error {
            return Err(e.into());
        }
        bail!("no compatible memory type")
    }
}
impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe { self.dev.destroy_device(None) }
    }
}

pub(super) struct Buffer {
    gpu: Arc<Gpu>,
    pub raw: vk::Buffer,
    mem: vk::DeviceMemory,
    pub size: u64,
    pub allocation: AllocationInfo,
    pub prefer_local: bool,
    pub address: u64,
    pub history: Option<(u64, [u32; 2])>,
    pub uploaded: Option<std::sync::Weak<[u8]>>,
}
impl Buffer {
    pub fn new(
        gpu: &Arc<Gpu>,
        bytes: u64,
        usage: vk::BufferUsageFlags,
        prefer_local: bool,
    ) -> Result<Self> {
        unsafe {
            if usage.contains(vk::BufferUsageFlags::STORAGE_BUFFER) {
                ensure!(
                    bytes <= u64::from(gpu.limits.max_storage_buffer_range),
                    "storage buffer exceeds device limit"
                );
            }
            let d = &gpu.dev;
            let raw = d.create_buffer(
                &vk::BufferCreateInfo::builder()
                    .size(bytes)
                    .usage(usage | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS),
                None,
            )?;
            let mut b = Self {
                gpu: gpu.clone(),
                raw,
                mem: vk::DeviceMemory::null(),
                size: bytes,
                allocation: Default::default(),
                prefer_local,
                address: 0,
                uploaded: None,
                history: None,
            };
            (b.mem, b.allocation) = gpu.allocate(
                d.get_buffer_memory_requirements(raw),
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                true,
                prefer_local,
            )?;
            d.bind_buffer_memory(raw, b.mem, 0)?;
            b.address =
                d.get_buffer_device_address(&vk::BufferDeviceAddressInfo::builder().buffer(raw));
            Ok(b)
        }
    }
    pub fn mapped<T>(&mut self, f: impl FnOnce(&mut [u8]) -> Result<T>) -> Result<T> {
        unsafe {
            let ptr =
                self.gpu
                    .dev
                    .map_memory(self.mem, 0, self.size, vk::MemoryMapFlags::empty())?;
            let res = f(std::slice::from_raw_parts_mut(
                ptr.cast(),
                self.size as usize,
            ));
            self.gpu.dev.unmap_memory(self.mem);
            res
        }
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe {
            self.gpu.dev.destroy_buffer(self.raw, None);
            self.gpu.dev.free_memory(self.mem, None)
        }
    }
}
pub(super) const COLOR: vk::Format = vk::Format::R32G32_UINT;
pub(super) const DEPTH: vk::Format = vk::Format::D32_SFLOAT;
pub(super) struct Image {
    gpu: Arc<Gpu>,
    pub raw: vk::Image,
    mem: vk::DeviceMemory,
    pub view: vk::ImageView,
    pub range: vk::ImageSubresourceRange,
    pub allocation: AllocationInfo,
}
impl Image {
    pub fn new(gpu: &Arc<Gpu>, [width, height]: [u32; 2], depth: bool) -> Result<Self> {
        unsafe {
            let d = &gpu.dev;
            let (format, usage, aspect) = if depth {
                (
                    DEPTH,
                    vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
                        | vk::ImageUsageFlags::TRANSFER_SRC,
                    vk::ImageAspectFlags::DEPTH,
                )
            } else {
                (
                    COLOR,
                    vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
                    vk::ImageAspectFlags::COLOR,
                )
            };
            let raw = d.create_image(
                &vk::ImageCreateInfo::builder()
                    .image_type(vk::ImageType::_2D)
                    .format(format)
                    .extent(vk::Extent3D {
                        width,
                        height,
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::_1)
                    .usage(usage),
                None,
            )?;
            let range = vk::ImageSubresourceRange::builder()
                .aspect_mask(aspect)
                .level_count(1)
                .layer_count(1)
                .build();
            let mut img = Self {
                gpu: gpu.clone(),
                raw,
                view: vk::ImageView::null(),
                mem: vk::DeviceMemory::null(),
                range,
                allocation: Default::default(),
            };
            (img.mem, img.allocation) = gpu.allocate(
                d.get_image_memory_requirements(raw),
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
                false,
                false,
            )?;
            d.bind_image_memory(raw, img.mem, 0)?;
            img.view = d.create_image_view(
                &vk::ImageViewCreateInfo::builder()
                    .image(raw)
                    .view_type(vk::ImageViewType::_2D)
                    .format(format)
                    .subresource_range(range),
                None,
            )?;
            Ok(img)
        }
    }
}
impl Drop for Image {
    fn drop(&mut self) {
        unsafe {
            let d = &self.gpu.dev;
            d.destroy_image_view(self.view, None);
            d.destroy_image(self.raw, None);
            d.free_memory(self.mem, None)
        }
    }
}
