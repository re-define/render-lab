use crate::scene::*;
use anyhow::{Context, Result, bail};
use glam::Mat4;
use gltf::{image::Format, mesh::Mode};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
};

/// Build into a temporary world so a bad primitive never partially changes the live scene.
pub fn import(target: &mut World, path: &Path) -> Result<Vec<Id>> {
    let (doc, buffers, images) =
        gltf::import(path).with_context(|| format!("opening {}", path.display()))?;
    let mut w = target.clone();
    let mut textures = Vec::new();
    for image in images {
        let (channels, bytes) = match image.format {
            Format::R8 => (1, 1),
            Format::R8G8 => (2, 1),
            Format::R8G8B8 => (3, 1),
            Format::R8G8B8A8 => (4, 1),
            Format::R16 => (1, 2),
            Format::R16G16 => (2, 2),
            Format::R16G16B16 => (3, 2),
            Format::R16G16B16A16 => (4, 2),
            Format::R32G32B32FLOAT => (3, 4),
            Format::R32G32B32A32FLOAT => (4, 4),
        };
        let mut rgba = Vec::new();
        for pixel in image.pixels.chunks_exact(channels * bytes) {
            let mut c = [0, 0, 0, 255];
            for (j, b) in pixel.chunks_exact(bytes).enumerate() {
                c[j] = match bytes {
                    1 => b[0],
                    2 => (u16::from_ne_bytes(b.try_into()?) >> 8) as u8,
                    _ => (f32::from_ne_bytes(b.try_into()?).clamp(0., 1.) * 255.).round() as u8,
                };
            }
            if channels <= 2 {
                let alpha = if channels == 2 { c[1] } else { 255 };
                c = [c[0], c[0], c[0], alpha];
            }
            rgba.extend(c);
        }
        textures.push(w.spawn(Entity {
            texture: Some(Arc::new(Texture {
                width: image.width,
                height: image.height,
                rgba,
            })),
            ..Default::default()
        }));
    }
    let materials = doc
        .materials()
        .map(|m| {
            let p = m.pbr_metallic_roughness();
            w.spawn(Entity {
                name: m.name().unwrap_or("Material").into(),
                material: Some(Material {
                    shading: Shading::Pbr,
                    base_color: p.base_color_factor(),
                    metallic: p.metallic_factor(),
                    roughness: p.roughness_factor(),
                    texture: p
                        .base_color_texture()
                        .map(|t| textures[t.texture().source().index()]),
                }),
                ..Default::default()
            })
        })
        .collect::<Vec<_>>();
    let mut meshes = BTreeMap::new();
    for mesh in doc.meshes() {
        for p in mesh.primitives() {
            let r = p.reader(|b| Some(&buffers[b.index()].0));
            let positions: Vec<_> = r
                .read_positions()
                .context("primitive missing POSITION")?
                .collect();
            let indices: Vec<u32> = r
                .read_indices()
                .map(|i| i.into_u32().collect())
                .unwrap_or_else(|| (0..positions.len() as u32).collect());
            let indices = match p.mode() {
                Mode::Triangles => indices,
                Mode::TriangleStrip => indices
                    .windows(3)
                    .enumerate()
                    .flat_map(|(i, t)| {
                        if i % 2 == 0 {
                            [t[0], t[1], t[2]]
                        } else {
                            [t[1], t[0], t[2]]
                        }
                    })
                    .collect(),
                Mode::TriangleFan => indices
                    .windows(2)
                    .skip(1)
                    .flat_map(|t| [indices[0], t[0], t[1]])
                    .collect(),
                other => bail!("unsupported glTF primitive mode {other:?}"),
            };
            let uv_set = p
                .material()
                .pbr_metallic_roughness()
                .base_color_texture()
                .map_or(0, |t| t.tex_coord());
            let id = w.spawn(Entity {
                name: mesh.name().unwrap_or("Mesh").into(),
                mesh: Some(Arc::new(Mesh {
                    positions,
                    indices,
                    normals: r.read_normals().map(Iterator::collect).unwrap_or_default(),
                    uv: r
                        .read_tex_coords(uv_set)
                        .map(|t| t.into_f32().collect())
                        .unwrap_or_default(),
                    joints: r
                        .read_joints(0)
                        .map(|t| t.into_u16().collect())
                        .unwrap_or_default(),
                    weights: r
                        .read_weights(0)
                        .map(|t| t.into_f32().collect())
                        .unwrap_or_default(),
                })),
                ..Default::default()
            });
            meshes.insert((mesh.index(), p.index()), id);
        }
    }
    let nodes = doc
        .nodes()
        .map(|n| {
            w.spawn(Entity {
                name: n.name().unwrap_or("Node").into(),
                local: Mat4::from_cols_array_2d(&n.transform().matrix()),
                ..Default::default()
            })
        })
        .collect::<Vec<_>>();
    for animation in doc.animations() {
        use gltf::animation::util::ReadOutputs;
        let tracks = animation
            .channels()
            .map(|c| {
                let r = c.reader(|b| Some(&buffers[b.index()].0));
                let times = r
                    .read_inputs()
                    .context("missing animation times")?
                    .collect();
                let values = match r.read_outputs().context("missing animation outputs")? {
                    ReadOutputs::Translations(v) => TrackValues::Translation(v.collect()),
                    ReadOutputs::Rotations(v) => TrackValues::Rotation(v.into_f32().collect()),
                    ReadOutputs::Scales(v) => TrackValues::Scale(v.collect()),
                    ReadOutputs::MorphTargetWeights(v) => {
                        TrackValues::MorphWeights(v.into_f32().collect())
                    }
                };
                let interpolation = match c.sampler().interpolation() {
                    gltf::animation::Interpolation::Step => Interpolation::Step,
                    gltf::animation::Interpolation::Linear => Interpolation::Linear,
                    gltf::animation::Interpolation::CubicSpline => Interpolation::CubicSpline,
                };
                Ok(Track {
                    target: nodes[c.target().node().index()],
                    times,
                    values,
                    interpolation,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        w.spawn(Entity {
            name: animation.name().unwrap_or("Animation").into(),
            animation: Some(Arc::new(tracks)),
            ..Default::default()
        });
    }
    let skins = doc
        .skins()
        .map(|s| {
            let joints = s.joints().map(|n| nodes[n.index()]).collect::<Vec<_>>();
            let inverse_bind = s
                .reader(|b| Some(&buffers[b.index()].0))
                .read_inverse_bind_matrices()
                .map(|m| m.map(|x| Mat4::from_cols_array_2d(&x)).collect())
                .unwrap_or_else(|| vec![Mat4::IDENTITY; joints.len()]);
            w.spawn(Entity {
                skin: Some(Skin {
                    joints,
                    inverse_bind,
                }),
                ..Default::default()
            })
        })
        .collect::<Vec<_>>();
    let scene = doc
        .default_scene()
        .or_else(|| doc.scenes().next())
        .context("no glTF scene")?;
    let roots = scene.nodes().map(|n| nodes[n.index()]).collect();
    let mut stack: Vec<_> = scene.nodes().collect();
    let mut active = BTreeSet::new();
    while let Some(n) = stack.pop() {
        if active.insert(n.index()) {
            stack.extend(n.children());
        }
    }
    for n in doc.nodes() {
        for c in n.children() {
            w.entities.get_mut(&nodes[c.index()]).unwrap().parent = Some(nodes[n.index()]);
        }
        if !active.contains(&n.index()) {
            continue;
        }
        if let Some(m) = n.mesh() {
            for p in m.primitives() {
                w.spawn(Entity {
                    name: format!("{} / {}", m.name().unwrap_or("Mesh"), p.index()),
                    parent: Some(nodes[n.index()]),
                    skin_binding: n.skin().map(|s| skins[s.index()]),
                    instance: Some(Instance {
                        mesh: meshes[&(m.index(), p.index())],
                        material: p.material().index().map(|i| materials[i]),
                        legacy_shading: None,
                        outline: false,
                    }),
                    ..Default::default()
                });
            }
        }
    }
    w.validate()?;
    *target = w;
    Ok(roots)
}
