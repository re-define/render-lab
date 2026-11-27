use super::{Samples, SurfaceLighting, pass, pixel_buffer};
use crate::{
    experiments::light_stack::{AddLight, Contribution, Signal},
    rdg::{Access, PassPhase, Resource},
    render::FrameGraph,
};
use Access::{ComputeRead as R, ComputeReadWrite as RW, ComputeWrite as W};
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(u32)]
pub(in crate::experiments) enum Denoise {
    Off,
    Spatial,
    Svgf,
}

macro_rules! spirv {
    ($shader:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/", $shader, ".comp.spv"))
    };
}

pub(in crate::experiments) fn stage<'a>(mode: Denoise) -> AddLight<'a> {
    AddLight::new(
        "Direct-light resolve",
        &[Signal::Surfaces, Signal::RawDirect],
        Signal::Direct,
        move |g, _, products| {
            let samples = products.samples()?;
            let scene = products.surfaces()?;
            Ok(Contribution::Direct(
                resolve(g, scene, *samples, mode),
                scene.config,
            ))
        },
    )
}

fn resolve(
    g: &mut FrameGraph,
    scene: &SurfaceLighting,
    Samples { raw, initial }: Samples,
    mode: Denoise,
) -> Resource {
    let SurfaceLighting {
        current,
        config,
        history,
        indices,
        motion,
        depth,
        ..
    } = *scene;
    let accumulated = if mode == Denoise::Svgf {
        let filtered = pixel_buffer(g, "Temporally accumulated radiance and moments", 32);
        pass(
            g,
            PassPhase::Denoise,
            "SVGF: temporal reprojection and moments",
            spirv!("restir_temporal_filter"),
            vec![
                (current, R),
                (config, R),
                (history, R),
                (raw, R),
                (filtered, W),
                (indices, R),
            ],
            vec![],
        );
        filtered
    } else {
        raw
    };
    pass(
        g,
        PassPhase::Lighting,
        "Store results for the next rendered frame",
        spirv!("restir_commit"),
        vec![
            (current, R),
            (config, R),
            (initial, R),
            (accumulated, R),
            (history, RW),
            (motion, R),
            (depth, Access::ComputeSample),
        ],
        vec![],
    );
    let mut output = accumulated;
    let steps = match mode {
        Denoise::Off => &[][..],
        Denoise::Spatial => &[1i32, 2][..],
        Denoise::Svgf => &[1, 2, 4][..],
    };
    if !steps.is_empty() {
        let ping = pixel_buffer(g, "À-trous radiance A", 32);
        let pong = pixel_buffer(g, "À-trous radiance B", 32);
        for (i, stride) in steps.iter().enumerate() {
            let dest = if i % 2 == 0 { ping } else { pong };
            pass(
                g,
                PassPhase::Denoise,
                &format!("À-trous denoise step {stride}"),
                spirv!("restir_atrous"),
                vec![(current, R), (config, R), (output, R), (dest, W)],
                stride.to_ne_bytes().to_vec(),
            );
            output = dest;
        }
    }
    output
}
