use super::*;
use crate::{
    experiments::lighting::geometry,
    rdg::{Access, Dispatch, Op, PassPhase, Resource, Shader, Storage},
    render::{ResolveInputs, rays::RayScene},
};
use Access::{ComputeRead as R, ComputeReadWrite as RW, ComputeWrite as W};
use bytemuck::{Pod, Zeroable};

use std::hash::{DefaultHasher, Hash, Hasher};

macro_rules! spirv {
    ($shader:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/", $shader, ".comp.spv"))
    };
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct Uniform {
    reprojection: [f32; 4],
    options: [u32; 4],
    viewport: [u32; 4],
    controls: [f32; 4],
    canopy_sky: [f32; 4],
}

fn pixel_buffer(g: &mut FrameGraph, name: &str, stride: usize) -> Resource {
    let [w, h] = g.extent;
    let buffer = g.resource(
        name,
        Storage::Buffer {
            bytes: w as usize * h as usize * stride,
            initial: None,
        },
    );
    g.prefer_local_buffer(buffer);
    buffer
}

fn pass(
    g: &mut FrameGraph,
    phase: PassPhase,
    name: &str,
    shader: &'static [u8],
    bindings: Vec<(Resource, Access)>,
    push: Vec<u8>,
) {
    let [w, h] = g.extent;
    g.pass_in(
        phase,
        name,
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(shader),
                bindings,
                push,
            },
            groups: [w.div_ceil(8), h.div_ceil(8), 1],
        },
    );
}

#[cfg(test)]
pub(super) fn build(s: &Snapshot, technique: &Restir, cfg: Settings) -> Result<FrameGraph> {
    let surfaces = crate::render::Forward::default().posed_surfaces(s, |_| true)?;
    let layer = technique.prepare_with(s, &surfaces, cfg)?;
    super::super::light_stack::build(s, surfaces, vec![layer], false)
}

#[derive(Default)]
pub(in crate::experiments) struct ShadowCache {
    rigid: geometry::Cache,
}

impl ShadowCache {
    pub(in crate::experiments) fn get(
        &self,
        scene: &crate::render::RenderScene,
        surfaces: &[crate::render::PosedSurface<'_>],
    ) -> Result<Arc<geometry::Packed>> {
        let rigid = geometry::shadow_casters(surfaces)
            .filter(|s| s.instance.palette.is_none())
            .collect::<Vec<_>>();
        self.rigid.get(scene, &rigid, geometry::Packing::Shadow)
    }
}

pub(in crate::experiments) fn add_light(
    g: &mut FrameGraph,
    s: &Snapshot,
    technique: &Restir,
    cfg: Settings,
    ResolveInputs {
        ids,
        objects,
        addresses,
        camera,
        depth,
        motion,
    }: ResolveInputs,
    rigid: Option<&geometry::Packed>,
) -> Result<(Resource, Resource, Resource)> {
    let [w, h] = s.extent;
    let authored = cfg.scene_lights(s);
    let amount = if authored {
        u32::try_from(s.scene.lights.entries.len())?
    } else {
        cfg.lights
    };
    ensure!(
        amount <= LIGHT_MAX,
        "ReSTIR supports up to {LIGHT_MAX} active light instances"
    );
    let normalization = if authored { 1. } else { 1. / amount as f32 };
    let motion = motion.filter(|_| cfg.uses_history());
    let jitter = s.temporal.as_ref().map_or([0.; 2], |f| f.jitter_delta);
    let scene = RayScene::build(
        g,
        cfg.use_hardware(s.scene.setting(crate::experiments::vegetation::SETTING)),
        cfg.indirect(),
        rigid.map(|p| [p.acceleration.clone(), p.triangles.clone()]),
    )?;
    let config = Uniform {
        reprojection: [jitter[0], jitter[1], 0., 0.],
        options: [
            cfg.sampling as u32,
            amount,
            cfg.candidates,
            cfg.reference_samples,
        ],
        viewport: [
            w,
            h,
            match &scene {
                RayScene::Software { deformed, .. } => deformed.count,
                RayScene::Hardware(_) => 0,
            },
            u32::from(motion.is_some()),
        ],
        controls: [
            cfg.ev.exp2(),
            cfg.denoise as u32 as f32,
            normalization,
            cfg.gi as u32 as f32,
        ],
        canopy_sky: if authored {
            s.scene.lights.diffuse_sky
        } else {
            [0.; 4]
        },
    };
    let config = g.input("ReSTIR controls", bytemuck::bytes_of(&config).to_vec());
    let lights = if authored {
        g.resident_input("ReSTIR scene light instances", s.scene.lights.bytes.clone())
    } else {
        g.resident_input("Point light table", technique.lights.clone())
    };
    let current = pixel_buffer(g, "World normal, position and instance ID", 32);
    pass(
        g,
        PassPhase::Lighting,
        "Reconstruct visible surfaces",
        spirv!("restir_surface"),
        vec![
            (ids, Access::ComputeSample),
            (objects, R),
            (current, W),
            (addresses, R),
            (camera, R),
        ],
        vec![],
    );
    let mut key = DefaultHasher::new();
    (
        technique.generation,
        motion.and(s.temporal.as_ref()).map(|f| f.generation_key()),
        cfg.sampling as u32,
        g.products.cluster_shadow.is_some(),
        cfg.rays,
        cfg.denoise as u32,
        amount,
        authored.then_some(s.scene.lights.signature),
        s.scene
            .setting(crate::experiments::vegetation::SETTING)
            .and_then(|v| v.get("coarse_shadows"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        cfg.candidates,
        cfg.reference_samples,
    )
        .hash(&mut key);
    // std430 Previous occupies 64 bytes. Stateless sampling only needs the seed counter.
    let history = g.history_buffer(
        "Previous reservoirs and radiance moments",
        key.finish(),
        16 + if motion.is_some() {
            w as usize * h as usize * 64
        } else {
            0
        },
    );
    g.prefer_local_buffer(history);
    let indices = if let Some(samples) = motion {
        let indices = pixel_buffer(g, "Validated previous pixel indices", 4);
        let mut bindings = vec![
            (current, R),
            (config, R),
            (history, R),
            (samples, R),
            (indices, W),
        ];
        let stats = cfg
            .history_stats
            .then(|| g.input("Reprojection counters", vec![0; 16]));
        if let Some(stats) = stats {
            bindings.push((stats, RW));
        }
        pass(
            g,
            PassPhase::Motion,
            "Validate ReSTIR surface correspondence",
            if stats.is_some() {
                spirv!("restir_reproject_stats")
            } else {
                spirv!("restir_reproject")
            },
            bindings,
            vec![],
        );
        if let Some(stats) = stats {
            g.pass(
                "Inspect ReSTIR reprojection",
                Op::Counters {
                    buffer: stats,
                    first: 0,
                    labels: vec![
                        "ReSTIR surface pixels",
                        "ReSTIR history accepted",
                        "ReSTIR moving pixels",
                        "ReSTIR moving history accepted",
                    ],
                },
            );
        }
        indices
    } else {
        camera
    }; // Unread binding when viewport.w is zero.
    let initial = if matches!(cfg.sampling, Sampling::Ris | Sampling::Restir) {
        let initial = pixel_buffer(g, "Initial and temporal reservoirs", 16);
        let name = if cfg.sampling == Sampling::Restir {
            "ReSTIR: initial and temporal resampling"
        } else {
            "RIS: initial resampling"
        };
        pass(
            g,
            PassPhase::Lighting,
            name,
            spirv!("restir_initial"),
            vec![
                (current, R),
                (lights, R),
                (config, R),
                (history, R),
                (initial, W),
                (indices, R),
            ],
            vec![],
        );
        initial
    } else {
        g.input("Empty reservoir", vec![0; 16])
    };
    let selected = if cfg.sampling == Sampling::Restir {
        let spatial = pixel_buffer(g, "Spatially resampled reservoirs", 16);
        pass(
            g,
            PassPhase::Lighting,
            "ReSTIR: spatial resampling",
            spirv!("restir_spatial"),
            vec![
                (current, R),
                (lights, R),
                (config, R),
                (history, R),
                (initial, R),
                (spatial, W),
            ],
            vec![],
        );
        spatial
    } else {
        initial
    };
    let raw = pixel_buffer(g, "Raw diffuse direct light", 32);
    let visibility = if cfg.sampling == Sampling::Reference {
        "Stratified direct-light reference"
    } else {
        "Trace one shadow ray per sampled pixel"
    };
    match &scene {
        RayScene::Hardware(n) => {
            let read = Access::RayRead;
            n.shadows(
                g,
                vec![
                    (current, read),
                    (lights, read),
                    (config, read),
                    (history, read),
                    (selected, read),
                    (raw, Access::RayWrite),
                ],
            );
        }
        RayScene::Software {
            nodes,
            triangles,
            deformed: dynamic,
        } => {
            pass(
                g,
                PassPhase::Lighting,
                visibility,
                spirv!("restir_light"),
                vec![
                    (current, R),
                    (lights, R),
                    (config, R),
                    (history, R),
                    (selected, R),
                    (*nodes, R),
                    (*triangles, R),
                    (raw, W),
                    (dynamic.nodes, R),
                    (dynamic.triangles, R),
                ],
                vec![],
            );
        }
    }
    let accumulated = if cfg.denoise == Denoise::Svgf {
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
            (motion.unwrap_or(camera), R),
            (depth, Access::ComputeSample),
        ],
        vec![],
    );
    let mut output = accumulated;
    let steps = match cfg.denoise {
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
    let indirect = if cfg.gi == DiffuseGi::Direct {
        g.input("Disabled indirect diffuse", vec![0; 16])
    } else {
        diffuse_gi::build(
            g,
            s,
            cfg.gi,
            key.finish(),
            diffuse_gi::Inputs {
                scene,
                raster: [objects, addresses, camera],
                surface: current,
                config,
                lights,
                history,
                old_pixels: indices,
            },
        )?
    };
    Ok((output, config, indirect))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires Vulkan descriptor heap"]
    fn backlit_leaf_transmits_both_point_sun_and_solar_disk() -> Result<()> {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        let mut gpu = crate::rhi::Backend::open()?;
        for disk_radius in [0.0f32, 0.02] {
            // Read the first two 32-byte Radiance records through a 16-pixel readback.
            let mut g = FrameGraph::new([16, 1]);
            let mut receivers = [0u32; 16 * 8];
            for (i, sky) in [1f32, -2.].into_iter().enumerate() {
                receivers[i * 8 + 6] = sky.to_bits(); // oct (0,0) faces +Z
                receivers[i * 8 + 7] = (i + 1) as u32;
            }
            let surface = g.input(
                "Two backlit receivers",
                bytemuck::cast_slice(&receivers).to_vec(),
            );
            let lamp = [
                f32::from_bits(1),
                0.,
                0.,
                0.,
                0.,
                0.,
                0.,
                0.,
                0.,
                0.,
                -1.,
                disk_radius,
                1.,
                1.,
                1.,
                0.,
            ];
            let lights = g.input(
                "Sun behind both surfaces",
                bytemuck::cast_slice(&lamp).to_vec(),
            );
            let config = g.input(
                "Reference lighting",
                bytemuck::bytes_of(&Uniform {
                    options: [Sampling::Reference as u32, 1, 1, 1],
                    viewport: [16, 1, 0, 0],
                    controls: [1., 0., 1., 0.],
                    ..Uniform::zeroed()
                })
                .to_vec(),
            );
            let clock = g.input("Light sampling clock", vec![0; 16]);
            let selected = g.input("Inactive reservoirs", vec![0; 256]);
            let triangles = g.input("Rigid triangles", vec![0; 128]);
            let dynamic_triangles = g.input("Deformed triangles", vec![0; 128]);
            let empty_bounds = [1f32, 0., 0., f32::from_bits(1), -1., 0., 0., 0.];
            let nodes = g.input(
                "Empty BVH root",
                bytemuck::cast_slice(&empty_bounds).to_vec(),
            );
            let dynamic_nodes = g.input(
                "Empty deformed root",
                bytemuck::cast_slice(&empty_bounds).to_vec(),
            );
            let raw = pixel_buffer(&mut g, "Direct radiance", 32);
            pass(
                &mut g,
                PassPhase::Lighting,
                "Trace backlight",
                spirv!("restir_light"),
                vec![
                    (surface, R),
                    (lights, R),
                    (config, R),
                    (clock, R),
                    (selected, R),
                    (nodes, R),
                    (triangles, R),
                    (raw, W),
                    (dynamic_nodes, R),
                    (dynamic_triangles, R),
                ],
                vec![],
            );
            g.set_output(raw);
            let frame = gpu.execute(g.compile()?, &pool)?;
            let component =
                |i: usize| f32::from_ne_bytes(frame.pixels[4 * i..4 * i + 4].try_into().unwrap());
            assert_eq!([component(0), component(1), component(2)], [0.; 3]);
            assert!(
                component(9) > 0.48 && component(9) > component(8) && component(8) > component(10)
            );
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires Vulkan descriptor heap"]
    fn reprojection_selects_the_previous_pixel_and_rejects_invalid_surfaces() -> Result<()> {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        let mut gpu = crate::rhi::Backend::open()?;
        let draw = |gpu: &mut crate::rhi::Backend, warm: bool| -> Result<Vec<u32>> {
            let mut g = FrameGraph::new([8, 1]);
            // Mirror std430 Surface, Previous and MotionSample using native 32-bit words.
            let mut surfaces = vec![0u32; 8 * 8];
            let mut previous = vec![0u32; 4 + 16 * 8];
            let mut samples = Vec::<u32>::new();
            previous[0] = u32::from(warm);
            let tags = [11, 33, 14, 15, 44, 55, 16, 17];
            for (i, &tag) in tags.iter().enumerate() {
                surfaces[i * 8 + 5] = 1f32.to_bits();
                surfaces[i * 8 + 7] = u32::from(i != 7);
                let base = 4 + i * 16;
                previous[base + 1] = (if i == 0 { -1f32 } else { 1f32 }).to_bits();
                previous[base + 3] = tag;
                previous[base + 15] = (if i == 4 { 0.2f32 } else { 0.5f32 }).to_bits();
                let (dx, expected_tag, depth): (f32, u32, f32) = [
                    (0.1, 33, 0.5),
                    (2.4, 44, 0.5),
                    (-2.6, 11, 0.5),
                    (-10., 15, 0.5),
                    (-0.6, 0, 0.5),
                    (-0.6, 55, 0.5),
                    (-0.6, 16, -1.),
                    (-0.6, 17, 0.5),
                ][i];
                samples.extend([dx.to_bits(), 0, depth.to_bits(), expected_tag, tag, 1]);
            }
            let current = g.input("Current", bytemuck::cast_slice(&surfaces).to_vec());
            let config = g.input(
                "View",
                bytemuck::bytes_of(&Uniform {
                    reprojection: [0.6, 0., 0., 0.],
                    options: [0; 4],
                    viewport: [8, 1, 0, 1],
                    controls: [0.; 4],
                    canopy_sky: [0.; 4],
                })
                .to_vec(),
            );
            let history = g.input("Previous", bytemuck::cast_slice(&previous).to_vec());
            let motion = g.input("Motion", bytemuck::cast_slice(&samples).to_vec());
            let indices = pixel_buffer(&mut g, "Indices", 4);
            pass(
                &mut g,
                PassPhase::Motion,
                "Check correspondence",
                spirv!("restir_reproject"),
                vec![
                    (current, R),
                    (config, R),
                    (history, R),
                    (motion, R),
                    (indices, W),
                ],
                vec![],
            );
            g.set_output(indices);
            let frame = gpu.execute(g.compile()?, &pool)?;
            Ok(frame
                .pixels
                .chunks_exact(4)
                .map(|b| u32::from_ne_bytes(b.try_into().unwrap()))
                .collect())
        };
        let invalid = u32::MAX;
        assert_eq!(draw(&mut gpu, false)?, [invalid; 8]);
        assert_eq!(
            draw(&mut gpu, true)?,
            [1, invalid, invalid, invalid, invalid, 5, invalid, invalid]
        );
        Ok(())
    }
}
