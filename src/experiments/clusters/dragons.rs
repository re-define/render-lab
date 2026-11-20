use super::*;
use std::path::Path;
/// The four dragon primitives share a mesh-local coordinate system in the research scene.
pub fn scene() -> Result<World> {
    let mut imported = World::default();
    crate::import::import(
        &mut imported,
        Path::new("assets/scenes/dragon/dragon_core.gltf"),
    )?;
    let mut mesh = Mesh {
        positions: vec![],
        normals: vec![],
        uv: vec![],
        indices: vec![],
        joints: vec![],
        weights: vec![],
    };
    for e in imported
        .entities
        .values()
        .filter(|e| e.name.ends_with("_Dragon"))
    {
        if let Some(part) = &e.mesh {
            let base = mesh.positions.len() as u32;
            mesh.positions.extend_from_slice(&part.positions);
            mesh.normals.extend_from_slice(&part.normals);
            mesh.uv.extend_from_slice(&part.uv);
            mesh.indices.extend(part.indices.iter().map(|i| i + base));
        }
    }
    ensure!(
        !mesh.indices.is_empty(),
        "dragon primitives missing from glTF"
    );
    drop(imported);
    let mut w = World::default();
    let dragon = w.spawn(Entity {
        name: "Dragon geometry".into(),
        mesh: Some(Arc::new(mesh)),
        ..Default::default()
    });
    let jade = w.spawn(Entity {
        name: "Jade PBR".into(),
        material: Some(Material {
            base_color: [0.12, 0.49, 0.35, 1.],
            metallic: 0.17,
            shading: Shading::Pbr,
            ..Default::default()
        }),
        ..Default::default()
    });
    for z in 0..6 {
        for x in 0..6 {
            w.spawn(Entity {
                name: format!("Dragon {}-{}", x + 1, z + 1),
                local: Mat4::from_translation(Vec3::new(
                    (x as f32 - 2.5) * 24.,
                    0.,
                    (z as f32 - 2.5) * 25.,
                )) * Mat4::from_rotation_y((x + z) as f32 * 0.19),
                instance: Some(Instance {
                    mesh: dragon,
                    material: Some(jade),
                    legacy_shading: None,
                    outline: false,
                }),
                ..Default::default()
            });
        }
    }
    let cube = w.spawn(Entity {
        name: "Ordinary indexed block".into(),
        components: BTreeMap::from([(ORDINARY.into(), true.into())]),
        mesh: Some(Arc::new(Mesh {
            positions: vec![
                [-8., 0., -4.],
                [8., 0., -4.],
                [8., 13., -4.],
                [-8., 13., -4.],
                [-8., 0., 4.],
                [8., 0., 4.],
                [8., 13., 4.],
                [-8., 13., 4.],
            ],
            indices: vec![
                0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 4, 7, 0, 7, 3, 1, 2, 6, 1, 6, 5, 3, 7, 6, 3,
                6, 2, 0, 1, 5, 0, 5, 4,
            ],
            normals: vec![],
            uv: vec![],
            joints: vec![],
            weights: vec![],
        })),
        ..Default::default()
    });
    let clay = w.spawn(Entity {
        name: "Orange toon".into(),
        material: Some(Material {
            base_color: [0.97, 0.24, 0.035, 1.],
            shading: Shading::Toon,
            ..Default::default()
        }),
        ..Default::default()
    });
    w.spawn(Entity {
        name: "Ordinary block".into(),
        local: Mat4::from_translation(Vec3::new(0., 0., 55.)),
        instance: Some(Instance {
            mesh: cube,
            material: Some(clay),
            legacy_shading: None,
            outline: false,
        }),
        ..Default::default()
    });
    prepare(&mut w)?;
    let error = w
        .entities
        .values_mut()
        .find_map(|e| e.components.get_mut(SETTING))
        .unwrap();
    *error = 0.25.into();
    Ok(w)
}
