use super::*;
use crate::render::{GpuLight, pack_lights};
use crate::{
    experiments::lighting::{
        EMITTER,
        scenes::{cube, sphere},
    },
    scene::{Entity, Instance, Material, Mesh, Shading},
};
use glam::{Mat4, Quat};

fn random(seed: &mut u32) -> f32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 17;
    *seed ^= *seed << 5;
    (*seed >> 8) as f32 * (1. / 16777216.)
}

pub(super) fn lights() -> Arc<[u8]> {
    let mut seed = 0x19875a77;
    let lights = (0..LIGHT_MAX)
        .map(|i| {
            let bank = i % 6;
            let side = if bank % 2 == 0 { -1. } else { 1. };
            let x = side * (0.93 + 0.78 * (bank / 2) as f32 + random(&mut seed) * 0.88);
            let y = 2.65 + random(&mut seed) * 1.35;
            let z = 3.2 - random(&mut seed) * 17.0;
            let color = match (bank, (z < -5.) as u32) {
                (0 | 2, 0) => Vec3::new(1., 0.32, 0.045),
                (1 | 3, 0) => Vec3::new(0.12, 0.57, 1.),
                (0 | 4, 1) => Vec3::new(0.33, 0.42, 1.),
                (1 | 5, 1) => Vec3::new(0.12, 1., 0.6),
                _ => Vec3::new(1., 0.78, 0.56),
            };
            let pulse = if i.is_multiple_of(13) {
                9.5 + 3.5 * random(&mut seed)
            } else {
                0.09 + 0.26 * random(&mut seed)
            };
            GpuLight::point(Vec3::new(x, y, z), color * pulse * 34., 0.)
        })
        .collect::<Vec<_>>();
    pack_lights(&lights)
}

pub fn scene() -> Result<World> {
    let mut world = World::default();
    let box_mesh = mesh(&mut world, "Hall boxes", cube());
    let sphere_mesh = mesh(&mut world, "Test sphere", sphere(20, 10));
    let shadow = mesh(&mut world, "Sphere shadow hull", sphere(8, 4));
    let bounds =
        crate::scene::LocalBounds::from_mesh(world.entities[&sphere_mesh].mesh.as_ref().unwrap())?;
    let sphere_products = world.spawn(Entity {
        name: "Sphere derived geometry".into(),
        geometry_assets: Some(Arc::new(crate::scene::GeometryAssets {
            source_mesh: sphere_mesh,
            local_bounds: Some(bounds),
            shadow_mesh: Some(shadow),
        })),
        ..Default::default()
    });
    let concrete = material(&mut world, "Concrete", [0.61, 0.65, 0.73]);
    let pale = material(&mut world, "Ivory", [0.78, 0.79, 0.72]);
    let red = material(&mut world, "Iron oxide", [0.64, 0.24, 0.12]);
    let blue = material(&mut world, "Blue ceramic", [0.17, 0.35, 0.67]);
    let teal = material(&mut world, "Teal ceramic", [0.13, 0.56, 0.47]);
    for (name, p, scale, mat) in [
        ("Floor", [0., -0.15, -5.], [13.8, 0.3, 20.5], concrete),
        ("Rear wall", [0., 2.1, -15.05], [13.8, 4.2, 0.2], concrete),
        ("Left wall", [-6.9, 2.1, -5.], [0.2, 4.2, 20.5], concrete),
        ("Right wall", [6.9, 2.1, -5.], [0.2, 4.2, 20.5], concrete),
        (
            "Tall near occluder",
            [0.82, 1.15, -0.85],
            [0.48, 2.3, 2.24],
            red,
        ),
        (
            "Low platform",
            [-1.58, 0.23, -1.43],
            [2.42, 0.46, 2.35],
            pale,
        ),
        (
            "Rear divider",
            [0., 1.17, -8.1],
            [2.7, 2.34, 0.34],
            concrete,
        ),
        ("Right bench", [2.05, 0.35, -4.7], [1.3, 0.7, 2.62], blue),
    ] {
        object(&mut world, box_mesh, mat, name, p, scale);
    }
    for i in 0..5 {
        let z = 1.65 - i as f32 * 3.27;
        for x in [-4.28, 4.28] {
            object(
                &mut world,
                box_mesh,
                pale,
                "Cast-shadow pillar",
                [x, 1.9, z],
                [0.78, 3.8, 0.84],
            );
        }
        object(
            &mut world,
            box_mesh,
            concrete,
            "Ceiling beam",
            [0., 4.03, z],
            [8.9, 0.24, 0.28],
        );
    }
    for (mat, position, diameter) in [
        (blue, [-1.88, 1.15, -1.28], 1.36),
        (pale, [2.08, 1.15, -4.85], 0.9),
        (teal, [-2.16, 0.71, -5.69], 1.42),
        (red, [2.6, 0.64, -10.96], 1.28),
    ] {
        let id = object(
            &mut world,
            sphere_mesh,
            mat,
            "Diffuse reference ball",
            position,
            [diameter; 3],
        );
        world.bind_geometry_assets(id, Some(sphere_products))?;
    }
    for (x, tint) in [
        (-2.2, [1., 0.27, 0.055]),
        (2.2, [0.1, 0.51, 1.]),
        (0., [0.77, 0.91, 1.]),
    ] {
        let mut strips = Mesh {
            positions: vec![],
            indices: vec![],
            normals: vec![],
            uv: vec![],
            joints: vec![],
            weights: vec![],
        };
        for i in 0..18 {
            let z = 3.82 - i as f32 * 0.99;
            let first = strips.positions.len() as u32;
            strips.positions.extend([
                [x - 0.19, 3.73, z - 0.24],
                [x + 0.19, 3.73, z - 0.24],
                [x + 0.19, 3.73, z + 0.24],
                [x - 0.19, 3.73, z + 0.24],
            ]);
            strips.normals.extend([[0., -1., 0.]; 4]);
            strips.indices.extend([0, 1, 2, 0, 2, 3].map(|i| first + i));
        }
        let mesh = mesh(&mut world, "Ceiling fixture markers", strips);
        let tint = material(&mut world, "Fixture tint", tint);
        let id = object(&mut world, mesh, tint, "Ceiling fixtures", [0.; 3], [1.; 3]);
        world
            .entities
            .get_mut(&id)
            .unwrap()
            .components
            .insert(EMITTER.into(), true.into());
    }
    world.spawn(Entity {
        name: "Many-light sampling settings".into(),
        components: [(SETTING.into(), serde_json::to_value(Settings::default())?)].into(),
        ..Default::default()
    });
    world.validate()?;
    Ok(world)
}

pub(in crate::experiments) fn mesh(world: &mut World, name: &str, mesh: Mesh) -> Id {
    world.spawn(Entity {
        name: name.into(),
        mesh: Some(Arc::new(mesh)),
        ..Default::default()
    })
}

pub(super) fn material(world: &mut World, name: &str, rgb: [f32; 3]) -> Id {
    world.spawn(Entity {
        name: name.into(),
        material: Some(Material {
            shading: Shading::Custom(PROGRAM.into()),
            base_color: Vec3::from_array(rgb).extend(1.).to_array(),
            ..Default::default()
        }),
        ..Default::default()
    })
}

pub(in crate::experiments) fn object(
    world: &mut World,
    mesh: Id,
    mat: Id,
    name: &str,
    p: [f32; 3],
    scale: [f32; 3],
) -> Id {
    world.spawn(Entity {
        name: name.into(),
        local: Mat4::from_scale_rotation_translation(scale.into(), Quat::IDENTITY, p.into()),
        instance: Some(Instance::new(mesh, Some(mat))),
        ..Default::default()
    })
}
