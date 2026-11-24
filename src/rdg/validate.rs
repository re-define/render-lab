//! Validate per-pass resource contracts before scheduling or Vulkan recording.
use super::*;
use std::collections::BTreeMap;

impl Graph {
    pub(super) fn validate_pass(
        &self,
        p: &Pass,
        defined: &[bool],
        tlas_sources: &mut BTreeMap<Resource, BTreeSet<Resource>>,
    ) -> Result<()> {
        if let Some(d) = p.op.program() {
            ensure!(
                matches!(
                    (&p.op, d.shader),
                    (Op::Draw { .. }, Shader::Graphics(..) | Shader::Mesh(..))
                        | (
                            Op::Compute { .. } | Op::ComputeIndirect { .. },
                            Shader::Compute(_)
                        )
                        | (
                            Op::TraceRays { .. },
                            Shader::RayTracing(..)
                                | Shader::ClusterShadows(..)
                                | Shader::ClusterRays(..)
                        )
                ),
                "shader kind differs from pass"
            );
            ensure!(
                d.bindings.iter().all(|(_, a)| match p.op {
                    Op::Draw { .. } => matches!(
                        a,
                        Access::Vertex | Access::Mesh | Access::MeshFragment | Access::Fragment
                    ),
                    Op::TraceRays { .. } =>
                        matches!(a, Access::RayRead | Access::RayWrite | Access::RayReadWrite),
                    _ => matches!(
                        a,
                        Access::ComputeRead
                            | Access::ComputeSample
                            | Access::ComputeWrite
                            | Access::ComputeReadWrite
                    ),
                }),
                "invalid shader binding access"
            );
        }
        let acceleration = |r: Resource| match self.resources.get(r.0) {
            Some((_, Storage::Acceleration(shape))) => Some(*shape),
            _ => None,
        };
        let fits = |r: Resource, bytes: u64| matches!(self.resources.get(r.0),Some((_,Storage::Buffer{bytes:size,..})) if *size as u64>=bytes);
        match p.op {
            Op::HostRead(r) | Op::Counters { buffer: r, .. } => {
                ensure!(
                    self.host_readable_buffer(r),
                    "host reads require a frame-local host-readable buffer"
                );
            }
            Op::BuildClusters {
                records,
                vertices,
                indices,
                addresses,
                count,
                output,
            } => {
                let Some(AccelerationShape::TriangleClusters {
                    count: n,
                    vertices: nv,
                    triangles: nt,
                }) = acceleration(output)
                else {
                    anyhow::bail!("expected triangle CLAS storage")
                };

                ensure!(
                    n > 0
                        && nv > 0
                        && nt > 0
                        && fits(records, 64 * u64::from(n))
                        && fits(count, 4)
                        && fits(addresses, 8 * u64::from(n))
                        && fits(vertices, 12 * u64::from(nv))
                        && fits(indices, 4 * u64::from(nt))
                        && !defined[output.0],
                    "invalid CLAS build buffers"
                );
            }
            Op::BuildClusterBottom {
                clusters,
                output,
                addresses,
                record,
            } => {
                let Some(AccelerationShape::ClusterBottom { count }) = acceleration(output) else {
                    anyhow::bail!("expected cluster BLAS")
                };
                ensure!(
                    matches!(acceleration(clusters),Some(AccelerationShape::TriangleClusters{count:n,..}) if n==count)
                        && fits(record, 16)
                        && fits(addresses, 8 * u64::from(count))
                        && !defined[output.0],
                    "invalid cluster BLAS input"
                );
            }
            Op::BuildBlas {
                vertices,
                indices,
                topology,
                output,
            } => {
                let Some(AccelerationShape::Triangles { count, vertices: n }) =
                    acceleration(output)
                else {
                    anyhow::bail!("expected BLAS")
                };

                ensure!(
                    n > 0
                        && count > 0
                        && fits(vertices, u64::from(n) * 32)
                        && fits(indices, u64::from(count) * 12),
                    "invalid ray geometry streams"
                );
                ensure!(!defined[output.0], "duplicate BLAS build");
                ensure!(fits(topology, 1), "invalid BLAS topology version buffer");
            }
            Op::BuildTlas {
                ref instances,
                output,
            } => {
                ensure!(
                    acceleration(output)
                        == Some(AccelerationShape::Instances {
                            count: instances.len().try_into()?
                        })
                        && !instances.is_empty(),
                    "invalid TLAS instance count"
                );
                ensure!(
                    instances.iter().all(|i| matches!(
                        acceleration(i.blas),
                        Some(
                            AccelerationShape::Triangles { .. }
                                | AccelerationShape::ClusterBottom { .. }
                        )
                    ) && i.first_triangle <= 0xffffff),
                    "invalid ray instance"
                );
                ensure!(
                    tlas_sources
                        .insert(output, instances.iter().map(|i| i.blas).collect())
                        .is_none(),
                    "duplicate TLAS build"
                );
            }
            Op::TraceRays {
                tlas,
                ref blases,
                ref program,
                ..
            } => {
                ensure!(
                    tlas_sources.get(&tlas) == Some(&blases.iter().copied().collect()),
                    "ray trace must declare all referenced BLAS"
                );
                ensure!(
                    program.shader.supports_clusters()
                        || !blases.iter().any(|&r| matches!(
                            acceleration(r),
                            Some(AccelerationShape::ClusterBottom { .. })
                        )),
                    "tracing a cluster BLAS requires a cluster-capable ray pipeline"
                );
            }
            Op::Dlss(dlss) => {
                let source = |format| {
                    Some(ImageDesc {
                        extent: self.extent,
                        format,
                    })
                };
                ensure!(
                    self.image_desc(dlss.color) == source(ImageFormat::Rgba32Float)
                        && self.image_desc(dlss.depth) == source(ImageFormat::Depth)
                        && self.image_desc(dlss.motion) == source(ImageFormat::Rg16Float)
                        && self.image_desc(dlss.output)
                            == Some(ImageDesc {
                                extent: self.output_extent(),
                                format: ImageFormat::Rgba32Float,
                            }),
                    "incompatible DLSS image inputs"
                );
            }
            _ => (),
        }
        if let Op::Draw {
            count: DrawCount::Indirect(r),
            ..
        } = p.op
        {
            ensure!(
                matches!(self.resources.get(r.0),Some((_,Storage::Buffer{bytes,..})) if *bytes>=16),
                "invalid indirect draw buffer"
            );
        }
        if let Op::ComputeIndirect { args, offset, .. }
        | Op::Draw {
            count: DrawCount::MeshIndirect { args, offset },
            ..
        } = p.op
        {
            ensure!(
                matches!(self.resources.get(args.0),Some((_,Storage::Buffer{bytes,..})) if offset.is_multiple_of(4) && offset.checked_add(12).is_some_and(|end|end<=*bytes as u64)),
                "invalid indirect argument range"
            );
        }
        if let Op::Draw {
            ref program, count, ..
        } = p.op
        {
            ensure!(
                matches!(
                    (program.shader, count),
                    (_, DrawCount::Indirect(_))
                        | (Shader::Mesh(..), DrawCount::MeshIndirect { .. })
                        | (Shader::Mesh(..), DrawCount::Mesh(_))
                        | (Shader::Graphics(..), DrawCount::Vertices(_))
                ),
                "draw arguments do not match shader"
            );
        }
        if let Op::Draw { color, depth, .. } = p.op {
            let color = self.image_desc(color).context("draw needs a color image")?;
            let depth = self.image_desc(depth).context("draw needs a depth image")?;
            ensure!(
                color.extent == depth.extent
                    && color.extent.iter().all(|&n| n > 0)
                    && color.format == ImageFormat::Visibility
                    && depth.format == ImageFormat::Depth,
                "incompatible draw attachments"
            );
        }
        if let Op::Present(r) = p.op {
            ensure!(
                matches!(self.resources.get(r.0), Some((_, Storage::Buffer { .. }))),
                "presentation needs a pixel buffer"
            );
        }
        if let Op::Counters {
            buffer,
            first,
            ref labels,
        } = p.op
        {
            ensure!(
                matches!(self.resources.get(buffer.0),Some((_,Storage::Buffer{bytes,..})) if first.checked_add(labels.len()).is_some_and(|words|words<=bytes/4)),
                "invalid counter range"
            );
        }
        if let Op::ReadPixels { image, buffer } | Op::WritePixels { image, buffer } = p.op {
            let image = self
                .image_desc(image)
                .context("pixel copy needs an image")?;
            let pixels = u64::from(image.extent[0]) * u64::from(image.extent[1]);
            ensure!(
                matches!(self.resources.get(buffer.0),Some((_,Storage::Buffer{bytes:n,..})) if *n as u64 / image.format.bytes_per_pixel() >= pixels),
                "undersized pixel copy buffer"
            );
        }
        Ok(())
    }
}

impl Storage {
    pub(super) fn permits(&self, a: Access) -> bool {
        match self {
            Storage::Color
            | Storage::Image(ImageDesc {
                format: ImageFormat::Visibility,
                ..
            }) => matches!(
                a,
                Access::ColorClear
                    | Access::ColorLoad
                    | Access::CopySrc
                    | Access::CopyDst
                    | Access::ComputeSample
                    | Access::SdkImageRead
            ),
            Storage::Depth
            | Storage::Image(ImageDesc {
                format: ImageFormat::Depth,
                ..
            }) => matches!(
                a,
                Access::DepthClear
                    | Access::DepthLoad
                    | Access::CopySrc
                    | Access::CopyDst
                    | Access::ComputeSample
                    | Access::SdkImageRead
            ),
            Storage::Image(ImageDesc {
                format: ImageFormat::Rgba32Float | ImageFormat::Rg16Float,
                ..
            }) => matches!(
                a,
                Access::ComputeSample
                    | Access::SdkImageRead
                    | Access::SdkImageWrite
                    | Access::CopySrc
                    | Access::CopyDst
            ),
            Storage::Buffer { .. } => !matches!(
                a,
                Access::ColorLoad
                    | Access::ColorClear
                    | Access::DepthLoad
                    | Access::DepthClear
                    | Access::AccelerationWrite
                    | Access::AccelerationRead
                    | Access::RayAcceleration
                    | Access::ComputeSample
                    | Access::SdkImageRead
                    | Access::SdkImageWrite
            ),
            Storage::Acceleration(_) => matches!(
                a,
                Access::AccelerationWrite | Access::AccelerationRead | Access::RayAcceleration
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_pipeline_must_support_the_referenced_cluster_bottom() {
        let mut graph = Graph::new([1, 1]);
        let bottom = graph.resource(
            "cluster BLAS",
            Storage::Acceleration(AccelerationShape::ClusterBottom { count: 1 }),
        );
        let tlas = graph.resource(
            "TLAS",
            Storage::Acceleration(AccelerationShape::Instances { count: 1 }),
        );
        let mut sources = BTreeMap::from([(tlas, BTreeSet::from([bottom]))]);
        for shader in [
            Shader::RayTracing(&[], &[], &[]),
            Shader::ClusterShadows(&[], &[], &[]),
        ] {
            let index = graph.pass(
                "probe",
                Op::TraceRays {
                    extent: [1, 1],
                    tlas,
                    blases: vec![bottom],
                    program: Dispatch {
                        shader,
                        bindings: vec![],
                        push: vec![],
                    },
                },
            );
            let result = graph.validate_pass(&graph.passes[index], &[true; 2], &mut sources);
            assert_eq!(result.is_ok(), shader.supports_clusters());
        }
    }

    #[test]
    fn blas_version_must_reference_a_buffer_before_preparing_the_frame() {
        let mut graph = Graph::new([1, 1]);
        let vertices = graph.input("vertices", vec![0; 96]);
        let indices = graph.input("indices", vec![0; 12]);
        let output = graph.resource(
            "BLAS",
            Storage::Acceleration(AccelerationShape::Triangles {
                count: 1,
                vertices: 3,
            }),
        );
        graph.pass(
            "build",
            Op::BuildBlas {
                vertices,
                indices,
                topology: Resource(usize::MAX),
                output,
            },
        );
        assert!(
            graph
                .compile()
                .err()
                .unwrap()
                .to_string()
                .contains("topology")
        );
    }
}
