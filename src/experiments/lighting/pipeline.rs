use super::*;
use crate::rdg::PassPhase;
use crate::{
    rdg::{Access, Dispatch, DrawCount, ImageDesc, ImageFormat, Op, Resource, Storage},
    render::{Atlas, FrameLighting, MaterialInputs, ResolveInputs, build_frame_with_lighting},
};
use Access::{ComputeRead as R, ComputeSample as S, ComputeWrite as W};
use bytemuck::{Pod, Zeroable};

const PROBE_EXTENT: [u32; 3] = [7, 5, 7];

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct Light {
    view_projection: [f32; 16],
    position: [f32; 4],
    right: [f32; 4],
    up: [f32; 4],
    forward: [f32; 4],
    lens: [f32; 4],
    modes: [u32; 4],
    probe_extent: [u32; 4],
}

impl Light {
    fn new(cfg: Settings) -> Self {
        let (position, target, energy) = match cfg.kind {
            Kind::Gi => (Vec3::new(0., 3.88, -0.4), Vec3::new(0., 0., -0.4), 11.),
            _ => (Vec3::new(-3.8, 7.0, 4.3), Vec3::new(0.0, 0., -4.0), 69.),
        };
        let forward = (target - position).normalize();
        let right = forward
            .cross(if cfg.kind == Kind::Gi {
                Vec3::Z
            } else {
                Vec3::Y
            })
            .normalize();
        let up = right.cross(forward);
        let (near, far, half_fov) = (0.2, 32., 0.72f32);
        let mut projection = Mat4::perspective_rh(half_fov * 2., 1., near, far);
        projection.y_axis.y *= -1.;
        Self {
            view_projection: (projection * Mat4::look_at_rh(position, target, up)).to_cols_array(),
            position: position.extend(energy).to_array(),
            right: right.extend(cfg.size).to_array(),
            up: up.extend(cfg.size).to_array(),
            forward: forward.extend(0.).to_array(),
            lens: [near, far, half_fov.tan(), cfg.bias],
            modes: [
                cfg.kind as u32,
                cfg.gi as u32,
                cfg.shadow as u32,
                cfg.view as u32,
            ],
            probe_extent: [PROBE_EXTENT[0], PROBE_EXTENT[1], PROBE_EXTENT[2], 0],
        }
    }
}

fn output(g: &mut FrameGraph, name: &str, stride: usize) -> Resource {
    let [w, h] = g.extent;
    g.resource(
        name,
        Storage::Buffer {
            bytes: w as usize * h as usize * stride,
            initial: None,
        },
    )
}

fn compute(
    g: &mut FrameGraph,
    name: &str,
    shader: &'static [u8],
    bindings: Vec<(Resource, Access)>,
    [w, h]: [u32; 2],
) {
    g.pass_in(
        PassPhase::Lighting,
        name,
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(shader),
                bindings,
                push: vec![],
            },
            groups: [w.div_ceil(8), h.div_ceil(8), 1],
        },
    );
}

fn shadow_map(g: &mut FrameGraph, triangles: Resource, light: Resource, count: u32) -> Resource {
    let extent = [1024, 1024];
    let color = g.resource(
        "Light-space auxiliary color",
        Storage::Image(ImageDesc {
            extent,
            format: ImageFormat::Visibility,
        }),
    );
    let depth = g.resource(
        "Light-space depth attachment",
        Storage::Image(ImageDesc {
            extent,
            format: ImageFormat::Depth,
        }),
    );
    let groups = count.div_ceil(32);
    g.pass_in(
        PassPhase::Lighting,
        "Rasterize light-space depth",
        Op::Draw {
            program: Dispatch {
                shader: Shader::Mesh(spirv!("lighting_shadow.mesh"), spirv!("visibility.frag")),
                bindings: vec![(triangles, Access::Mesh), (light, Access::Mesh)],
                push: vec![],
            },
            color,
            depth,
            clear: true,
            count: DrawCount::Mesh([groups.min(32768), groups.div_ceil(32768), 1]),
        },
    );
    depth
}

pub(super) fn raster_frame(
    s: &Snapshot,
    cfg: Settings,
    cache: &geometry::Cache,
) -> Result<FrameGraph> {
    let shading = Shading::Custom(PROGRAM.into());
    let bin = s.materials.bin(&shading)?;
    let surfaces = Forward::default().posed_surfaces(s, |_| true)?;
    let packed = if surfaces.iter().any(|v| v.instance.palette.is_some()) {
        None
    } else {
        Some(cache.get(
            &s.scene,
            &geometry::shadow_casters(&surfaces).collect::<Vec<_>>(),
            geometry::Packing::SoftwareLighting,
        )?)
    };
    build_frame_with_lighting(
        s,
        Atlas::cached(&s.scene)?,
        |g, t| {
            g.products.instance_visits += surfaces.len();
            for surface in surfaces {
                let mut material = t.material(s, surface.instance, Mat4::IDENTITY)?;
                material.params[3] = if surface.instance.emitter {
                    1.
                } else if surface.instance.grid {
                    2.
                } else {
                    0.
                };
                surface.draw(g, t, s, material)?;
            }
            Ok(())
        },
        |g,
         ResolveInputs {
             ids,
             objects,
             addresses,
             camera,
             depth,
             ..
         }| {
            let (triangles, nodes, count) = if let Some(packed) = packed {
                (
                    g.resident_input("World-space lighting triangles", packed.triangles.clone()),
                    g.resident_input("Reference ray BVH", packed.acceleration.clone()),
                    packed.count,
                )
            } else {
                let gpu = crate::render::rays::software(g, false)?;
                (gpu.triangles, gpu.nodes, gpu.count)
            };
            ensure!(count > 0, "lighting experiments require triangles");
            let light = g.input(
                "Lighting and experiment settings",
                bytemuck::bytes_of(&Light::new(cfg)).to_vec(),
            );
            let map = if matches!(cfg.shadow, Shadow::Ray) {
                depth
            } else {
                shadow_map(g, triangles, light, count)
            };
            let direct = output(g, "Direct light and visible surface data", 48);
            let mode = match cfg.shadow {
                Shadow::Hard => "Direct light: hard depth comparison",
                Shadow::Pcf => "Direct light: 3×3 PCF",
                Shadow::Pcss => "Direct light: PCSS",
                Shadow::Ray => "Direct light: area-light rays",
            };
            compute(
                g,
                mode,
                spirv!("lighting_direct.comp"),
                vec![
                    (ids, S),
                    (objects, R),
                    (nodes, R),
                    (addresses, R),
                    (camera, R),
                    (light, R),
                    (triangles, R),
                    (direct, W),
                    (map, S),
                ],
                s.extent,
            );
            let indirect = match cfg.gi {
                Gi::Off => g.input("No indirect light", vec![0; 16]),
                Gi::Ssgi | Gi::Ray | Gi::Probes => {
                    let target = output(g, "Diffuse indirect irradiance", 16);
                    let half = s.extent.map(|value| value.div_ceil(2));
                    let raw = g.resource(
                        "Half-resolution indirect samples",
                        Storage::Buffer {
                            bytes: half[0] as usize * half[1] as usize * 16,
                            initial: None,
                        },
                    );
                    let (hit_materials, _) = Atlas::gi_hit_table(&s.scene, g)?;
                    let atlas = g
                        .products
                        .material_atlas
                        .expect("visibility material atlas");
                    let name = match cfg.gi {
                        Gi::Ssgi => "GI: screen-space diffuse tracing",
                        Gi::Ray => "GI: one diffuse bounce",
                        _ => "GI: interpolate irradiance probes",
                    };
                    let probes = if matches!(cfg.gi, Gi::Probes) {
                        let probes = g.resource(
                            "Probe irradiance lobes",
                            Storage::Buffer {
                                bytes: PROBE_EXTENT.iter().product::<u32>() as usize * 6 * 16,
                                initial: None,
                            },
                        );
                        g.pass_in(
                            PassPhase::Lighting,
                            "GI: trace rays from irradiance probes",
                            Op::Compute {
                                program: Dispatch {
                                    shader: Shader::Compute(spirv!("lighting_probe_bake.comp")),
                                    bindings: vec![
                                        (light, R),
                                        (nodes, R),
                                        (triangles, R),
                                        (probes, W),
                                        (hit_materials, R),
                                        (atlas, R),
                                    ],
                                    push: vec![],
                                },
                                groups: PROBE_EXTENT.map(|v| v.div_ceil(4)),
                            },
                        );
                        probes
                    } else {
                        g.input("No irradiance probes", vec![0; 16])
                    };
                    compute(
                        g,
                        name,
                        spirv!("lighting_gi.comp"),
                        vec![
                            (ids, S),
                            (objects, R),
                            (direct, R),
                            (raw, W),
                            (camera, R),
                            (light, R),
                            (nodes, R),
                            (triangles, R),
                            (probes, R),
                            (hit_materials, R),
                            (atlas, R),
                        ],
                        half,
                    );
                    compute(
                        g,
                        "Bilateral upsample diffuse GI",
                        spirv!("lighting_filter.comp"),
                        vec![(direct, R), (raw, R), (target, W), (camera, R)],
                        s.extent,
                    );
                    target
                }
            };
            Ok(FrameLighting::single(MaterialInputs {
                bin,
                bindings: vec![(direct, R), (indirect, R), (light, R)],
            }))
        },
        |_, _| Ok(()),
        |_, _| Ok(()),
    )
}

pub(super) fn trace_frame(s: &Snapshot, cfg: Settings) -> Result<FrameGraph> {
    let mut g = FrameGraph::new(s.extent);
    let mut imports = Default::default();
    for posed in Forward::default().posed_surfaces(s, |i| !i.emitter)? {
        let streams = posed.geometry.ray_stream(&mut g, s, &mut imports)?;
        let material = posed
            .instance
            .material
            .and_then(|id| s.scene.materials.get(&id));
        let fallback = crate::scene::Material::default();
        let m = material.map_or(&fallback, Arc::as_ref);
        g.products
            .ray_surfaces
            .push(crate::render::rays::Surface::new(
                streams,
                m.base_color,
                m.metallic,
                posed.instance.palette.is_some(),
                posed
                    .instance
                    .material
                    .map_or(0, |m| s.scene.routes.material_slots[&m] as u32),
            ));
    }
    let color = crate::render::rays::hardware(&mut g, s, cfg.ray_view as u32)?;
    g.set_output(color);
    Ok(g)
}
