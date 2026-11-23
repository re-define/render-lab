//! Expand stable scene slots on the GPU into the ABI shared with ordinary visibility draws.
use super::*;
use crate::render::SurfacePrefix;
pub(super) fn prepare(
    g: &mut FrameGraph,
    t: &mut Targets,
    s: &Snapshot,
    assets: &[[u32; 8]],
    count: u32,
    wind: Option<Resource>,
) -> Result<(Resource, Option<Resource>)> {
    let routes = &s.scene.routes;
    let layout = g.resident_input("Cluster slot bindings", routes.bindings.clone());
    let (current, mat) = t.scene_tables(g, s)?;
    let temporal = s.temporal.as_ref();
    let previous = temporal.and_then(|f| f.previous_scene.as_ref());
    let prior = previous.map(|p| &p.models);
    let old = if let Some(prior) = prior {
        g.table("Prior instance slots", prior)
    } else {
        g.input("Empty previous slot", vec![0; 80])
    };
    let metadata = g.input(
        "Cluster asset surface bindings",
        bytemuck::cast_slice(assets).to_vec(),
    );
    let mut buffer = |name: &str, n: usize| {
        let r = g.resource(
            name,
            Storage::Buffer {
                bytes: n.max(16),
                initial: None,
            },
        );
        g.prefer_local_buffer(r);
        r
    };
    let total = s.scene.instances.len();
    let cull = buffer("Cluster instances", count as usize * 128);
    let shading = buffer("Cluster and ordinary surfaces", total * 144);
    let motion = buffer(
        "Cluster and ordinary motion",
        if temporal.is_some() { total * 80 } else { 16 },
    );
    let old_plants = buffer(
        "Previous plant transforms",
        if wind.is_some() {
            count as usize * 80
        } else {
            16
        },
    );
    let palette = wind.map(|r| t.motion_address(r)).transpose()?.unwrap_or(0);
    let push = [
        count,
        u32::from(temporal.is_some()),
        u32::from(previous.is_some()),
        u32::from(wind.is_some()),
        palette,
    ];
    g.pass_in(
        PassPhase::Raster,
        "Resolve resident cluster slots",
        Op::Compute {
            program: compute(
                spirv!("cluster_tables.comp"),
                &[
                    (layout, R),
                    (current, R),
                    (old, R),
                    (mat, R),
                    (metadata, R),
                    (cull, W),
                    (shading, W),
                    (motion, W),
                    (old_plants, W),
                ],
                bytemuck::bytes_of(&push).to_vec(),
            ),
            groups: [
                count.div_ceil(64).min(32768),
                count.div_ceil(64).div_ceil(32768),
                1,
            ],
        },
    );
    t.prefix = Some(SurfacePrefix {
        count,
        shade: shading,
        motion: temporal.map(|_| motion),
    });
    Ok((cull, wind.map(|_| old_plants)))
}
