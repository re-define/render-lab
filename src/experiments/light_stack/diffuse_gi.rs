//! Diffuse screen probes and the two-bounce world radiance cache from GI-1.2.
//! Separate RDG passes make cache publication and one-frame feedback explicit.
use crate::render::FrameGraph;
use crate::render::Snapshot;
use crate::render::rays::RayScene;
use crate::{
    rdg::{Access, Dispatch, Op, PassPhase, Resource, Shader},
    render::{Atlas, rays},
};
use Access::{ComputeRead as R, ComputeReadWrite as RW, ComputeWrite as W};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::hash::{DefaultHasher, Hash, Hasher};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(in crate::experiments) enum DiffuseGi {
    #[default]
    Direct,
    Single,
    Multi,
    Uncached,
}

struct Inputs<'a> {
    pub scene: &'a RayScene,
    pub raster: [Resource; 3],
    pub surface: Resource,
    pub config: Resource,
    pub lights: Resource,
    pub history: Resource,
    pub old_pixels: Resource,
}
macro_rules! shader {
    ($name:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/gi12_", $name, ".comp.spv"))
    };
}
#[cfg(test)]
mod tests;

const CELLS: u32 = 131072;
fn dispatch(
    g: &mut FrameGraph,
    name: &str,
    shader: &'static [u8],
    bindings: Vec<(Resource, Access)>,
    groups: [u32; 3],
) {
    g.pass_in(
        PassPhase::Lighting,
        name,
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(shader),
                bindings,
                push: Vec::new(),
            },
            groups,
        },
    );
}
pub(in crate::experiments) fn stage(s: &Snapshot, mode: DiffuseGi) -> super::AddLight<'_> {
    use super::{AddLight, Contribution, Signal};
    if mode == DiffuseGi::Direct {
        return AddLight::new(
            "Disabled indirect diffuse",
            &[],
            Signal::IndirectDiffuse,
            |g, _, _| {
                Ok(Contribution::IndirectDiffuse(
                    g.input("Disabled indirect diffuse", vec![0; 16]),
                ))
            },
        );
    }
    AddLight::new(
        "Diffuse GI",
        &[Signal::Surfaces],
        Signal::IndirectDiffuse,
        move |g, _, products| {
            let scene = products.surfaces()?;
            let inputs = Inputs {
                scene: scene.material_hits.as_ref().unwrap_or(&scene.occluders),
                raster: scene.raster,
                surface: scene.current,
                config: scene.config,
                lights: scene.lights,
                history: scene.history,
                old_pixels: scene.indices,
            };
            Ok(Contribution::IndirectDiffuse(build(
                g,
                s,
                mode,
                scene.history_key,
                inputs,
            )?))
        },
    )
}

fn build(
    g: &mut FrameGraph,
    s: &Snapshot,
    mode: DiffuseGi,
    key: u64,
    i: Inputs,
) -> Result<Resource> {
    let [w, h] = g.extent;
    let tiles = [w.div_ceil(8), h.div_ceil(8), 1];
    let probes = tiles[0] * tiles[1];
    let (materials, signature) = Atlas::gi_hit_table(&s.scene, g)?;
    let atlas = g
        .products
        .material_atlas
        .expect("scene atlas registered before lighting");
    let paths = rays::buffer(g, "GI-1.2 diffuse paths", probes as usize * 8 * 96);
    let current = rays::buffer(g, "GI-1.2 current probes", probes as usize * 48);
    let probe_groups = [probes.div_ceil(64), 1, 1];
    if mode == DiffuseGi::Uncached {
        // The common ray-pipeline ABI has two cache slots. This mode never reads
        // or writes them, and allocates no world grid, scratch grid or probe history.
        let cache = g.input("Unused world cell", vec![0; 112]);
        let scratch = g.input("Unused world sample", vec![0; 48]);
        trace(g, &i, [materials, atlas], [cache, scratch, paths], tiles)?;
        dispatch(
            g,
            "Uncached GI: average first-hit radiance",
            shader!("uncached"),
            vec![(i.surface, R), (paths, R), (current, W)],
            probe_groups,
        );
        return Ok(resolve(g, &i, current, tiles));
    }
    let mut history = |name: &str, bytes: usize| {
        let mut hash = DefaultHasher::new();
        (name, key, signature, mode).hash(&mut hash);
        let id = g.history_buffer(name, hash.finish(), bytes);
        g.prefer_local_buffer(id);
        id
    };
    let cache = history("GI-1.2 world radiance cells", CELLS as usize * 112);
    let old = history("GI-1.2 reprojected probes", probes as usize * 48);
    let scratch = rays::buffer(g, "GI-1.2 world samples", CELLS as usize * 48);
    let cells = [CELLS.div_ceil(64), 1, 1];
    dispatch(
        g,
        "GI-1.2 clear samples and expire stale cells",
        shader!("clear"),
        vec![(cache, RW), (scratch, W), (i.history, R)],
        cells,
    );
    trace(g, &i, [materials, atlas], [cache, scratch, paths], tiles)?;
    dispatch(
        g,
        "GI-1.2 resolve direct and delayed second bounce",
        shader!("cache"),
        vec![(cache, RW), (scratch, R), (i.history, R), (paths, R)],
        cells,
    );
    dispatch(
        g,
        "GI-1.2 integrate probes and transfer second hits",
        shader!("probe"),
        vec![
            (i.config, R),
            (i.surface, R),
            (paths, R),
            (cache, RW),
            (i.old_pixels, R),
            (old, R),
            (current, W),
        ],
        probe_groups,
    );
    dispatch(
        g,
        "GI-1.2 store probe history",
        shader!("store"),
        vec![(current, R), (old, W)],
        probe_groups,
    );
    Ok(resolve(g, &i, current, tiles))
}

fn trace(
    g: &mut FrameGraph,
    i: &Inputs,
    [materials, atlas]: [Resource; 2],
    [cache, scratch, paths]: [Resource; 3],
    tiles: [u32; 3],
) -> Result<()> {
    let mut trace_inputs = vec![
        (i.surface, R),
        (i.lights, R),
        (i.config, R),
        (i.history, R),
        (materials, R),
    ];
    trace_inputs.extend(i.scene.probe_inputs(atlas)?.into_iter().map(|r| (r, R)));
    trace_inputs.extend([(cache, RW), (scratch, RW), (paths, W)]);
    if let RayScene::Hardware(native) = i.scene {
        if native.inline_queries {
            g.pass_in(
                crate::rdg::PassPhase::Lighting,
                "GI-1.2 trace inline probe paths",
                native.inline(tiles, shader!("inline"), trace_inputs),
            );
        } else {
            let bindings = trace_inputs
                .into_iter()
                .map(|(r, a)| {
                    (
                        r,
                        match a {
                            R => Access::RayRead,
                            W => Access::RayWrite,
                            RW => Access::RayReadWrite,
                            _ => unreachable!(),
                        },
                    )
                })
                .collect();
            native.probes(g, [tiles[0] * 8, tiles[1]], bindings, i.raster);
        }
    } else {
        dispatch(
            g,
            "GI-1.2 trace diffuse probe paths",
            shader!("trace"),
            trace_inputs,
            tiles,
        );
    }
    Ok(())
}

fn resolve(g: &mut FrameGraph, i: &Inputs, current: Resource, tiles: [u32; 3]) -> Resource {
    let [w, h] = g.extent;
    let result = rays::buffer(g, "GI-1.2 indirect diffuse", w as usize * h as usize * 16);
    dispatch(
        g,
        "GI-1.2 interpolate indirect diffuse",
        shader!("resolve"),
        vec![(i.config, R), (i.surface, R), (current, R), (result, W)],
        tiles,
    );
    result
}
