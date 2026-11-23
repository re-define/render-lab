//! Native triangle CLAS and Cluster BLAS storage. A frame slot owns its scratch and results.
use super::*;
use vk::NvClusterAccelerationStructureExtensionDeviceCommands;

pub(super) struct Allocation {
    pub shape: AccelerationShape,
    pub address: u64,
    pub versions: Option<Arc<Buffer>>,
    stride: u32,
    layout: Vec<std::sync::Weak<[u8]>>,
    generation: u32,
    storage: Buffer,
    scratch: Buffer,
    scratch_address: u64,
    destination: Option<Buffer>,
}

fn with_input<T>(
    shape: AccelerationShape,
    call: impl FnOnce(&vk::ClusterAccelerationStructureInputInfoNV) -> T,
) -> T {
    use vk::ClusterAccelerationStructureOpTypeNV as Op;
    let mut triangles = vk::ClusterAccelerationStructureTriangleClusterInputNV::default();
    let mut bottom = vk::ClusterAccelerationStructureClustersBottomLevelInputNV::default();
    let mut info = vk::ClusterAccelerationStructureInputInfoNV {
        flags: vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE,
        ..Default::default()
    };
    match shape {
        AccelerationShape::TriangleClusters {
            count,
            vertices,
            triangles: faces,
        } => {
            triangles.vertex_format = vk::Format::R32G32B32_SFLOAT;
            triangles.max_cluster_unique_geometry_count = 1;
            triangles.max_cluster_triangle_count = 64;
            triangles.max_cluster_vertex_count = 64;
            triangles.max_total_triangle_count = faces;
            triangles.max_total_vertex_count = vertices;
            info.max_acceleration_structure_count = count;
            info.op_type = Op::BUILD_TRIANGLE_CLUSTER;
            info.op_mode = vk::ClusterAccelerationStructureOpModeNV::EXPLICIT_DESTINATIONS;
            info.op_input.triangle_clusters = &mut triangles;
        }
        AccelerationShape::ClusterBottom { count } => {
            bottom.max_total_cluster_count = count;
            bottom.max_cluster_count_per_acceleration_structure = count;
            info.max_acceleration_structure_count = 1;
            info.op_type = Op::BUILD_CLUSTERS_BOTTOM_LEVEL;
            info.op_mode = vk::ClusterAccelerationStructureOpModeNV::EXPLICIT_DESTINATIONS;
            info.op_input.clusters_bottom_level = &mut bottom;
        }
        _ => unreachable!(),
    }
    call(&info)
}

impl Allocation {
    pub fn new(g: &Arc<Gpu>, shape: AccelerationShape, tag: Purpose<'_>) -> Result<Self> {
        let props = g
            .ray
            .as_ref()
            .and_then(|r| r.clusters.as_ref())
            .context("Mega Geometry requires VK_NV_cluster_acceleration_structure")?;
        ensure!(
            props.max_vertices_per_cluster >= 64 && props.max_triangles_per_cluster >= 64,
            "64-element meshlets exceed CLAS limits"
        );
        let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
        unsafe {
            with_input(shape, |input| {
                g.dev
                    .get_cluster_acceleration_structure_build_sizes_nv(input, &mut sizes)
            })
        };
        let bottom = matches!(shape, AccelerationShape::ClusterBottom { .. });
        let align = u64::from(if bottom {
            props.cluster_bottom_level_byte_alignment
        } else {
            props.cluster_byte_alignment
        })
        .max(1);
        let scratch_align = u64::from(props.cluster_scratch_byte_alignment).max(1);
        ensure!(
            sizes.acceleration_structure_size > 0,
            "driver returned empty cluster allocation"
        );
        let (result_bytes, stride, versions) = if bottom {
            (sizes.acceleration_structure_size, 0, None)
        } else {
            // Explicit destinations must each accommodate a worst-case meshlet.
            let mut one = vk::AccelerationStructureBuildSizesInfoKHR::default();
            unsafe {
                with_input(
                    AccelerationShape::TriangleClusters {
                        count: 1,
                        vertices: 64,
                        triangles: 64,
                    },
                    |input| {
                        g.dev
                            .get_cluster_acceleration_structure_build_sizes_nv(input, &mut one)
                    },
                );
            }
            let stride = u32::try_from(one.acceleration_structure_size.next_multiple_of(align))?;
            ensure!(stride > 0, "driver returned an empty CLAS stride");
            let count = u64::from(shape.count());
            let mut versions = Buffer::new(
                g,
                count * 32,
                vk::BufferUsageFlags::STORAGE_BUFFER,
                true,
                Purpose {
                    class: Class::Acceleration,
                    ..tag
                },
            )?;
            versions.mapped(|p| {
                p.fill(0);
                Ok(())
            })?;
            (u64::from(stride) * count, stride, Some(Arc::new(versions)))
        };
        let storage = Buffer::device(
            g,
            result_bytes
                .checked_add(align - 1)
                .context("CLAS storage overflow")?,
            vk::BufferUsageFlags::ACCELERATION_STRUCTURE_STORAGE_KHR,
            Purpose {
                class: Class::Acceleration,
                ..tag
            },
        )?;
        let address = storage.address.next_multiple_of(align);
        let scratch = Buffer::device(
            g,
            sizes.build_scratch_size.max(16) + scratch_align - 1,
            vk::BufferUsageFlags::STORAGE_BUFFER,
            Purpose {
                class: Class::BuildScratch,
                ..tag
            },
        )?;
        let scratch_address = scratch.address.next_multiple_of(scratch_align);
        let destination = if bottom {
            let mut dst = Buffer::new(
                g,
                8,
                vk::BufferUsageFlags::STORAGE_BUFFER
                    | vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR,
                true,
                Purpose {
                    class: Class::Acceleration,
                    ..tag
                },
            )?;
            dst.mapped(|data| {
                data[..8].copy_from_slice(&address.to_ne_bytes());
                Ok(())
            })?;
            Some(dst)
        } else {
            None
        };
        Ok(Self {
            shape,
            address,
            storage,
            scratch,
            scratch_address,
            destination,
            stride,
            versions,
            layout: Vec::new(),
            generation: 0,
        })
    }
    /// A live weak identity prevents address reuse from passing as the same geometry.
    pub fn cache_parameters(&mut self, sources: &[Arc<[u8]>], force: bool) -> Result<[u32; 4]> {
        if force
            || self.generation == 0
            || self.layout.len() != sources.len()
            || self
                .layout
                .iter()
                .zip(sources)
                .any(|(a, b)| !a.ptr_eq(&Arc::downgrade(b)))
        {
            self.generation = self
                .generation
                .checked_add(1)
                .context("CLAS cache generation exhausted")?;
            self.layout = sources.iter().map(Arc::downgrade).collect();
        }
        Ok([
            self.address as u32,
            (self.address >> 32) as u32,
            self.stride,
            self.generation,
        ])
    }

    pub unsafe fn record(
        &self,
        g: &Gpu,
        cmd: vk::CommandBuffer,
        inputs: u64,
        count: u64,
        addresses: u64,
    ) {
        let region = |device_address, stride, size| vk::StridedDeviceAddressRegionKHR {
            device_address,
            stride,
            size,
        };
        let (stride, n, dest) = match &self.destination {
            Some(d) => (16, 1, d.address),
            None => (64, u64::from(self.shape.count()), addresses),
        };
        with_input(self.shape, |input| {
            g.dev.cmd_build_cluster_acceleration_structure_indirect_nv(
                cmd,
                &vk::ClusterAccelerationStructureCommandsInfoNV {
                    input: *input,
                    scratch_data: self.scratch_address,
                    dst_addresses_array: region(dest, 8, 8 * n),
                    src_infos_array: region(inputs, stride, stride * n),
                    src_infos_count: count,
                    ..Default::default()
                },
            )
        });
    }
    pub fn allocations(&self) -> impl Iterator<Item = &AllocationInfo> {
        [&self.storage.allocation, &self.scratch.allocation]
            .into_iter()
            .chain(self.destination.iter().map(|b| &b.allocation))
    }
}
