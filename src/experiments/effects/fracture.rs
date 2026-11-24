use super::*;
use glam::Quat;
use std::f32::consts::TAU;

const SHARD: &str = "lab.fracture.shard";
const LEFT: Vec3 = Vec3::new(-2.02, 0.12, 0.);
const RIGHT: Vec3 = Vec3::new(2.02, 0.12, 0.);
const PERIOD: f64 = 5.4;

#[derive(Clone)]
struct Face {
    points: Vec<Vec3>,
    interior: bool,
}
type Solid = Vec<Face>;

fn shell() -> Solid {
    const SIDES: usize = 9;
    let rings: Vec<Vec<Vec3>> = [(0., 0.62), (0.38, 0.98), (1.75, 0.89), (2.36, 0.49)]
        .into_iter()
        .map(|(y, r)| {
            (0..SIDES)
                .map(|s| {
                    let a = TAU * s as f32 / SIDES as f32 + 0.18;
                    Vec3::new(a.cos() * r + 0.075 * y, y, a.sin() * r * 0.89)
                })
                .collect()
        })
        .collect();
    let mut polygons = vec![rings[0].clone(), rings[3].iter().rev().copied().collect()];
    for pair in rings.windows(2) {
        for i in 0..SIDES {
            let j = (i + 1) % SIDES;
            polygons.push(vec![pair[0][i], pair[1][i], pair[1][j], pair[0][j]]);
        }
    }
    polygons
        .into_iter()
        .map(|points| Face {
            points,
            interior: false,
        })
        .collect()
}
fn push_distinct(points: &mut Vec<Vec3>, point: Vec3) {
    if !points.iter().any(|v| v.distance_squared(point) < 1e-10) {
        points.push(point);
    }
}
// Clip a closed convex cell to the half space and seal its newly exposed cross-section.
fn clip(faces: Solid, n: Vec3, offset: f32) -> Solid {
    let mut kept = Vec::new();
    let mut rim = Vec::new();
    for face in faces {
        let mut points = Vec::new();
        let mut a = *face.points.last().unwrap();
        let mut da = a.dot(n) - offset;
        for &b in &face.points {
            let db = b.dot(n) - offset;
            if (da <= 0.) != (db <= 0.) {
                let hit = a.lerp(b, da / (da - db));
                push_distinct(&mut points, hit);
                push_distinct(&mut rim, hit);
            }
            if db <= 0. {
                push_distinct(&mut points, b);
            }
            a = b;
            da = db;
        }
        if points.len() >= 3 {
            kept.push(Face {
                points,
                interior: face.interior,
            });
        }
    }
    if rim.len() >= 3 {
        let center = rim.iter().copied().sum::<Vec3>() / rim.len() as f32;
        let tangent = n.any_orthonormal_vector();
        let bitangent = n.cross(tangent);
        rim.sort_by(|&a, &b| {
            let angle = |p: Vec3| {
                let d = p - center;
                d.dot(bitangent).atan2(d.dot(tangent))
            };
            angle(a).total_cmp(&angle(b))
        });
        kept.push(Face {
            points: rim,
            interior: true,
        });
    }
    kept
}
fn into_mesh(faces: &[Face]) -> Mesh {
    let mut mesh = Mesh {
        positions: vec![],
        indices: vec![],
        normals: vec![],
        uv: vec![],
        joints: vec![],
        weights: vec![],
    };
    for f in faces {
        let a = f.points[0];
        let area = f.points[1..]
            .windows(2)
            .map(|p| (p[0] - a).cross(p[1] - a))
            .sum::<Vec3>();
        let Some(n) = area.try_normalize() else {
            continue;
        };
        let start = mesh.positions.len() as u32;
        for &point in &f.points {
            mesh.positions.push(point.into());
            mesh.normals.push(n.into());
            mesh.uv.push([u32::from(f.interior) as f32, 0.]);
        }
        for i in 1..f.points.len() - 1 {
            mesh.indices
                .extend([start, start + i as u32, start + i as u32 + 1]);
        }
    }
    mesh
}
fn sites() -> Vec<Vec3> {
    let mut seed = 0x5465_92afu32;
    let mut random = || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (seed >> 8) as f32 / 16_777_216.
    };
    let mut sites = Vec::new();
    for y in 0..4 {
        for z in 0..3 {
            for x in 0..3 {
                sites.push(Vec3::new(
                    (x as f32 - 1. + (random() - 0.5) * 0.76) * 0.66,
                    (y as f32 + 0.5 + (random() - 0.5) * 0.68) * 0.59,
                    (z as f32 - 1. + (random() - 0.5) * 0.76) * 0.58,
                ));
            }
        }
    }
    sites
}
fn voronoi(body: &Solid, sites: &[Vec3], index: usize) -> Solid {
    let site = sites[index];
    let mut cell = body.clone();
    for (j, &other) in sites.iter().enumerate() {
        if j != index && !cell.is_empty() {
            let normal = (other - site).normalize();
            cell = clip(cell, normal, normal.dot((other + site) * 0.5));
        }
    }
    cell
}
#[derive(Serialize, Deserialize)]
struct Shard {
    pivot: Vec3,
    velocity: Vec3,
    spin: Vec3,
}
fn transform(shard: &Shard, mesh: &Mesh, t: f32) -> Mat4 {
    let t = (t - 0.8).max(0.);
    if t == 0. {
        return Mat4::from_translation(LEFT);
    }
    let v = shard.velocity;
    let flight = (v.y + (v.y * v.y + 19.62 * shard.pivot.y).sqrt()) / 9.81;
    let sliding = (t - flight).max(0.);
    let travel = t.min(flight) + (1. - (-5. * sliding).exp()) / 5.;
    let angle = shard.spin.length() * travel;
    let rotation = Quat::from_axis_angle(shard.spin.normalize(), angle);
    let bottom = mesh
        .positions
        .iter()
        .map(|p| (rotation * (Vec3::from(*p) - shard.pivot)).y)
        .fold(f32::INFINITY, f32::min);
    let y = (shard.pivot.y + v.y * t - 4.905 * t * t).max(-bottom);
    let center = LEFT
        + Vec3::new(
            shard.pivot.x + v.x * travel,
            y,
            shard.pivot.z + v.z * travel,
        );
    Mat4::from_rotation_translation(rotation, center) * Mat4::from_translation(-shard.pivot)
}
pub(super) struct Piece {
    pub(super) id: Id,
    mesh: Arc<Mesh>,
    shard: Shard,
}
impl Piece {
    pub fn pose(&self, time: f64) -> (Id, Mat4) {
        (
            self.id,
            transform(&self.shard, &self.mesh, (time % PERIOD) as f32),
        )
    }
}
pub(super) fn pieces(world: &World) -> Result<Vec<Piece>> {
    world
        .entities
        .iter()
        .filter_map(|(&id, e)| e.components.get(SHARD).map(|raw| (id, e, raw)))
        .map(|(id, e, raw)| {
            let shard: Shard = serde_json::from_value(raw.clone())?;
            ensure!(
                shard.pivot.is_finite()
                    && shard.velocity.is_finite()
                    && shard.spin.is_finite()
                    && shard.spin.length_squared() > 1e-8,
                "invalid shard motion"
            );
            let mesh = e
                .instance
                .as_ref()
                .and_then(|i| world.entities.get(&i.mesh))
                .and_then(|e| e.mesh.as_ref())
                .context("shard mesh missing")?;
            Ok(Piece {
                id,
                mesh: mesh.clone(),
                shard,
            })
        })
        .collect()
}
pub(super) fn dissolve(time: f64) -> f32 {
    let u = (((time % PERIOD) as f32 - 0.72) / 2.96).clamp(0., 1.);
    -0.18 + 1.46 * (u * u * (3. - 2. * u))
}
pub(super) fn scene() -> Result<World> {
    let mut world = setup(Kind::Destruction);
    let cube = mesh(
        &mut world,
        "Unit block",
        super::super::lighting::scenes::cube(),
    );
    let floor = material(&mut world, "Gallery slate", STONE, [0.14, 0.17, 0.20]);
    let deck = material(
        &mut world,
        "Slate display platform",
        STONE,
        [0.23, 0.28, 0.28],
    );
    let edge = material(&mut world, "Platform inlay", STONE, [0.54, 0.32, 0.13]);
    slab(
        &mut world,
        "Gallery floor",
        cube,
        floor,
        [0., -0.26, 0.],
        [40., 0.5, 40.],
    );
    slab(
        &mut world,
        "Specimen platform",
        cube,
        deck,
        [0., 0.052, 0.],
        [10., 0.136, 6.0],
    );
    slab(
        &mut world,
        "Front inlay",
        cube,
        edge,
        [0., 0.027, 3.007],
        [10., 0.038, 0.02],
    );
    let left = material(&mut world, "Malachite", ORE, [0.055, 0.37, 0.26]);
    let right = material(&mut world, "Amethyst", DISSOLVE, [0.28, 0.12, 0.46]);
    let body = shell();
    let whole = mesh(&mut world, "Intact crystal", into_mesh(&body));
    world.set_setting(whole, crate::experiments::clusters::ORDINARY, true.into());
    instance(
        &mut world,
        "Eroding crystal",
        whole,
        right,
        Mat4::from_translation(RIGHT),
    );
    let seeds = sites();
    for i in 0..seeds.len() {
        let fragments = voronoi(&body, &seeds, i);
        let geometry = into_mesh(&fragments);
        if geometry.indices.is_empty() {
            continue;
        }
        let pivot = geometry
            .positions
            .iter()
            .map(|&p| Vec3::from(p))
            .sum::<Vec3>()
            / geometry.positions.len() as f32;
        let a = i as f32 * 2.39996;
        let outward =
            Vec3::new(pivot.x + 0.12 * a.sin(), 0., pivot.z + 0.12 * a.cos()).normalize_or_zero();
        let velocity =
            outward * (0.72 + 0.43 * (a.sin() + 1.)) + Vec3::Y * (1.5 + 0.75 * (a.cos() + 1.));
        let shard = Shard {
            pivot,
            velocity,
            spin: Vec3::new(a.sin() * 2.7, 0.82, a.cos() * 2.9),
        };
        let part = mesh(&mut world, "Convex fracture cell", geometry);
        let id = instance(
            &mut world,
            "Crystal fragment",
            part,
            left,
            Mat4::from_translation(LEFT),
        );
        world
            .entities
            .get_mut(&id)
            .unwrap()
            .components
            .insert(SHARD.into(), serde_json::to_value(shard)?);
    }
    world.validate()?;
    Ok(world)
}
