//! A seeded limestone valley for comparing canopy light against illuminated and shaded cliffs.
use super::{generate::Random, *};
use glam::{IVec3, Vec2};
use std::collections::HashMap;
use std::f32::consts::TAU;

fn ground(p: Vec2) -> f32 {
    let back = (-p.y - 53.).max(0.);
    let foothill = (0.22 * back).min(42.) * (0.72 + 0.23 * (p.x * 0.042 - p.y * 0.018).sin());
    foothill
        + 3.6 * (p.x * 0.043 + p.y * 0.026).sin()
        + 2.7 * (p.y * 0.058 - p.x * 0.028).cos()
        + 0.85 * (p.x * 0.15 + p.y * 0.10).sin()
}

fn valley_floor() -> Mesh {
    let mut mesh = generate::empty();
    let (cols, rows) = (206, 166);
    for z in 0..=rows {
        for x in 0..=cols {
            let p = Vec2::new((x as f32 - 103.) * 3.2, 84. - z as f32 * 3.2);
            let h = ground(p);
            let normal = Vec3::new(
                h - ground(p + Vec2::X * 0.15),
                0.15,
                h - ground(p + Vec2::Y * 0.15),
            )
            .normalize();
            mesh.positions.push([p.x, h, p.y]);
            mesh.normals.push(normal.into());
            mesh.uv.push((p / 46.).to_array());
            if x < cols && z < rows {
                let i = z * (cols + 1) + x;
                mesh.indices
                    .extend([i, i + 1, i + cols + 1, i + 1, i + cols + 2, i + cols + 1]);
            }
        }
    }
    mesh
}

struct Pillar {
    center: Vec2,
    radius: Vec2,
    height: f32,
    angle: f32,
    seed: f32,
}

fn erosion(p: Vec3) -> f32 {
    let cell = p.floor().as_ivec3();
    let frac = p - p.floor();
    let f = frac * frac * (3. - 2. * frac);
    let mut value = 0.;
    for z in 0..2 {
        for y in 0..2 {
            for x in 0..2 {
                let offset = IVec3::new(x, y, z);
                let q = (cell + offset).as_uvec3();
                let mut h = q.x.wrapping_mul(0x8da6_b343)
                    ^ q.y.wrapping_mul(0xd816_3841)
                    ^ q.z.wrapping_mul(0xcb1a_b31f);
                h = (h ^ (h >> 16)).wrapping_mul(0x85eb_ca6b);
                let weight = (Vec3::ONE - (f - offset.as_vec3()).abs()).element_product();
                value += weight * ((h ^ (h >> 13)) >> 8) as f32 * (2. / 16_777_216.);
            }
        }
    }
    value - 1.
}

impl Pillar {
    fn top(&self) -> Vec3 {
        Vec3::new(
            self.center.x,
            ground(self.center) + self.height,
            self.center.y,
        )
    }
    fn distance(&self, p: Vec2) -> f32 {
        let local = Vec2::from_angle(-self.angle).rotate(p - self.center);
        (local / self.radius).length()
    }
    fn ring(&self, i: usize, y: f32, width: f32) -> Vec3 {
        let a = i as f32 * TAU / SIDES as f32;
        let dir = Vec2::from_angle(a);
        let square = (dir.x.abs().powi(6) + dir.y.abs().powi(6)).powf(-1. / 6.);
        let joint = 1.
            + 0.100 * (3. * a + self.seed).sin()
            + 0.064 * (7. * a - self.seed).cos()
            + 0.043 * (11. * a + self.seed * 2.).sin();
        let t = (y / self.height).clamp(0., 1.);
        let stone = dir * self.radius * square;
        let field = Vec3::new(stone.x + self.center.x, y, stone.y + self.center.y);
        let ribs = erosion(field * Vec3::new(0.52, 0.085, 0.52));
        let bulk = erosion(field * Vec3::new(0.23, 0.15, 0.23));
        let chips = erosion(field * 0.86);
        let taper = 0.93 - 0.32 * t + 0.34 * (1. - t).powi(3);
        let shelf = (0.5 + 0.5 * erosion(field * Vec3::new(0.31, 0.10, 0.31))).powi(2);
        let profile = taper - (0.12 + 0.09 * (a + self.seed).sin()) * t.powi(3);
        let relief =
            self.radius.min_element().min(5.) * (0.20 * bulk + 0.22 * ribs + 0.040 * chips);
        let local = stone * (joint * profile * (1. + (width - 1.) * shelf)) + dir * relief;
        let lean = Vec2::from_angle(self.seed) * self.radius.min_element() * 0.15 * t;
        let p = self.center + Vec2::from_angle(self.angle).rotate(local) + lean;
        let dip = 0.08 * local.x - 0.050 * local.y + 0.25 * bulk
            - t.powi(7) * self.radius.min_element() * (0.11 + 0.18 * ribs).max(0.);
        Vec3::new(p.x, ground(self.center) + y + dip, p.y)
    }
    fn append(&self, mesh: &mut Mesh) {
        let beds = (self.height / 1.45).ceil().max(2.) as usize;
        let step = self.height / beds as f32;
        let mut ring: [Vec3; SIDES] = std::array::from_fn(|i| self.ring(i, -3., 1.10));
        for bed in 0..beds {
            let bed_noise = (self.seed * 1.37 + bed as f32 * 2.43).sin();
            let h = step * ((bed + 1) as f32 + if bed + 1 < beds { 0.22 * bed_noise } else { 0. });
            let relief = 0.12 * (bed_noise + 0.2).max(0.);
            for (dy, width) in [(-0.28 * step, 1.), (0., 1. - relief)] {
                let next = std::array::from_fn(|i| self.ring(i, h + dy, width));
                for i in 0..SIDES {
                    let j = (i + 1) % SIDES;
                    triangle(mesh, ring[i], next[i], ring[j]);
                    triangle(mesh, ring[j], next[i], next[j]);
                }
                ring = next;
            }
        }
        let top = self.top();
        let inner: [Vec3; SIDES] = std::array::from_fn(|i| {
            let mut p = top.lerp(ring[i], 0.58);
            p.y = top.y
                + (p.y - top.y) * 0.35
                + self.radius.min_element().min(5.) * 0.085 * erosion(p * 0.46);
            p
        });
        for i in 0..SIDES {
            let j = (i + 1) % SIDES;
            triangle(mesh, top, inner[j], inner[i]);
            triangle(mesh, inner[i], inner[j], ring[i]);
            triangle(mesh, ring[i], inner[j], ring[j]);
        }
    }
}
const SIDES: usize = 37;

fn triangle(mesh: &mut Mesh, a: Vec3, b: Vec3, c: Vec3) {
    let i = mesh.positions.len() as u32;
    let n = (b - a).cross(c - a).normalize();
    mesh.positions.extend([a, b, c].map(|p| p.to_array()));
    mesh.normals.extend([n.to_array(); 3]);
    // Secondary GI samples the same rock asset through a dominant-plane UV.
    let axis = n.abs();
    mesh.uv.extend([a, b, c].map(|p| {
        let uv = if axis.y > axis.x.max(axis.z) {
            Vec2::new(p.x, p.z)
        } else if axis.x > axis.z {
            Vec2::new(p.z, p.y)
        } else {
            Vec2::new(p.x, p.y)
        };
        (uv / 5.6).to_array()
    }));
    mesh.indices.extend([i, i + 1, i + 2]);
}

fn soften_stone(mesh: &mut Mesh) {
    let mut fans = HashMap::<[u32; 3], Vec<Vec3>>::new();
    for (&p, &n) in mesh.positions.iter().zip(&mesh.normals) {
        fans.entry(p.map(f32::to_bits))
            .or_default()
            .push(Vec3::from(n));
    }
    for (&p, n) in mesh.positions.iter().zip(&mut mesh.normals) {
        let face = Vec3::from(*n);
        let smooth = fans[&p.map(f32::to_bits)]
            .iter()
            .filter(|v| v.dot(face) > 0.55)
            .copied()
            .sum::<Vec3>()
            .normalize_or(face);
        *n = face.lerp(smooth, 0.82).normalize().into();
    }
}

fn outcrops(rng: &mut Random) -> Vec<Pillar> {
    let mut rocks = Vec::new();
    // The main wall stands to the right of the valley. Smaller joints break each
    // massif into separate summits, with taller tors rising behind the front row.
    let ridges = [
        (-19., -30., 7.4, 8.6, 34.),
        (-4., -53., 6.3, 6.8, 47.),
        (11., -39., 3.6, 5.0, 27.),
        (-3., -19., 5.1, 4.4, 16.),
        (5., -25., 3.3, 4.3, 20.),
        (-23., -16., 4.0, 5.0, 13.),
        (42., -50., 11., 12., 42.),
        (55., -66., 14., 12., 49.),
        (35., -67., 6.1, 7.6, 35.),
        (-58., -78., 6.5, 7.8, 17.),
        (-48., -105., 8., 10., 25.),
        (6., -119., 7., 9., 19.),
        (72., -112., 13., 12., 42.),
        (-82., -143., 13., 12., 28.),
        (-27., -157., 8., 11., 21.),
    ];
    for (x, z, rx, rz, h) in ridges {
        let center = Vec2::new(x, z);
        let radius = Vec2::new(rx, rz);
        let angle = rng.signed() * 0.85;
        rocks.push(Pillar {
            center,
            radius,
            height: h,
            angle,
            seed: rng.next() * TAU,
        });
        let phase = rng.next() * TAU;
        for k in 0..7 {
            let dir = Vec2::from_angle(phase + TAU * (k as f32 + 0.28 * rng.signed()) / 7.);
            let reach = 0.67 + 0.39 * rng.next();
            let p = center + Vec2::from_angle(angle).rotate(dir * radius) * reach;
            rocks.push(Pillar {
                center: p,
                radius: radius * (0.26 + 0.31 * rng.next()),
                height: h * (0.19 + 0.55 * rng.next()),
                angle: angle + 0.30 * rng.signed(),
                seed: rng.next() * TAU,
            });
        }
        for _ in 0..11 {
            let p =
                center + Vec2::from_angle(rng.next() * TAU) * radius * (1.12 + 0.65 * rng.next());
            let size = 0.65 + 1.60 * rng.next();
            rocks.push(Pillar {
                center: p,
                radius: Vec2::new(size, 0.75 * size),
                height: size * (0.45 + rng.next()),
                angle: rng.next() * TAU,
                seed: rng.next() * TAU,
            });
        }
    }
    rocks
}

fn scanned_material(
    world: &mut World,
    name: &str,
    program: &str,
    bytes: &[u8],
    extent: u32,
) -> Result<Id> {
    let scan = image::load_from_memory(bytes)?
        .resize_exact(extent, extent, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    let texture = world.spawn(Entity {
        name: name.into(),
        texture: Some(Arc::new(Texture {
            width: scan.width(),
            height: scan.height(),
            rgba: scan.into_raw(),
        })),
        ..Default::default()
    });
    Ok(material(world, name, [1.; 3], program, Some(texture)))
}

pub(in crate::experiments) fn scene() -> Result<World> {
    let mut world = World::default();
    environment(&mut world, Preset::Karst)?;
    let trees = tree_prototypes(&mut world);
    let mut dry_grove = trees.clone();
    for (i, color) in [(2, [0.285, 0.265, 0.119]), (3, [0.226, 0.200, 0.086])] {
        dry_grove[i][1].1 = material(&mut world, "Dry ridge foliage", color, PROGRAM, None);
    }
    let mut rng = Random::new(0x40b3_8173);
    let rocks = outcrops(&mut rng);
    let mut stone = generate::empty();
    for rock in &rocks {
        rock.append(&mut stone);
    }
    soften_stone(&mut stone);
    let limestone = scanned_material(
        &mut world,
        "Weathered limestone",
        "GI limestone",
        include_bytes!("../../../assets/environments/rock_face_03_diff_1k.jpg"),
        256,
    )?;
    let soil = scanned_material(
        &mut world,
        "Karst woodland soil",
        "GI woodland soil",
        include_bytes!("../../../assets/environments/rocky_terrain_diff_1k.jpg"),
        512,
    )?;
    for (name, surface, mat) in [
        ("Karst valley floor", valley_floor(), soil),
        ("Jointed limestone towers", stone, limestone),
    ] {
        let geometry = mesh(&mut world, name, surface, None);
        spawn(&mut world, name, &[(geometry, mat)], Mat4::IDENTITY);
    }
    for row in 0..48 {
        for col in 0..50 {
            let p = Vec2::new(
                (col as f32 - 25.) * 5.65 + rng.signed() * 2.62,
                53. - row as f32 * 5.65 + rng.signed() * 2.62,
            );
            let patch =
                (p.x * 0.079 + p.y * 0.061).sin() + 0.64 * (p.y * 0.103 - p.x * 0.041).cos();
            if rng.next() < 0.16 + 0.18 * patch || rocks.iter().any(|r| r.distance(p) < 1.13) {
                continue;
            }
            let species = if rng.next() < 0.12 {
                usize::from(rng.next() > 0.30)
            } else if rng.next() < 0.58 {
                3
            } else {
                2
            };
            let scale = 0.57 + 0.82 * rng.next() + 0.19 * patch;
            let breadth = 0.88 + 0.42 * rng.next();
            let transform = Mat4::from_scale_rotation_translation(
                Vec3::new(
                    scale * breadth,
                    scale,
                    scale * (breadth + 0.12 * rng.signed()),
                ),
                Quat::from_rotation_y(TAU * rng.next())
                    * Quat::from_rotation_z(rng.signed() * 0.09),
                Vec3::new(p.x, ground(p) - 0.075, p.y),
            );
            let palette = if patch > 0.32 && (p.x * 0.97 + p.y * 1.61).sin() > 0.43 {
                &dry_grove
            } else {
                &trees
            };
            spawn(&mut world, "Forest tree", &palette[species], transform);
        }
    }
    // Saplings on the talus break up the bare band between crowns and cliff bases.
    for rock in rocks.iter().filter(|r| r.height > 8.) {
        for _ in 0..12 {
            let p = rock.center
                + Vec2::from_angle(rng.next() * TAU) * rock.radius * (1.2 + 0.85 * rng.next());
            if rocks.iter().any(|r| r.distance(p) < 1.04) {
                continue;
            }
            let scale = 0.23 + 0.37 * rng.next();
            let model = Mat4::from_scale_rotation_translation(
                Vec3::new(1.3, 0.91, 1.2) * scale,
                Quat::from_rotation_y(TAU * rng.next()),
                Vec3::new(p.x, ground(p) - 0.075, p.y),
            );
            spawn(
                &mut world,
                "Talus sapling",
                &trees[if rng.next() < 0.65 { 3 } else { 2 }],
                model,
            );
        }
    }
    for r in rocks.iter().filter(|r| r.height > 5.) {
        if rng.next() < 0.28
            || rocks
                .iter()
                .any(|other| other.top().y > r.top().y && other.distance(r.center) < 0.96)
        {
            continue;
        }
        let transform = Mat4::from_scale_rotation_translation(
            Vec3::splat(0.25 + 0.33 * rng.next()),
            Quat::from_rotation_y(TAU * rng.next()),
            r.top() - 0.06 * Vec3::Y,
        );
        spawn(
            &mut world,
            "Ledge tree",
            &trees[if rng.next() < 0.47 { 2 } else { 3 }],
            transform,
        );
    }
    clusters::prepare(&mut world)?;
    Ok(world)
}
