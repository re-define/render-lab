mod exposure;
mod grassland;
pub(super) use grassland::river_grass;
mod tree;
pub(super) use tree::{Species, tree};

use crate::scene::{Mesh, PlantBone, PlantGeometry};
use glam::Vec3;
use std::f32::consts::TAU;

pub(super) struct Random(u32);
impl Random {
    pub fn new(seed: u32) -> Self {
        Self(seed)
    }
    pub fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f32 * (1. / 16777216.)
    }
    pub fn signed(&mut self) -> f32 {
        self.next() * 2. - 1.
    }
}

pub(super) fn empty() -> Mesh {
    Mesh {
        positions: vec![],
        indices: vec![],
        normals: vec![],
        uv: vec![],
        joints: vec![],
        weights: vec![],
    }
}

fn bone(bones: &mut Vec<PlantBone>, parent: Option<u16>, pivot: Vec3, bend: f32) -> u16 {
    let id = bones.len() as u16;
    bones.push(PlantBone {
        parent,
        pivot: pivot.into(),
        bend,
    });
    id
}

#[derive(Clone, Copy)]
struct Anchor {
    point: Vec3,
    joint: u16,
}

fn vertex(mesh: &mut Mesh, p: Vec3, n: Vec3, uv: [f32; 2], joints: [u16; 2], t: f32) {
    mesh.positions.push(p.into());
    mesh.normals.push(n.into());
    mesh.uv.push(uv);
    mesh.joints.push([joints[0], joints[1], 0, 0]);
    mesh.weights.push([1. - t, t, 0., 0.]);
}

fn tube(mesh: &mut Mesh, a: Anchor, b: Anchor, radius: [f32; 2], sides: u32) {
    let base = mesh.positions.len() as u32;
    let direction = (b.point - a.point).normalize();
    let reference = if direction.x.abs() < 0.8 {
        Vec3::X
    } else {
        Vec3::Z
    };
    let tangent = direction.cross(reference).normalize();
    let bitangent = direction.cross(tangent);
    for (anchor, r) in [(a, radius[0]), (b, radius[1])] {
        for lane in 0..sides {
            let (sine, cosine) = (TAU * lane as f32 / sides as f32).sin_cos();
            let n = tangent * cosine + bitangent * sine;
            vertex(
                mesh,
                anchor.point + n * r,
                n,
                [lane as f32 / sides as f32, anchor.point.y],
                [anchor.joint; 2],
                0.,
            );
        }
    }
    for lane in 0..sides {
        let a = base + lane;
        let b = base + (lane + 1) % sides;
        mesh.indices
            .extend([a, b, a + sides, b, b + sides, a + sides]);
    }
}

pub(super) fn grass(seed: u32) -> (Mesh, Vec<PlantBone>) {
    let mut rng = Random::new(seed);
    let mut mesh = empty();
    let mut bones = vec![];
    bone(&mut bones, None, Vec3::ZERO, 0.);
    for joint in 1..4 {
        bone(
            &mut bones,
            Some(joint - 1),
            Vec3::Y * (joint as f32 - 1.) * 0.22,
            0.11,
        );
    }
    for blade in 0..21 {
        let angle = TAU * (blade as f32 * 0.381966 + rng.next() * 0.14);
        let facing = Vec3::new(angle.cos(), 0., angle.sin());
        let side = Vec3::Y.cross(facing);
        let root = facing * (0.04 + rng.next() * 0.24);
        let height = 0.24 + rng.next() * 0.63;
        let sweep = (0.11 + rng.next() * 0.47) * (1.2 - height * 0.42);
        let tint = rng.next();
        let width = 0.021 + 0.024 * rng.next();
        let base = mesh.positions.len() as u32;
        for seg in 0..=4 {
            let t = seg as f32 * 0.25;
            let p = root + Vec3::Y * (height * t) + facing * (sweep * t.powi(2));
            let n = (facing * height - Vec3::Y * (2. * sweep * t)).normalize();
            let j = (seg as u16).min(3);
            let w = width * (1. - t * t).max(0.02);
            for sign in [-1., 1.] {
                vertex(
                    &mut mesh,
                    p + side * (sign * w),
                    n,
                    [tint, t],
                    [j, (j + 1).min(3)],
                    0.48,
                );
            }
            if seg > 0 {
                let i = base + 2 * (seg - 1);
                mesh.indices.extend([i, i + 1, i + 2, i + 1, i + 3, i + 2]);
            }
        }
    }
    (mesh, bones)
}

pub(super) fn rig(mesh: &Mesh, bones: &[PlantBone], cell_size: Option<f32>) -> PlantGeometry {
    let mut displacement_bound = 0f32;
    for (&p, (&joints, &weights)) in mesh
        .positions
        .iter()
        .zip(mesh.joints.iter().zip(&mesh.weights))
    {
        for (&j, &w) in joints.iter().zip(&weights) {
            if w == 0. {
                continue;
            }
            let mut bound = 0.;
            let mut next = Some(j);
            while let Some(id) = next {
                let b = bones[id as usize];
                // Each ancestor's max point displacement is its rotation chord at wind 2.
                bound += 2. * b.bend.sin() * Vec3::from(p).distance(Vec3::from(b.pivot));
                next = b.parent;
            }
            displacement_bound = displacement_bound.max(bound);
        }
    }
    PlantGeometry {
        bones: bones.to_vec(),
        displacement_bound: displacement_bound + 0.0001,
        voxel_cell_size: cell_size,
    }
}
