use super::allocator::{Allocator, Backing, Target};
use super::*;
use anyhow::{anyhow, bail};
use std::ffi::CStr;
use vulkanalia::loader::{LIBRARY, LibloadingLoader, Loader};

struct InstanceOwner {
    instance: vulkanalia::Instance,
    _entry: vulkanalia::Entry,
}
impl Drop for InstanceOwner {
    fn drop(&mut self) {
        unsafe { self.instance.destroy_instance(None) }
    }
}
/// Keeps the loader and instance alive through the last device call, including vendor shutdown.
pub(super) struct DeviceOwner {
    pub(super) dev: vulkanalia::Device,
    instance: InstanceOwner,
}
impl DeviceOwner {
    pub(super) fn instance(&self) -> &vulkanalia::Instance {
        &self.instance.instance
    }
}
impl Drop for DeviceOwner {
    fn drop(&mut self) {
        unsafe { self.dev.destroy_device(None) }
    }
}

pub(super) struct Gpu {
    pub dev: vulkanalia::Device,
    pub queues: Vec<vk::Queue>,
    pub physical: vk::PhysicalDevice,
    pub family: u32,
    pub name: String,
    pub memory: vk::PhysicalDeviceMemoryProperties,
    pub allocator: std::sync::Mutex<Allocator>,
    pub limits: vk::PhysicalDeviceLimits,
    pub heap: vk::PhysicalDeviceDescriptorHeapPropertiesEXT,
    pub timestamp_bits: u32,
    pub common_clock: bool,
    pub raster_queries: bool,
    pub ray: Option<RayProperties>,
    pub ngx: Option<ngx::Context>,
    pub dlss_unavailable: Option<String>,
    pub buffer_atomic64: bool,
    owner: Arc<DeviceOwner>,
}

pub(super) struct RayProperties {
    pub clusters: Option<vk::PhysicalDeviceClusterAccelerationStructurePropertiesNV>,
    pub pipeline: vk::PhysicalDeviceRayTracingPipelinePropertiesKHR,
    pub acceleration: vk::PhysicalDeviceAccelerationStructurePropertiesKHR,
}
impl Gpu {
    pub fn instance(&self) -> &vulkanalia::Instance {
        self.owner.instance()
    }
    pub fn open(raster_queries: bool) -> Result<Arc<Self>> {
        unsafe {
            let loader = LibloadingLoader::new(LIBRARY).map_err(|e| anyhow!("{e}"))?;
            let get_instance =
                std::mem::transmute::<extern "system" fn(), vk::PFN_vkGetInstanceProcAddr>(
                    loader
                        .load(b"vkGetInstanceProcAddr\0")
                        .map_err(|e| anyhow!("{e}"))?,
                );
            let entry = vulkanalia::Entry::new(loader).map_err(|e| anyhow!("{e}"))?;
            let version = vk::make_version(1, 4, 0);
            let app = vk::ApplicationInfo::builder()
                .application_name(b"Render Lab\0")
                .api_version(version);
            let mut instance_exts =
                vec![c"VK_KHR_surface".as_ptr(), c"VK_KHR_win32_surface".as_ptr()];
            let ngx = ngx::Request::from_env().and_then(|request| {
                let required =
                    request.extensions(vk::Instance::null(), vk::PhysicalDevice::null())?;
                let supported = entry.enumerate_instance_extension_properties(None)?;
                for name in &required {
                    ensure!(
                        supported
                            .iter()
                            .any(|p| CStr::from_ptr(p.extension_name.as_ptr()) == name.as_c_str()),
                        "missing NGX instance extension {}",
                        name.to_string_lossy()
                    );
                }
                Ok((request, required))
            });
            if let Ok((_, extensions)) = &ngx {
                for name in extensions {
                    if !instance_exts
                        .iter()
                        .any(|&p| CStr::from_ptr(p) == name.as_c_str())
                    {
                        instance_exts.push(name.as_ptr());
                    }
                }
            }
            let instance = entry.create_instance(
                &vk::InstanceCreateInfo::builder()
                    .application_info(&app)
                    .enabled_extension_names(&instance_exts),
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
                let mut acceleration =
                    vk::PhysicalDeviceAccelerationStructureFeaturesKHR::default();
                let mut ray = vk::PhysicalDeviceRayTracingPipelineFeaturesKHR::default();
                let ray_extensions = [
                    vk::KHR_ACCELERATION_STRUCTURE_EXTENSION.name,
                    vk::KHR_DEFERRED_HOST_OPERATIONS_EXTENSION.name,
                    vk::KHR_RAY_TRACING_PIPELINE_EXTENSION.name,
                ];
                let has_ray_extensions = ray_extensions.iter().all(|&name| supported(name));
                let mut query = vk::PhysicalDeviceFeatures2::builder()
                    .push_next(&mut h)
                    .push_next(&mut v12)
                    .push_next(&mut v13)
                    .push_next(&mut mf);
                let mut cluster_feature =
                    vk::PhysicalDeviceClusterAccelerationStructureFeaturesNV::default();
                let cluster_ext = vk::NV_CLUSTER_ACCELERATION_STRUCTURE_EXTENSION.name;
                if has_ray_extensions && supported(cluster_ext) {
                    query = query.push_next(&mut cluster_feature);
                }
                if has_ray_extensions {
                    query = query.push_next(&mut acceleration).push_next(&mut ray);
                }
                ins.get_physical_device_features2(pd, &mut query);
                let precise_stats = query.features.pipeline_statistics_query != 0
                    && query.features.occlusion_query_precise != 0;
                let int64 = query.features.shader_int64 != 0;
                let buffer_atomic64 = int64 && v12.shader_buffer_int64_atomics != 0;
                if raster_queries && !precise_stats {
                    continue;
                }
                let enable_ray = has_ray_extensions
                    && acceleration.acceleration_structure != 0
                    && ray.ray_tracing_pipeline != 0;

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
                let ngx_device = ngx.as_ref().map_err(|e| anyhow!("{e:#}")).and_then(|(request, _)| {
                    ensure!(props.vendor_id == 0x10de, "the rendering adapter is not NVIDIA");
                    let required = request.extensions(ins.handle(), pd)?;
                    for name in &required {
                        ensure!(extensions.iter().any(|p| CStr::from_ptr(p.extension_name.as_ptr()) == name.as_c_str()),
                            "missing NGX device extension {}", name.to_string_lossy());
                        ensure!(name.as_c_str() != c"VK_EXT_buffer_device_address",
                            "NGX requested legacy VK_EXT_buffer_device_address, which conflicts with the Vulkan 1.2 core address feature");
                    }
                    Ok(required)
                });
                if let Ok(required) = &ngx_device {
                    for name in required {
                        if !exts.iter().any(|&p| CStr::from_ptr(p) == name.as_c_str()) {
                            exts.push(name.as_ptr());
                        }
                    }
                }
                let clock = [
                    vk::KHR_CALIBRATED_TIMESTAMPS_EXTENSION.name,
                    vk::EXT_CALIBRATED_TIMESTAMPS_EXTENSION.name,
                ]
                .into_iter()
                .find(|name| supported(*name));
                if let Some(name) = &clock {
                    exts.push(name.as_ptr());
                }
                let ray_properties = if enable_ray {
                    exts.extend(ray_extensions.iter().map(|name| name.as_ptr()));
                    let clusters = if cluster_feature.cluster_acceleration_structure != 0 {
                        exts.push(cluster_ext.as_ptr());
                        let mut limits =
                            vk::PhysicalDeviceClusterAccelerationStructurePropertiesNV::default();
                        ins.get_physical_device_properties2(
                            pd,
                            &mut vk::PhysicalDeviceProperties2::builder().push_next(&mut limits),
                        );
                        Some(limits)
                    } else {
                        None
                    };
                    let mut properties = RayProperties {
                        clusters,
                        pipeline: Default::default(),
                        acceleration: Default::default(),
                    };
                    ins.get_physical_device_properties2(
                        pd,
                        &mut vk::PhysicalDeviceProperties2::builder()
                            .push_next(&mut properties.pipeline)
                            .push_next(&mut properties.acceleration),
                    );
                    properties.pipeline.next = std::ptr::null_mut();
                    properties.acceleration.next = std::ptr::null_mut();
                    Some(properties)
                } else {
                    None
                };
                let mut h =
                    vk::PhysicalDeviceDescriptorHeapFeaturesEXT::builder().descriptor_heap(true);
                let mut v12 = vk::PhysicalDeviceVulkan12Features::builder()
                    .shader_buffer_int64_atomics(buffer_atomic64)
                    .buffer_device_address(true)
                    .timeline_semaphore(true);
                let mut v13 = vk::PhysicalDeviceVulkan13Features::builder()
                    .dynamic_rendering(true)
                    .synchronization2(true);
                let mut mf = vk::PhysicalDeviceMeshShaderFeaturesEXT::builder().mesh_shader(true);
                let mut acceleration =
                    vk::PhysicalDeviceAccelerationStructureFeaturesKHR::builder()
                        .acceleration_structure(true);
                let mut ray = vk::PhysicalDeviceRayTracingPipelineFeaturesKHR::builder()
                    .ray_tracing_pipeline(true);
                let features = vk::PhysicalDeviceFeatures::builder()
                    .shader_int64(buffer_atomic64)
                    .pipeline_statistics_query(raster_queries)
                    .occlusion_query_precise(raster_queries);
                let mut create = vk::DeviceCreateInfo::builder()
                    .enabled_features(&features)
                    .queue_create_infos(&queues)
                    .enabled_extension_names(&exts)
                    .push_next(&mut h)
                    .push_next(&mut v12)
                    .push_next(&mut v13)
                    .push_next(&mut mf);
                if enable_ray {
                    create = create.push_next(&mut acceleration).push_next(&mut ray);
                }
                cluster_feature.next = std::ptr::null_mut();
                if enable_ray && cluster_feature.cluster_acceleration_structure != 0 {
                    create = create.push_next(&mut cluster_feature);
                }
                let dev = ins.create_device(pd, &create, None)?;
                let memory = ins.get_physical_device_memory_properties(pd);
                let owner = Arc::new(DeviceOwner {
                    dev: dev.clone(),
                    instance: owner,
                });
                let ngx_context =
                    ngx_device.and_then(|_| ngx.unwrap().0.open(get_instance, pd, owner.clone()));
                let (ngx, dlss_unavailable) = match ngx_context {
                    Ok(context) => (Some(context), None),
                    Err(error) => (None, Some(format!("{error:#}"))),
                };
                let queues = (0..priorities.len())
                    .map(|i| dev.get_device_queue(family as u32, i as u32))
                    .collect();
                return Ok(Arc::new(Self {
                    dev,
                    queues,
                    allocator: Default::default(),
                    physical: pd,
                    family: family as u32,
                    name: props.device_name.to_string(),
                    memory,
                    limits: props.limits,
                    heap,
                    timestamp_bits: q.timestamp_valid_bits,
                    common_clock: clock.is_some(),
                    raster_queries,
                    ray: ray_properties,
                    ngx,
                    dlss_unavailable,
                    buffer_atomic64,
                    owner,
                }));
            }
            bail!(
                "requires Vulkan 1.4, descriptor heap, mesh shaders, swapchain, timeline semaphores and a timestamp-capable graphics+compute queue"
            )
        }
    }
}
pub(super) struct Buffer {
    gpu: Arc<Gpu>,
    pub raw: vk::Buffer,
    backing: Backing,
    pub size: u64,
    pub usage: vk::BufferUsageFlags,
    pub allocation: AllocationInfo,
    pub(super) class: Class,
    pub prefer_local: bool,
    pub address: u64,
    pub table: Option<crate::rdg::rows::Rows>,
    pub uploaded: Option<std::sync::Weak<[u8]>>,
}
impl Buffer {
    pub fn new(
        gpu: &Arc<Gpu>,
        bytes: u64,
        usage: vk::BufferUsageFlags,
        prefer_local: bool,
        tag: Purpose<'_>,
    ) -> Result<Self> {
        Self::allocate(
            gpu,
            bytes,
            usage,
            prefer_local,
            tag,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )
    }
    pub fn device(
        gpu: &Arc<Gpu>,
        bytes: u64,
        usage: vk::BufferUsageFlags,
        tag: Purpose<'_>,
    ) -> Result<Self> {
        Self::allocate(
            gpu,
            bytes,
            usage,
            true,
            tag,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )
    }
    fn allocate(
        gpu: &Arc<Gpu>,
        bytes: u64,
        usage: vk::BufferUsageFlags,
        prefer_local: bool,
        tag: Purpose<'_>,
        memory: vk::MemoryPropertyFlags,
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
                backing: Default::default(),
                size: bytes,
                usage,
                allocation: Default::default(),
                prefer_local,
                class: tag.class,
                address: 0,
                table: None,
                uploaded: None,
            };
            (b.backing, b.allocation) = gpu.allocator.lock().unwrap().allocate(
                gpu,
                Target::Buffer(raw, bytes),
                memory,
                prefer_local,
                tag,
            )?;
            d.bind_buffer_memory(raw, b.backing.memory, b.allocation.offset)?;
            b.address =
                d.get_buffer_device_address(&vk::BufferDeviceAddressInfo::builder().buffer(raw));
            Ok(b)
        }
    }
    pub fn use_bytes(&mut self, bytes: u64) {
        if self.backing.used != bytes {
            self.gpu
                .allocator
                .lock()
                .unwrap()
                .use_bytes(&mut self.backing, bytes);
        }
    }
    pub fn mapped<T>(&mut self, f: impl FnOnce(&mut [u8]) -> Result<T>) -> Result<T> {
        unsafe {
            if self.allocation.pooled {
                // This Buffer exclusively owns this bound interval until after its last GPU use.
                let data = self
                    .backing
                    .host_slice(self.allocation.offset, self.size as usize)
                    .context("buffer block is not host visible")?;
                return f(data);
            }
            let ptr = self.gpu.dev.map_memory(
                self.backing.memory,
                0,
                self.size,
                vk::MemoryMapFlags::empty(),
            )?;
            let res = f(std::slice::from_raw_parts_mut(
                ptr.cast(),
                self.size as usize,
            ));
            self.gpu.dev.unmap_memory(self.backing.memory);
            res
        }
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe {
            self.gpu.dev.destroy_buffer(self.raw, None);
            self.gpu
                .allocator
                .lock()
                .unwrap()
                .release(&self.gpu, self.backing, &self.allocation);
        }
    }
}
pub(super) const COLOR: vk::Format = vk::Format::R32G32_UINT;
pub(super) const DEPTH: vk::Format = vk::Format::D32_SFLOAT;
pub(super) struct Image {
    gpu: Arc<Gpu>,
    pub raw: vk::Image,
    backing: Backing,
    pub view: vk::ImageView,
    pub range: vk::ImageSubresourceRange,
    pub format: vk::Format,
    pub usage: vk::ImageUsageFlags,
    pub allocation: AllocationInfo,
}
impl Image {
    pub fn usage_for(format: ImageFormat, declared: ImageUsage) -> vk::ImageUsageFlags {
        let mut flags = match format {
            ImageFormat::Depth => vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
            ImageFormat::Visibility => vk::ImageUsageFlags::COLOR_ATTACHMENT,
            ImageFormat::Rgba32Float | ImageFormat::Rg16Float => vk::ImageUsageFlags::empty(),
        };
        flags.set(vk::ImageUsageFlags::SAMPLED, declared.sampled);
        flags.set(vk::ImageUsageFlags::TRANSFER_SRC, declared.copy_source);
        flags.set(vk::ImageUsageFlags::TRANSFER_DST, declared.copy_destination);
        flags.set(vk::ImageUsageFlags::STORAGE, declared.storage);
        flags
    }

    pub fn new(
        gpu: &Arc<Gpu>,
        ImageDesc {
            extent: [width, height],
            format,
        }: ImageDesc,
        usage: vk::ImageUsageFlags,
        tag: Purpose<'_>,
    ) -> Result<Self> {
        unsafe {
            let d = &gpu.dev;
            let (format, aspect) = match format {
                ImageFormat::Depth => (DEPTH, vk::ImageAspectFlags::DEPTH),
                ImageFormat::Visibility => (COLOR, vk::ImageAspectFlags::COLOR),
                ImageFormat::Rgba32Float => {
                    (vk::Format::R32G32B32A32_SFLOAT, vk::ImageAspectFlags::COLOR)
                }
                ImageFormat::Rg16Float => (vk::Format::R16G16_SFLOAT, vk::ImageAspectFlags::COLOR),
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
                format,
                usage,
                view: vk::ImageView::null(),
                backing: Default::default(),
                range,
                allocation: Default::default(),
            };
            (img.backing, img.allocation) = gpu.allocator.lock().unwrap().allocate(
                gpu,
                Target::Image(raw),
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
                false,
                tag,
            )?;
            d.bind_image_memory(raw, img.backing.memory, img.allocation.offset)?;
            if usage.intersects(
                vk::ImageUsageFlags::COLOR_ATTACHMENT
                    | vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::STORAGE,
            ) {
                img.view = d.create_image_view(
                    &vk::ImageViewCreateInfo::builder()
                        .image(raw)
                        .view_type(vk::ImageViewType::_2D)
                        .format(format)
                        .subresource_range(range),
                    None,
                )?;
            }
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
            self.gpu
                .allocator
                .lock()
                .unwrap()
                .release(&self.gpu, self.backing, &self.allocation);
        }
    }
}
