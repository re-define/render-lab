use super::*;
use crate::{
    rdg::AccelerationShape,
    render::rays::{self, ClusterShadow},
    scene::LightKind,
};

pub(super) fn caster_directions(s: &Snapshot) -> ([[f32; 4]; 2], f32) {
    let mut rays = [[0.; 4]; 2];
    let lights = &s.scene.lights.entries;
    if lights.len() > 2 || lights.iter().any(|l| l.kind != LightKind::Directional) {
        return (rays, -1.);
    }
    for (dst, light) in rays.iter_mut().zip(lights) {
        *dst = light
            .direction
            .extend(light.angular_radius_degrees.to_radians().sin())
            .to_array();
    }
    (rays, lights.len() as f32)
}
pub(super) fn publish(
    g: &mut FrameGraph,
    s: &ClusterScene,
    cut: Work,
    proxy_start: usize,
    [layout, models]: [Resource; 2],
    force_rebuild: bool,
) -> Result<()> {
    let cut = if s.voxel_capacity != 0 {
        let pages = Work::new(g, "Selected triangle CLAS pages", s.shadow_capacity, 1);
        g.pass_in(
            PassPhase::Culling,
            "Expand selected voxel clusters into CLAS pages",
            Op::ComputeIndirect {
                program: compute(
                    spirv!("brick_clas.comp"),
                    &[
                        (s.assets, R),
                        (s.instances, R),
                        (cut.items, R),
                        (pages.items, RW),
                        (pages.args, RW),
                    ],
                    vec![],
                ),
                args: cut.args,
                offset: 0,
            },
        );
        pages
    } else {
        cut
    };
    let count = s
        .shadow_capacity
        .checked_add(1)
        .context("shadow list exceeds 32 bits")?;
    let [vertices, triangles] = s.shadow_elements.map(u32::try_from);
    let (vertices, triangles) = (vertices?, triangles?);
    ensure!(
        u64::from(vertices) * 12 <= u64::from(u32::MAX)
            && u64::from(triangles) * 4 <= u64::from(u32::MAX),
        "cluster vertex or index stream exceeds shader address-offset range"
    );
    let clusters = g.resource(
        "Triangle CLAS",
        Storage::Acceleration(AccelerationShape::TriangleClusters {
            count,
            vertices,
            triangles,
        }),
    );
    let versions = rays::buffer(g, "CLAS cache versions", count as usize * 32);
    let parameters = g.input("CLAS cache parameters", vec![0; 16]);
    g.cluster_caches.insert(
        versions,
        crate::rdg::ClusterCache {
            output: clusters,
            parameters,
            sources: s.shadow_sources.clone(),
            force_rebuild,
        },
    );
    let ranges = g.input(
        "CLAS per-prototype ranges",
        bytemuck::cast_slice(&s.shadow_ranges).to_vec(),
    );
    let points = rays::buffer(g, "Posed CLAS vertices", vertices as usize * 12);
    let faces = rays::buffer(g, "Selected cluster faces", triangles as usize * 4);
    let records = rays::buffer(g, "Triangle cluster build records", count as usize * 64);
    let coverage = rays::buffer(g, "Cluster transition intervals", count as usize * 16);
    let addresses = rays::buffer(g, "Built CLAS addresses", count as usize * 8);
    let destinations = rays::buffer(g, "Changed CLAS destinations", count as usize * 8);
    let changes = g.input(
        "Changed CLAS count and geometry offsets",
        bytemuck::cast_slice(&[1u32; 4]).to_vec(),
    );
    let bottom_record = rays::buffer(g, "Cluster BLAS build record", 16);
    let pointers = g.build_addresses(vec![points, faces, addresses]);
    let counters = g.input(
        "Mega Geometry counters",
        bytemuck::cast_slice(&[0u32, 1, 1, 0, 0, 0, 0, 0, 0, 0]).to_vec(),
    );
    let palette = s.palette.unwrap_or(s.view);
    let mut buffers = vec![
        (s.assets, R),
        (s.instances, R),
        (s.view, R),
        (cut.items, R),
        (palette, R),
        (points, W),
        (faces, W),
        (records, W),
        (coverage, W),
        (counters, RW),
        (pointers, R),
        (ranges, R),
        (layout, R),
        (models, R),
        (versions, RW),
        (parameters, R),
        (destinations, W),
        (addresses, W),
        (changes, RW),
    ];
    g.pass_in(
        PassPhase::Lighting,
        "Pose selected shadow clusters",
        Op::ComputeIndirect {
            program: compute(spirv!("mega_clusters.comp"), &buffers, vec![]),
            args: cut.args,
            offset: 0,
        },
    );
    for b in &mut buffers[5..9] {
        b.1 = RW;
    }
    // Finalization only emits the degenerate sentinel and BLAS header.
    for resource in &mut buffers[16..18] {
        resource.1 = RW;
    }
    buffers.push((bottom_record, W));
    g.pass_in(
        PassPhase::Lighting,
        "Finalize cluster build records",
        Op::Compute {
            program: compute(spirv!("mega_finalize.comp"), &buffers, vec![]),
            groups: [1, 1, 1],
        },
    );

    g.pass(
        "Build selected triangle CLAS",
        Op::BuildClusters {
            records,
            count: changes,
            vertices: points,
            indices: faces,
            addresses: destinations,
            output: clusters,
        },
    );
    let blas = g.resource(
        "Forest cluster BLAS",
        Storage::Acceleration(AccelerationShape::ClusterBottom { count }),
    );
    g.pass(
        "Build forest cluster BLAS",
        Op::BuildClusterBottom {
            record: bottom_record,
            addresses,
            clusters,
            output: blas,
        },
    );
    g.pass(
        "Shadow cut statistics",
        Op::Counters {
            buffer: counters,
            first: 0,
            labels: vec![
                "CLAS including empty sentinel",
                "CLAS vertices including sentinel",
                "CLAS triangles including sentinel",
                "Offscreen shadow clusters",
                "Transition shadow clusters",
                "Voxel shadow clusters",
                "Reused CLAS",
                "New CLAS",
                "Transform invalidated CLAS",
                "Pose invalidated CLAS",
            ],
        },
    );
    g.products.cluster_shadow = Some(ClusterShadow {
        blas,
        metadata: coverage,
        view: s.view,
        proxy_start,
        proxy_end: g.products.ray_surfaces.len(),
    });
    Ok(())
}
