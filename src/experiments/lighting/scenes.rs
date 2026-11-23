use super::*;
use glam::{Quat, Vec3};

const WHITE: [f32; 3] = [0.72, 0.75, 0.78];
const RED: [f32; 3] = [0.72, 0.065, 0.032];
const GREEN: [f32; 3] = [0.055, 0.54, 0.22];
const BLUE: [f32; 3] = [0.06, 0.25, 0.72];

pub fn scene(kind: Kind) -> Result<World> {
    let mut world = World::default();
    let config = Settings::new(kind);
    let cube = mesh(&mut world, "Unit box", cube());
    let sphere = mesh(&mut world, "Unit sphere", sphere(24, 12));
    match kind {
        Kind::Gi => {
            for (name, center, size, color) in [
                ("Neutral floor", [0., -0.1, 0.], [6., 0.2, 6.], WHITE),
                ("Neutral ceiling", [0., 4.1, 0.], [6., 0.2, 6.], WHITE),
                ("Neutral back wall", [0., 2., -3.1], [6., 4., 0.2], WHITE),
                ("Red bounce wall", [-3.1, 2., 0.], [0.2, 4., 6.], RED),
                ("Green bounce wall", [3.1, 2., 0.], [0.2, 4., 6.], GREEN),
                (
                    "White test block",
                    [-1.67, 0.65, -0.95],
                    [1.16, 1.3, 1.2],
                    WHITE,
                ),
                (
                    "White test step",
                    [1.35, 0.26, 0.8],
                    [1.35, 0.52, 1.2],
                    WHITE,
                ),
                (
                    "Recess occluder",
                    [0.78, 0.94, -1.75],
                    [0.18, 1.88, 1.5],
                    WHITE,
                ),
                (
                    "Recess cold panel",
                    [1.85, 0.35, -2.82],
                    [1.3, 0.7, 0.24],
                    BLUE,
                ),
            ] {
                let yaw = if name == "White test block" {
                    -0.26
                } else {
                    0.
                };
                object(&mut world, cube, name, (center, size, yaw), color, 0.);
            }
            object(
                &mut world,
                sphere,
                "Neutral sphere",
                ([1.36, 0.95, 0.77], [0.88; 3], 0.),
                WHITE,
                0.,
            );
            let id = object(
                &mut world,
                cube,
                "Ceiling emitter",
                ([0., 3.92, -0.4], [1.36, 0.06, 1.36], 0.),
                [1.; 3],
                0.,
            );
            world
                .entities
                .get_mut(&id)
                .unwrap()
                .components
                .insert(EMITTER.into(), true.into());
        }
        Kind::Shadow => {
            let ground = object(
                &mut world,
                cube,
                "Gridded receiver",
                ([0., -0.12, -5.], [13., 0.24, 21.], 0.),
                WHITE,
                0.,
            );
            world
                .entities
                .get_mut(&ground)
                .unwrap()
                .components
                .insert(GRID.into(), true.into());
            for (i, (x, z, clearance)) in [
                (-2.4, 0.5, 0.),
                (0., -1.8, 0.42),
                (2.5, -4., 1.15),
                (-1.8, -8., 1.15),
            ]
            .into_iter()
            .enumerate()
            {
                object(
                    &mut world,
                    sphere,
                    &format!("Sphere receiver gap {clearance} m"),
                    ([x, 0.62 + clearance, z], [1.24; 3], 0.),
                    [BLUE, BLUE, BLUE, GREEN][i],
                    0.,
                );
            }
            object(
                &mut world,
                cube,
                "Raised receiver",
                ([2.65, 0.2, -6.7], [3.2, 0.4, 2.5], 0.),
                [0.57, 0.62, 0.68],
                0.,
            );
            object(
                &mut world,
                cube,
                "Rotated tower",
                ([-3.0, 1.05, -4.26], [0.95, 2.1, 0.9], 0.32),
                [0.57, 0.34, 0.2],
                0.,
            );
            let ramp = object(
                &mut world,
                cube,
                "Grazing sloped receiver",
                ([0.4, 0.87, -6.8], [1.75, 0.09, 2.75], 0.),
                WHITE,
                0.,
            );
            world.entities.get_mut(&ramp).unwrap().local = Mat4::from_scale_rotation_translation(
                Vec3::new(1.75, 0.09, 2.75),
                Quat::from_rotation_x(-0.27),
                Vec3::new(0.4, 0.87, -6.8),
            );
            for i in 0..7 {
                let x = -1.55 + i as f32 * 0.24;
                object(
                    &mut world,
                    cube,
                    "Thin fence slat",
                    ([x, 0.72, 1.55], [0.055, 1.44, 0.075], 0.),
                    [0.4, 0.22, 0.10],
                    0.,
                );
            }
            object(
                &mut world,
                cube,
                "Fence crossbar",
                ([-0.83, 1.09, 1.55], [1.62, 0.10, 0.10], 0.),
                [0.4, 0.22, 0.10],
                0.,
            );
        }
        Kind::Rt => {
            object(
                &mut world,
                cube,
                "Matte floor",
                ([0., -0.15, -1.], [12., 0.3, 14.], 0.),
                [0.46, 0.52, 0.61],
                0.,
            );
            object(
                &mut world,
                cube,
                "Rear mirror",
                ([0., 1.5, -3.55], [4.4, 3., 0.13], 0.),
                [0.80, 0.90, 0.98],
                0.94,
            );
            object(
                &mut world,
                sphere,
                "Polished sphere",
                ([1.05, 1.02, 0.05], [2.04; 3], 0.),
                [0.87, 0.90, 0.97],
                0.92,
            );
            object(
                &mut world,
                sphere,
                "Matte orange sphere",
                ([-1.38, 0.72, -0.48], [1.44; 3], 0.),
                [0.90, 0.18, 0.045],
                0.,
            );
            object(
                &mut world,
                cube,
                "Rotated blue block",
                ([-2.36, 0.66, -1.82], [0.95, 1.32, 0.93], 0.34),
                BLUE,
                0.,
            );
            // This column is outside the default primary view. Reflected rays
            // can still see it in the rear mirror.
            object(
                &mut world,
                cube,
                "Off-camera reflection marker",
                ([2.9, 1.08, 4.2], [1.04, 2.16, 0.98], -0.23),
                GREEN,
                0.,
            );
            for i in 0..4 {
                object(
                    &mut world,
                    cube,
                    "Contrast pillar",
                    ([3.4, 0.85, -2.5 + i as f32 * 0.72], [0.34, 1.7, 0.34], 0.),
                    [RED, WHITE, RED, WHITE][i],
                    0.,
                );
            }
        }
    }
    world.spawn(Entity {
        name: format!("{} experiment settings", kind.name()),
        components: [(SETTING.into(), serde_json::to_value(config)?)].into(),
        ..Default::default()
    });
    world.validate()?;
    Ok(world)
}

fn mesh(world: &mut World, name: &str, mesh: Mesh) -> Id {
    world.spawn(Entity {
        name: name.into(),
        mesh: Some(Arc::new(mesh)),
        ..Default::default()
    })
}

fn object(
    world: &mut World,
    mesh: Id,
    name: &str,
    (position, size, yaw): ([f32; 3], [f32; 3], f32),
    color: [f32; 3],
    metallic: f32,
) -> Id {
    let material = world.spawn(Entity {
        name: format!("{name} material"),
        material: Some(Material {
            shading: Shading::Custom(PROGRAM.into()),
            base_color: Vec3::from_array(color).extend(1.).to_array(),
            metallic,
            ..Default::default()
        }),
        ..Default::default()
    });
    world.spawn(Entity {
        name: name.into(),
        local: Mat4::from_scale_rotation_translation(
            size.into(),
            Quat::from_rotation_y(yaw),
            position.into(),
        ),
        instance: Some(Instance::new(mesh, Some(material))),
        ..Default::default()
    })
}

pub(crate) fn cube() -> Mesh {
    let mut mesh = Mesh {
        positions: vec![],
        indices: vec![],
        normals: vec![],
        uv: vec![],
        joints: vec![],
        weights: vec![],
    };
    for (n, a, b) in [
        (Vec3::X, Vec3::Y, Vec3::Z),
        (-Vec3::X, Vec3::Y, -Vec3::Z),
        (Vec3::Y, Vec3::Z, Vec3::X),
        (-Vec3::Y, Vec3::Z, -Vec3::X),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (-Vec3::Z, Vec3::X, -Vec3::Y),
    ] {
        let start = mesh.positions.len() as u32;
        for point in [n - a - b, n + a - b, n + a + b, n - a + b] {
            mesh.positions.push((point * 0.5).to_array());
            mesh.normals.push(n.to_array());
        }
        mesh.indices.extend([0, 1, 2, 0, 2, 3].map(|i| start + i));
    }
    mesh
}

pub(crate) fn sphere(sides: u32, rings: u32) -> Mesh {
    let mut mesh = Mesh {
        positions: vec![],
        indices: vec![],
        normals: vec![],
        uv: vec![],
        joints: vec![],
        weights: vec![],
    };
    for row in 0..=rings {
        let (radius, y) = (std::f32::consts::PI * row as f32 / rings as f32).sin_cos();
        for col in 0..sides {
            let (z, x) = (std::f32::consts::TAU * col as f32 / sides as f32).sin_cos();
            let n = Vec3::new(x * radius, y, z * radius);
            mesh.positions.push((n * 0.5).to_array());
            mesh.normals.push(n.to_array());
            if row < rings {
                let a = row * sides + col;
                let b = row * sides + (col + 1) % sides;
                if row > 0 {
                    mesh.indices.extend([a, b, a + sides]);
                }
                if row + 1 < rings {
                    mesh.indices.extend([b, b + sides, a + sides]);
                }
            }
        }
    }
    mesh
}
