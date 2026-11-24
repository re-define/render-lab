use super::*;
use crate::rdg::AccelerationShape;
use vulkanalia::vk::{
    KhrAccelerationStructureExtensionDeviceCommands, KhrRayTracingPipelineExtensionDeviceCommands,
};

impl device::RayProperties {
    pub(super) fn validate_dispatch(
        &self,
        extent: [u32; 2],
        shader: Shader,
        limits: &vk::PhysicalDeviceLimits,
    ) -> Result<()> {
        ensure!(
            !shader.supports_clusters() || self.clusters.is_some(),
            "cluster ray pipeline requires VK_NV_cluster_acceleration_structure"
        );
        for (axis, size) in extent.into_iter().enumerate() {
            let maximum = u64::from(limits.max_compute_work_group_count[axis])
                * u64::from(limits.max_compute_work_group_size[axis]);
            ensure!(
                size > 0 && u64::from(size) <= maximum,
                "ray dispatch axis {axis} size {size} outside 1..={maximum}"
            );
        }
        ensure!(
            u64::from(extent[0]) * u64::from(extent[1])
                <= u64::from(self.pipeline.max_ray_dispatch_invocation_count),
            "ray dispatch exceeds device invocation limit"
        );
        Ok(())
    }
}

pub(super) struct Acceleration {
    gpu: Arc<Gpu>,
    pub raw: vk::AccelerationStructureKHR,
    pub address: u64,
    pub shape: AccelerationShape,
    backing: Buffer,
    scratch: Option<Buffer>,
    scratch_address: u64,
    instance: Option<Buffer>,
    source: Option<BuildSource>,
    pub needs_build: bool,
    pub update: bool,
}

/// The CPU versions keep pointer-based keys unique until every referencing frame retires.
pub(super) struct StaticBlas {
    pub acceleration: Acceleration,
    _sources: [Arc<[u8]>; 2],
    pub ready: std::sync::OnceLock<(usize, u64)>,
}

#[derive(Default)]
pub(super) struct StaticCache(HashMap<(usize, usize, u32, u32), Weak<StaticBlas>>);

impl StaticCache {
    pub fn prune(&mut self) {
        self.0.retain(|_, v| v.strong_count() != 0);
    }

    pub fn bind(
        &mut self,
        g: &Arc<Gpu>,
        shape: AccelerationShape,
        sources: [&Arc<[u8]>; 2],
        tag: Purpose<'_>,
    ) -> Result<Physical> {
        let AccelerationShape::Triangles { vertices, count } = shape else {
            unreachable!()
        };
        let key = (
            sources[0].as_ptr() as usize,
            sources[1].as_ptr() as usize,
            vertices,
            count,
        );
        if let Some(blas) = self.0.get(&key).and_then(Weak::upgrade)
            && blas.ready.get().is_some()
        {
            return Ok(Physical::StaticBlas {
                blas,
                scratch: None,
            });
        }
        let mut acceleration = Acceleration::new(g, shape, Purpose { slot: None, ..tag })?;
        let scratch = acceleration.scratch.take();
        let blas = Arc::new(StaticBlas {
            acceleration,
            _sources: sources.map(Arc::clone),
            ready: Default::default(),
        });
        self.0.insert(key, Arc::downgrade(&blas));
        Ok(Physical::StaticBlas { blas, scratch })
    }
}

enum BuildSource {
    Mesh {
        vertices: Option<Weak<[u8]>>,
        topology: Option<Weak<[u8]>>,
    },
    Instances(Vec<[u32; 16]>),
}

fn geometry(
    shape: AccelerationShape,
    address: u64,
    indices: u64,
) -> vk::AccelerationStructureGeometryKHR {
    let device = vk::DeviceOrHostAddressConstKHR {
        device_address: address,
    };
    let (geometry_type, data) = if let AccelerationShape::Triangles { vertices, .. } = shape {
        let triangles = vk::AccelerationStructureGeometryTrianglesDataKHR::builder()
            .vertex_format(vk::Format::R32G32B32_SFLOAT)
            .vertex_data(device)
            .vertex_stride(32)
            .max_vertex(vertices - 1)
            .index_type(vk::IndexType::UINT32)
            .index_data(vk::DeviceOrHostAddressConstKHR {
                device_address: indices,
            })
            .build();
        (
            vk::GeometryTypeKHR::TRIANGLES,
            vk::AccelerationStructureGeometryDataKHR { triangles },
        )
    } else {
        let instances = vk::AccelerationStructureGeometryInstancesDataKHR::builder()
            .data(device)
            .build();
        (
            vk::GeometryTypeKHR::INSTANCES,
            vk::AccelerationStructureGeometryDataKHR { instances },
        )
    };
    vk::AccelerationStructureGeometryKHR::builder()
        .geometry_type(geometry_type)
        .geometry(data)
        .flags(vk::GeometryFlagsKHR::OPAQUE)
        .build()
}

fn level(shape: AccelerationShape) -> vk::AccelerationStructureTypeKHR {
    if matches!(shape, AccelerationShape::Triangles { .. }) {
        vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL
    } else {
        vk::AccelerationStructureTypeKHR::TOP_LEVEL
    }
}

impl Acceleration {
    pub fn new(gpu: &Arc<Gpu>, shape: AccelerationShape, tag: Purpose<'_>) -> Result<Self> {
        unsafe {
            let properties = &gpu
                .ray
                .as_ref()
                .context("acceleration structure builds require VK_KHR_acceleration_structure")?
                .acceleration;
            if let AccelerationShape::Triangles { count, .. } = shape {
                ensure!(
                    count > 0
                        && count <= u32::MAX / 3
                        && u64::from(count) <= properties.max_primitive_count,
                    "acceleration structure exceeds triangle limit"
                );
            }
            ensure!(
                shape.count() > 0
                    && (!matches!(shape, AccelerationShape::Instances { .. })
                        || u64::from(shape.count()) <= properties.max_instance_count),
                "invalid acceleration capacity"
            );
            let geometries = [geometry(shape, 0, 0)];
            let info = vk::AccelerationStructureBuildGeometryInfoKHR::builder()
                .type_(level(shape))
                .flags(
                    vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE
                        | vk::BuildAccelerationStructureFlagsKHR::ALLOW_UPDATE,
                )
                .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
                .geometries(&geometries);
            let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
            gpu.dev.get_acceleration_structure_build_sizes_khr(
                vk::AccelerationStructureBuildTypeKHR::DEVICE,
                &info,
                &[shape.count()],
                &mut sizes,
            );
            let backing = Buffer::device(
                gpu,
                sizes.acceleration_structure_size,
                vk::BufferUsageFlags::ACCELERATION_STRUCTURE_STORAGE_KHR,
                Purpose {
                    class: Class::Acceleration,
                    ..tag
                },
            )?;
            let alignment =
                u64::from(properties.min_acceleration_structure_scratch_offset_alignment);
            let scratch = Buffer::device(
                gpu,
                (sizes.build_scratch_size.max(sizes.update_scratch_size) + alignment - 1).max(16),
                vk::BufferUsageFlags::STORAGE_BUFFER,
                Purpose {
                    class: Class::BuildScratch,
                    ..tag
                },
            )?;
            let scratch_address = scratch.address.next_multiple_of(alignment);
            let instance = if matches!(shape, AccelerationShape::Instances { .. }) {
                Some(Buffer::new(
                    gpu,
                    u64::from(shape.count()) * 64 + 15,
                    vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR,
                    false,
                    Purpose {
                        class: Class::Acceleration,
                        ..tag
                    },
                )?)
            } else {
                None
            };
            let raw = gpu.dev.create_acceleration_structure_khr(
                &vk::AccelerationStructureCreateInfoKHR::builder()
                    .buffer(backing.raw)
                    .size(sizes.acceleration_structure_size)
                    .type_(level(shape)),
                None,
            )?;
            let address = gpu.dev.get_acceleration_structure_device_address_khr(
                &vk::AccelerationStructureDeviceAddressInfoKHR::builder()
                    .acceleration_structure(raw),
            );
            Ok(Self {
                gpu: gpu.clone(),
                raw,
                address,
                shape,
                update: false,
                backing,
                scratch: Some(scratch),
                scratch_address,
                instance,
                source: None,
                needs_build: true,
            })
        }
    }

    pub fn prepare_blas(&mut self, vertices: Option<&Arc<[u8]>>, topology: Option<&Arc<[u8]>>) {
        let same = |old: &Option<Weak<[u8]>>, new: Option<&Arc<[u8]>>| {
            old.as_ref()
                .zip(new)
                .is_some_and(|(a, b)| a.ptr_eq(&Arc::downgrade(b)))
        };
        self.update = false;
        self.needs_build = true;
        if let Some(BuildSource::Mesh {
            vertices: prior,
            topology: old,
        }) = &self.source
            && same(old, topology)
        {
            self.needs_build = !same(prior, vertices);
            self.update = self.needs_build;
        }
        self.source = Some(BuildSource::Mesh {
            vertices: vertices.map(Arc::downgrade),
            topology: topology.map(Arc::downgrade),
        });
    }
    pub fn prepare_tlas(&mut self, records: Vec<[u32; 16]>, changed: bool) -> Result<()> {
        self.update = matches!(self.source, Some(BuildSource::Instances(_)));
        self.needs_build =
            changed || !matches!(&self.source,Some(BuildSource::Instances(old)) if *old==records);
        if self.needs_build {
            let instance = self.instance.as_mut().context("expected TLAS")?;
            let offset = (instance.address.next_multiple_of(16) - instance.address) as usize;
            let bytes = bytemuck::cast_slice(&records);
            instance.mapped(|p| {
                p[offset..offset + bytes.len()].copy_from_slice(bytes);
                Ok(())
            })?;
        }
        self.source = Some(BuildSource::Instances(records));
        Ok(())
    }
    pub unsafe fn record(&self, cmd: vk::CommandBuffer, vertices: Option<(u64, u64)>) {
        let (address, indices) = vertices.unwrap_or_else(|| {
            (
                self.instance.as_ref().unwrap().address.next_multiple_of(16),
                0,
            )
        });
        let geometry = [geometry(self.shape, address, indices)];
        let info = vk::AccelerationStructureBuildGeometryInfoKHR::builder()
            .type_(level(self.shape))
            .flags(
                vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE
                    | vk::BuildAccelerationStructureFlagsKHR::ALLOW_UPDATE,
            )
            .mode(if self.update {
                vk::BuildAccelerationStructureModeKHR::UPDATE
            } else {
                vk::BuildAccelerationStructureModeKHR::BUILD
            })
            .src_acceleration_structure(if self.update {
                self.raw
            } else {
                vk::AccelerationStructureKHR::null()
            })
            .dst_acceleration_structure(self.raw)
            .geometries(&geometry)
            .scratch_data(vk::DeviceOrHostAddressKHR {
                device_address: self.scratch_address,
            });
        let ranges = [vk::AccelerationStructureBuildRangeInfoKHR {
            primitive_count: self.shape.count(),
            ..Default::default()
        }];
        self.gpu
            .dev
            .cmd_build_acceleration_structures_khr(cmd, &[info], &[&ranges]);
    }

    pub fn allocations(&self) -> impl Iterator<Item = &AllocationInfo> {
        std::iter::once(&self.backing.allocation)
            .chain(self.scratch.iter().map(|b| &b.allocation))
            .chain(self.instance.iter().map(|buffer| &buffer.allocation))
    }
}

impl Drop for Acceleration {
    fn drop(&mut self) {
        unsafe {
            self.gpu
                .dev
                .destroy_acceleration_structure_khr(self.raw, None)
        }
    }
}

pub(super) struct ShaderTable {
    _buffer: Buffer,
    regions: [vk::StridedDeviceAddressRegionKHR; 3],
}

impl ShaderTable {
    pub unsafe fn new(gpu: &Arc<Gpu>, pipeline: vk::Pipeline) -> Result<Self> {
        let props = &gpu
            .ray
            .as_ref()
            .context("ray tracing pipeline unavailable")?
            .pipeline;
        let handle = u64::from(props.shader_group_handle_size);
        let alignment = u64::from(props.shader_group_base_alignment);
        let stride = handle.next_multiple_of(u64::from(props.shader_group_handle_alignment));
        let spacing = stride.next_multiple_of(alignment);
        ensure!(
            stride <= u64::from(props.max_shader_group_stride),
            "shader record exceeds stride limit"
        );
        let mut buffer = Buffer::new(
            gpu,
            spacing * 3 + alignment - 1,
            vk::BufferUsageFlags::SHADER_BINDING_TABLE_KHR,
            true,
            Class::Infrastructure.named("Ray shader binding table"),
        )?;
        let start = buffer.address.next_multiple_of(alignment);
        let base = (start - buffer.address) as usize;
        let mut handles = vec![0; 3 * handle as usize];
        gpu.dev
            .get_ray_tracing_shader_group_handles_khr(pipeline, 0, 3, &mut handles)?;
        buffer.mapped(|data| {
            for (index, src) in handles.chunks_exact(handle as usize).enumerate() {
                let offset = base + index * spacing as usize;
                data[offset..offset + src.len()].copy_from_slice(src);
            }
            Ok(())
        })?;
        let regions = std::array::from_fn(|index| vk::StridedDeviceAddressRegionKHR {
            device_address: start + spacing * index as u64,
            stride,
            size: stride,
        });
        Ok(Self {
            _buffer: buffer,
            regions,
        })
    }
    pub unsafe fn trace(
        &self,
        device: &vulkanalia::Device,
        cmd: vk::CommandBuffer,
        [width, height]: [u32; 2],
    ) {
        let [rgen, miss, hit] = &self.regions;
        device.cmd_trace_rays_khr(cmd, rgen, miss, hit, &Default::default(), width, height, 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_dispatch_checks_optional_cluster_feature_axes_and_invocations() {
        let limits = vk::PhysicalDeviceLimits {
            max_compute_work_group_count: [2, 3, 1],
            max_compute_work_group_size: [4, 2, 1],
            ..Default::default()
        };
        let mut caps = device::RayProperties {
            pipeline: vk::PhysicalDeviceRayTracingPipelinePropertiesKHR {
                max_ray_dispatch_invocation_count: 32,
                ..Default::default()
            },
            acceleration: Default::default(),
            clusters: None,
        };
        let plain = Shader::RayTracing(&[], &[], &[]);
        let cluster = Shader::ClusterRays(&[], &[], &[], &[]);
        assert!(caps.validate_dispatch([8, 4], plain, &limits).is_ok());
        for size in [[9, 1], [1, 7], [8, 5], [0, 1]] {
            assert!(caps.validate_dispatch(size, plain, &limits).is_err());
        }
        assert!(caps.validate_dispatch([1, 1], cluster, &limits).is_err());
        caps.clusters = Some(Default::default());
        assert!(caps.validate_dispatch([1, 6], cluster, &limits).is_ok());
    }
}
