use super::*;
use crate::{
    experiments::{
        lighting::scenes::cube,
        restir::scene::{mesh, object},
    },
    render::{Camera, MaterialPrograms, RaySurface, SurfaceBuffers},
    rhi::Backend,
    scene::{Entity, GiHitMaterial, GiHitModel, GiTexture, Material, Texture, World},
};
use std::sync::Arc;

#[test]
#[ignore = "requires Vulkan 1.4 and VK_EXT_descriptor_heap"]
fn barycentric_texture_sampling_and_delayed_second_bounce() -> Result<()> {
    check_bounces(false, false)
}

#[test]
#[ignore = "requires VK_KHR_ray_tracing_pipeline and VK_KHR_acceleration_structure"]
fn hardware_hits_sample_barycentric_textures_and_transfer_delayed_light() -> Result<()> {
    check_bounces(true, false)
}

#[test]
#[ignore = "requires VK_KHR_ray_query"]
fn inline_queries_preserve_hit_uv_and_second_bounce() -> Result<()> {
    check_bounces(true, true)
}
fn check_bounces(hardware: bool, inline: bool) -> Result<()> {
    let mut world = World::default();
    let texels = [128, 0, 0, 255, 0, 255, 0, 255];
    let texture = world.spawn(Entity {
        texture: Some(Arc::new(Texture {
            width: 2,
            height: 1,
            rgba: texels.to_vec(),
        })),
        ..Default::default()
    });
    let geometry = mesh(&mut world, "Test geometry", cube());
    let mut material = |name: &str, gi_hit| {
        let mat = world.spawn(Entity {
            name: name.into(),
            material: Some(Material {
                base_color: [1.; 4],
                gi_hit,
                ..Default::default()
            }),
            ..Default::default()
        });
        object(&mut world, geometry, mat, name, [0.; 3], [1.; 3]);
        mat
    };
    let emitter = material(
        "Painted emitter",
        GiHitMaterial {
            model: GiHitModel::Unlit,
            texture: GiTexture::Texture(texture),
            ..Default::default()
        },
    );
    use crate::scene::GiNode;
    let painted = material(
        "Channel program",
        GiHitMaterial {
            model: GiHitModel::Program {
                nodes: vec![
                    GiNode::Texture(GiTexture::Texture(texture)),
                    GiNode::Luminance(0),
                    GiNode::Constant([0., 0., 2.]),
                    GiNode::Multiply([1, 2]),
                    GiNode::Constant([0.; 3]),
                ],
                diffuse: 4,
                emission: 3,
            },
            ..Default::default()
        },
    );
    let reflector = material("Diffuse", GiHitMaterial::default());
    world.validate()?;
    let view = Snapshot::new(
        &world,
        Camera::default(),
        [8, 8],
        Arc::new(MaterialPrograms::default()),
    )?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut backend = Backend::open()?;
    let mut draw =
        |first, vertex_u: f32, mode: DiffuseGi, tick: u32, sequence: u64| -> Result<[f32; 3]> {
            // One primary surface between two facing planes. There are no analytic lights.
            // At the central hit the barycentrics are (1/4,1/4,1/2).
            let mut g = FrameGraph::new(view.extent);
            let mut geometry = Vec::new();
            for (z, m, normal) in [(0.012, first, -1.), (-0.012, emitter, 1.)] {
                let mut triangle = [[0f32; 4]; 8];
                triangle[..3].copy_from_slice(&[
                    [-16., -16., z, 0.],
                    [16., -16., z, 0.],
                    [0., 16., z, vertex_u],
                ]);
                triangle[3..6].fill([0., 0., normal, 0.5]);
                let slot = view.scene.routes.material_slots[&crate::render::MaterialId(m)] as u32;
                triangle[7][0] = f32::from_bits(slot);
                if hardware {
                    // Give the two local triangles instance transforms. Exercise both
                    // per-face material IDs (foliage) and per-instance IDs (rigid meshes).
                    let model = glam::Mat4::from_scale_rotation_translation(
                        glam::Vec3::new(2., 0.5, 1.),
                        glam::Quat::IDENTITY,
                        glam::Vec3::Z * z,
                    );
                    let mut vertices = Vec::new();
                    for j in 0..3 {
                        let [x, y, _, u] = triangle[j];
                        vertices.extend([[x * 0.5, y * 2., 0., u], triangle[3 + j]]);
                    }
                    let vertices = g.input(
                        "Local plane vertices",
                        bytemuck::cast_slice(&vertices).to_vec(),
                    );
                    let faces = g.input(
                        "Plane face",
                        bytemuck::cast_slice(&[0u32, 1, 2, slot]).to_vec(),
                    );
                    g.products.ray_surfaces.push(RaySurface::new(
                        SurfaceBuffers {
                            vertices,
                            faces,
                            count: 1,
                            model,
                        },
                        [1.; 4],
                        0.,
                        false,
                        if normal < 0. { u32::MAX } else { slot },
                    ));
                }
                geometry.extend_from_slice(&triangle);
            }
            let mut atlas = vec![255u8; 4];
            atlas.extend(texels);
            g.products.material_atlas = Some(g.input("Test texels", atlas));
            let s = [0f32, 0., 0., 1., 0., 0., 1., f32::from_bits(1)];
            let surface = g.input(
                "Visible plane",
                bytemuck::cast_slice(&s.repeat(64)).to_vec(),
            );
            let mut controls = [0u32; 20];
            controls[8] = 8;
            controls[9] = 8;
            controls[15] = (mode as u32 as f32).to_bits();
            let config = g.input("Controls", bytemuck::cast_slice(&controls).to_vec());
            let lights = g.input("No lights", vec![0; 16]);
            let history = g.input("Clock", bytemuck::cast_slice(&[tick, 0, 0, 0]).to_vec());
            let scene = if hardware {
                RayScene::build(
                    &mut g,
                    if inline {
                        rays::Traversal::Inline
                    } else {
                        rays::Traversal::Pipeline
                    },
                    true,
                    None,
                )?
            } else {
                let bounds = [-16f32, -16., -0.013, 0., 16., 16., 0.013, f32::from_bits(2)];
                RayScene::Software {
                    nodes: g.input("One leaf", bytemuck::cast_slice(&bounds).to_vec()),
                    triangles: g.input("Planes", bytemuck::cast_slice(&geometry).to_vec()),
                    deformed: rays::software(&mut g, true)?,
                }
            };
            let old_pixels = g.input("No reprojection", vec![255; 16]);
            let output = build(
                &mut g,
                &view,
                mode,
                sequence,
                Inputs {
                    scene,
                    raster: [surface; 3],
                    surface,
                    config,
                    lights,
                    history,
                    old_pixels,
                },
            )?;
            g.set_output(output);
            let frame = backend.execute(g.compile()?, &pool)?;
            Ok(std::array::from_fn(|i| {
                f32::from_ne_bytes(frame.pixels[i * 4..i * 4 + 4].try_into().unwrap())
            }))
        };
    let red = draw(emitter, 0.5, DiffuseGi::Single, 1, 1)?;
    let green = draw(emitter, 1.5, DiffuseGi::Single, 1, 2)?;
    assert!((red[0] - 0.216).abs() < 0.015 && red[1] < 0.012, "{red:?}");
    assert!(green[1] > 0.98 && green[0] < 0.012, "{green:?}");
    assert_eq!(draw(reflector, 1.5, DiffuseGi::Single, 1, 3)?, [0.; 3]);
    assert_eq!(draw(reflector, 1.5, DiffuseGi::Multi, 1, 4)?, [0.; 3]);
    let mut bounced = 0f32;
    for tick in 2..=8 {
        bounced = bounced.max(draw(reflector, 1.5, DiffuseGi::Multi, tick, 4)?[1]);
    }
    assert!(bounced > 0.4, "delayed emitter energy {bounced}");
    let blue = draw(painted, 1.5, DiffuseGi::Single, 1, 5)?;
    assert!(
        blue[0] < 0.001 && blue[1] < 0.001 && (blue[2] - 1.4304).abs() < 0.035,
        "{blue:?}"
    );
    Ok(())
}
