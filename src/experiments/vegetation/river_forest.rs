//! Deterministic mixed forest. Both species placement and the bed use the authored river cross section.
use super::{generate::Random, landscape::Terrain, *};
use crate::experiments::effects::river;
use glam::Vec2;

pub(super) const GROUND: &str = "Riverbank soil";

pub(super) fn camera(name: &str) -> Camera {
    let point = |z: f32, offset: f32, lift: f32| {
        let x = river::center(z) + river::width(z) * offset;
        Vec3::new(x, river::level(z).max(river::ground(x, z)) + lift, z)
    };
    let meadow_point = |z: f32, dx: f32, lift: f32| {
        let x = river::center(z) + river::width(z) + dx;
        Vec3::new(x, river::ground(x, z) + lift, z)
    };
    let (eye, target) = match name {
        "glade" => (
            meadow_point(-59., 10.5, 1.28),
            meadow_point(-68., 8.6, 0.50),
        ),
        "basin" => (point(-47., 1.7, 117.), point(-225., 0., 1.)),
        "grass" => (point(-14., 0.95, 1.05), point(-19.6, 1.37, 0.36)),
        _ => (point(-13., 0.28, 4.7), point(-45., 0., 1.2)),
    };
    let delta = eye - target;
    Camera {
        target,
        distance: delta.length(),
        yaw: delta.x.atan2(delta.z),
        pitch: (delta.y / delta.length()).asin(),
    }
}

fn transform(p: Vec2, scale: f32, yaw: f32) -> Mat4 {
    Mat4::from_scale_rotation_translation(
        Vec3::splat(scale),
        Quat::from_rotation_y(yaw),
        Vec3::new(p.x, river::ground(p.x, p.y) - 0.022, p.y),
    )
}
fn glade(p: Vec2) -> f32 {
    let x = p.x - river::center(p.y) - river::width(p.y) - 10.;
    let r = (x / 10.).powi(2) + ((p.y + 65.) / 17.).powi(2);
    (1.6 - r).clamp(0., 1.)
}
fn clearing(p: Vec2) -> f32 {
    let waves = (p.x * 0.059 + p.y * 0.032).sin()
        + 0.73 * (p.y * 0.071 - p.x * 0.024).cos()
        + 0.38 * (p.x * 0.112 + p.y * 0.087).sin();
    ((waves - 0.62) * 0.92).clamp(0., 1.).max(glade(p))
}
fn crowns(rng: &mut Random) -> Vec<(Mat4, usize)> {
    let mut output = Vec::new();
    for row in 0..162 {
        for col in 0..160 {
            let p = Vec2::new(
                (col as f32 - 79.5) * 3.5 + rng.signed() * 1.50,
                53. - row as f32 * 3.5 + rng.signed() * 1.48,
            );
            let d = river::shore(p.x, p.y);
            if d < 2.5 + rng.next() * 1.7 || rng.next() < (0.67 * clearing(p)).max(glade(p)) {
                continue;
            }
            let family = if d < 6. || rng.next() + 0.31 * (p.x * 0.046 - p.y * 0.053).sin() > 0.53 {
                2
            } else {
                0
            };
            let kind = family + usize::from(rng.next() > 0.54);
            let scale = 0.67 + 0.55 * rng.next() + 0.13 * (p.y * 0.034 + p.x * 0.028).sin();
            output.push((
                transform(p, scale, rng.next() * std::f32::consts::TAU),
                kind,
            ));
        }
    }
    output
}

fn grass_palette(world: &mut World, assets: &[Part]) -> Vec<[Part; 4]> {
    assets
        .iter()
        .map(|&(mesh, id)| {
            let color = glam::Vec4::from(world.entities[&id].material.as_ref().unwrap().base_color)
                .truncate();
            std::array::from_fn(|index| {
                if index == 3 {
                    return (mesh, id);
                }
                let light = 0.44 + 0.18 * index as f32;
                (
                    mesh,
                    material(
                        world,
                        "Grass under canopy",
                        (color * light).into(),
                        PROGRAM,
                        None,
                    ),
                )
            })
        })
        .collect()
}

fn grounded_grass(p: Vec2, scale: f32, yaw: f32, terrain: &Terrain, surface: &Mesh) -> Mat4 {
    let (h, normal) = terrain.surface_at(surface, p);
    let rotation = Quat::from_rotation_arc(Vec3::Y, normal) * Quat::from_rotation_y(yaw);
    Mat4::from_scale_rotation_translation(
        Vec3::splat(scale),
        rotation,
        Vec3::new(p.x, h - 0.017, p.y),
    )
}

fn grasses(
    world: &mut World,
    prototypes: &[[Part; 4]],
    terrain: &Terrain,
    light: &Texture,
    surface: &Mesh,
    rng: &mut Random,
) -> usize {
    let mut count = 0;
    for row in 0..575 {
        for col in 0..568 {
            let p = Vec2::new(
                (col as f32 - 283.5) * 0.98 + rng.signed() * 0.44,
                54. - row as f32 * 0.98 + rng.signed() * 0.45,
            );
            let bank = river::shore(p.x, p.y);
            if bank < 0.68 {
                continue;
            }
            let sun = terrain.light(light, p);
            // Neighbouring stems share a patch size, heading and species likelihood.
            let patch = 0.5
                + 0.5
                    * (p.x * 0.50 + 1.5 * (p.y * 0.21).sin()).sin()
                    * (p.y * 0.41 - p.x * 0.18).cos();
            let gap = clearing(p);
            let wet = (1. - (bank - 1.) / 5.).clamp(0., 1.);
            if rng.next() > 0.58 + 0.24 * sun + 0.16 * patch + 0.10 * wet {
                continue;
            }
            let kind = if wet > rng.next() * 0.85 + 0.10 {
                2
            } else if 0.55 * sun + 0.50 * gap + 0.12 * patch > 0.25 + 0.52 * rng.next() {
                1
            } else {
                0
            };
            let scale =
                (0.76 + 0.36 * patch + 0.22 * rng.next()) * (0.84 + 0.26 * sun + 0.18 * wet);
            let yaw = 4. * (p.x * 0.14 - p.y * 0.08).sin() + 1.23 * rng.signed();
            spawn(
                world,
                "Grass tuft",
                &[prototypes[kind][((sun - 0.3) * 3. / 0.7).round().clamp(0., 3.) as usize]],
                grounded_grass(p, scale, yaw, terrain, surface),
            );
            count += 1;
        }
    }
    // Short mats between the taller sedges break up the widely spaced clump bases.
    for row in 0..1060 {
        let z = 53. - row as f32 * 0.53;
        for side in [-1., 1.] {
            for band in 0..3 {
                let at = z + 0.21 * rng.signed();
                let d = 0.43 + band as f32 * 0.47 + 0.19 * rng.signed() + 0.16 * (at * 0.36).sin();
                let p = Vec2::new(river::center(at) + side * (river::width(at) + d), at);
                if river::ground(p.x, p.y) <= river::level(at) + 0.09 || rng.next() < 0.16 {
                    continue;
                }
                let scale = 0.43 + 0.31 * rng.next();
                spawn(
                    world,
                    "Grass tuft",
                    &[prototypes[0][2]],
                    grounded_grass(p, scale, 1.42 * rng.signed(), terrain, surface),
                );
                count += 1;
            }
        }
    }
    for row in 0..62 {
        for col in 0..42 {
            let z = -82. + (row as f32 + 0.4 * rng.signed()) * 0.55;
            let x =
                river::center(z) + river::width(z) - 1. + (col as f32 + 0.4 * rng.signed()) * 0.55;
            let p = Vec2::new(x, z);
            if rng.next() >= glade(p) || river::shore(x, z) < 1.2 {
                continue;
            }
            let kind = usize::from(rng.next() < 0.61);
            let scale = 0.61 + 0.44 * rng.next();
            spawn(
                world,
                "Grass tuft",
                &[prototypes[kind][3]],
                grounded_grass(p, scale, 0.84 * rng.signed(), terrain, surface),
            );
            count += 1;
        }
    }
    count
}

pub fn river_forest() -> Result<World> {
    let mut world = World::default();
    // Keep the two per-frame clocks directly accessible even in a large serialized world.
    environment(&mut world, Preset::River)?;
    river::install(&mut world);
    let (trees, grass) = prototypes(&mut world, true);
    let grass = grass_palette(&mut world, &grass);
    clusters::prepare(&mut world)?;
    let mut rng = Random::new(0x2173a879);
    let crowns = crowns(&mut rng);
    let terrain = Terrain::river();
    let (surface, mut shade) = terrain.build(&crowns);
    let grass_count = grasses(&mut world, &grass, &terrain, &shade, &surface, &mut rng);
    for &(model, kind) in &crowns {
        spawn(&mut world, "Forest tree", &trees[kind], model);
    }
    terrain.colour_riverbed(&mut shade);
    let texture = world.spawn(Entity {
        name: "Riverbank ground cover".into(),
        texture: Some(Arc::new(shade)),
        ..Default::default()
    });
    let bed = mesh(&mut world, "Forested river basin", surface, None);
    let soil = material(
        &mut world,
        "Riverbank moss and gravel",
        [1.; 3],
        GROUND,
        Some(texture),
    );
    spawn(
        &mut world,
        "Forested river basin",
        &[(bed, soil)],
        Mat4::IDENTITY,
    );
    eprintln!(
        "River forest: {} trees, {} grass tufts, {} m of channel along the valley axis",
        crowns.len(),
        grass_count,
        river::END - river::START
    );
    Ok(world)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dense_forest_preserves_the_open_channel_and_grounded_habitats() -> Result<()> {
        let world = river_forest()?;
        world.validate()?;
        let asset = |name: &str| {
            *world
                .entities
                .iter()
                .find(|(_, e)| e.name == name && e.mesh.is_some())
                .unwrap()
                .0
        };
        let grass_assets = [
            "Woodland short grass",
            "Meadow panicle grass",
            "Riverbank sedge",
        ]
        .map(asset);
        let bed = world.entities[&asset("Forested river basin")]
            .mesh
            .as_ref()
            .unwrap();
        let terrain = Terrain::river();
        let mut counts = [0; 3];
        let mut trees = 0;
        for e in world.entities.values() {
            let Some(i) = &e.instance else { continue };
            let p = e.local.w_axis;
            let d = river::shore(p.x, p.z);
            if let Some(kind) = grass_assets.iter().position(|&id| id == i.mesh) {
                counts[kind] += 1;
                assert!(d > 0.2);
                if kind == 2 {
                    assert!(d < 7.);
                }
                if counts[kind] % 151 == 0 {
                    let (h, n) = terrain.surface_at(bed, Vec2::new(p.x, p.z));
                    assert!((p.y - h + 0.017).abs() < 0.0001);
                    assert!(e.local.y_axis.truncate().normalize().dot(n) > 0.999);
                }
            } else if e.name == "Forest tree" {
                trees += 1;
                assert!(d >= 2.5);
            }
        }
        assert!(trees / 2 >= 20_000 && counts.iter().sum::<usize>() >= 200_000);
        assert!(counts.iter().all(|&n| n > 1000));
        assert_eq!(
            world
                .entities
                .values()
                .filter(|e| e.plant.is_some())
                .count(),
            11
        );
        Ok(())
    }
}
