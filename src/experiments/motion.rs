//! Brute-force motion matching reference. All distances and velocities are in character space.
use crate::{
    engine::{Edit, System},
    scene::*,
};
use anyhow::{Context, Result, ensure};
use glam::{Mat4, Quat, Vec2, Vec3};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
pub mod silver;

pub const PLAYER: &str = "lab.motion_matching.player";
const HZ: f32 = 30.;
const FRAMES: usize = 48;
const HORIZONS: [f32; 3] = [6. / HZ, 14. / HZ, 23. / HZ];
const BONE_COUNT: usize = 7;
// Hips, left thigh and shin, right thigh and shin, left and right arms.
const OFFSETS: [Vec3; BONE_COUNT] = [
    Vec3::new(0., 1.15, 0.),
    Vec3::new(-0.17, 0., 0.),
    Vec3::new(0., -0.47, 0.),
    Vec3::new(0.17, 0., 0.),
    Vec3::new(0., -0.47, 0.),
    Vec3::new(-0.36, 0.54, 0.),
    Vec3::new(0.36, 0.54, 0.),
];
const PARENTS: [Option<usize>; BONE_COUNT] =
    [None, Some(0), Some(1), Some(0), Some(3), Some(0), Some(0)];
#[derive(Clone, Serialize, Deserialize)]
pub struct Player {
    pub bones: [Id; BONE_COUNT],
    pub speed: f32,
    pub turn: f32,
    pub sample: usize,
    pub fraction: f32,
    pub angles: [f32; BONE_COUNT],
    pub clip: String,
    pub cost: f32,
}
#[derive(Clone)]
pub struct Sample {
    pub angles: [f32; BONE_COUNT],
    pub feet: [Vec3; 2],
    pub velocity: [Vec3; 2],
    pub trajectory: [Vec2; 3],
    pub facing: [Vec2; 3],
}
pub struct Clip {
    pub name: String,
    pub samples: Vec<Sample>,
}
pub struct Matcher {
    clips: Vec<Clip>,
}
fn trajectory(speed: f32, turn: f32) -> ([Vec2; 3], [Vec2; 3]) {
    let positions = HORIZONS.map(|t| {
        if turn.abs() < 0.0001 {
            Vec2::new(0., speed * t)
        } else {
            Vec2::new(1. - (turn * t).cos(), (turn * t).sin()) * (speed / turn)
        }
    });
    (
        positions,
        HORIZONS.map(|t| Vec2::new((turn * t).sin(), (turn * t).cos())),
    )
}
fn rest_pose(angles: [f32; BONE_COUNT]) -> [Mat4; BONE_COUNT] {
    let mut result = [Mat4::IDENTITY; BONE_COUNT];
    for i in 0..BONE_COUNT {
        let local = Mat4::from_rotation_translation(Quat::from_rotation_x(angles[i]), OFFSETS[i]);
        result[i] = PARENTS[i].map_or(local, |parent| result[parent] * local);
    }
    result
}
fn feet(angles: [f32; BONE_COUNT]) -> [Vec3; 2] {
    let m = rest_pose(angles);
    [2, 4].map(|i| m[i].transform_point3(Vec3::new(0., -0.44, 0.)))
}
fn gait(phase: f32, speed: f32, turn: f32) -> [f32; BONE_COUNT] {
    let s = phase.sin();
    let amplitude = speed.min(3.) * 0.26;
    [
        0.025 * speed,
        amplitude * s * (1. - turn * 0.2),
        -amplitude * (-s).max(0.) * 1.2,
        -amplitude * s * (1. + turn * 0.2),
        -amplitude * s.max(0.) * 1.2,
        -s * amplitude * 0.85,
        s * amplitude * 0.85,
    ]
}
impl Default for Matcher {
    fn default() -> Self {
        let mut clips = vec![];
        for (name, speed, turn, cycles) in [
            ("Idle", 0., 0., 1.),
            ("Walk", 1.35, 0., 1.),
            ("Run", 3., 0., 2.),
            ("Turn left", 1.35, -1., 1.),
            ("Turn right", 1.35, 1., 1.),
        ] {
            let (trajectory, facing) = trajectory(speed, turn);
            let samples = (0..FRAMES)
                .map(|i| {
                    let at = |index: usize| {
                        gait(
                            index as f32 / FRAMES as f32 * std::f32::consts::TAU * cycles,
                            speed,
                            turn,
                        )
                    };
                    let angles = at(i);
                    let feet_now = feet(angles);
                    let previous = feet(at((i + FRAMES - 1) % FRAMES));
                    Sample {
                        angles,
                        feet: feet_now,
                        velocity: std::array::from_fn(|j| (feet_now[j] - previous[j]) * HZ),
                        trajectory,
                        facing,
                    }
                })
                .collect();
            clips.push(Clip {
                name: name.into(),
                samples,
            });
        }
        Self { clips }
    }
}
impl Matcher {
    /// External samplers can supply the same feature schema at 30 Hz for this rig.
    pub fn from_clips(clips: Vec<Clip>) -> Result<Self> {
        ensure!(
            !clips.is_empty() && clips.iter().all(|c| !c.samples.is_empty()),
            "empty motion database"
        );
        Ok(Self { clips })
    }
    fn sample(&self, mut index: usize) -> Option<(usize, usize)> {
        for (clip, c) in self.clips.iter().enumerate() {
            if index < c.samples.len() {
                return Some((clip, index));
            }
            index -= c.samples.len();
        }
        None
    }
    fn advance(&self, p: &mut Player, dt: f32) -> Result<()> {
        ensure!(
            dt.is_finite() && dt >= 0. && p.speed.is_finite() && p.turn.is_finite(),
            "invalid motion query"
        );
        let (goal, heading) = trajectory(p.speed, p.turn);
        p.fraction += dt.min(0.25) * HZ;
        while p.fraction >= 1. {
            p.fraction -= 1.;
            let (ci, fi) = self
                .sample(p.sample)
                .context("saved motion sample outside database")?;
            let clip = &self.clips[ci];
            let next = (fi + 1) % clip.samples.len();
            let predicted = &clip.samples[next];
            let mut best = (f32::INFINITY, 0, 0, 0);
            let mut base = 0;
            for (c, candidate) in self.clips.iter().enumerate() {
                for (f, s) in candidate.samples.iter().enumerate() {
                    let mut cost = 0.;
                    for j in 0..3 {
                        cost += 3. * goal[j].distance_squared(s.trajectory[j])
                            + heading[j].distance_squared(s.facing[j]);
                    }
                    for j in 0..2 {
                        cost += predicted.feet[j].distance_squared(s.feet[j])
                            + 0.035 * predicted.velocity[j].distance_squared(s.velocity[j]);
                    }
                    if (ci, next) != (c, f) {
                        cost += 0.02;
                    }
                    if cost < best.0 {
                        best = (cost, base + f, c, f);
                    }
                }
                base += candidate.samples.len();
            }
            p.sample = best.1;
            p.clip = self.clips[best.2].name.clone();
            p.cost = best.0;
            let target = self.clips[best.2].samples[best.3].angles;
            for (value, target) in p.angles.iter_mut().zip(target) {
                *value += (target - *value) * 0.55;
            }
        }
        Ok(())
    }
}
impl System for Matcher {
    fn update(&self, w: &World, dt: f32) -> Result<Edit> {
        let mut changes = vec![];
        for (&id, e) in &w.entities {
            if let Some(raw) = e.components.get(PLAYER) {
                let mut p: Player = serde_json::from_value(raw.clone())?;
                ensure!(
                    p.bones.iter().all(|i| w.entities.contains_key(i)),
                    "motion player refers to missing bones"
                );
                self.advance(&mut p, dt)?;
                let transforms = p
                    .bones
                    .into_iter()
                    .enumerate()
                    .map(|(i, j)| {
                        (
                            j,
                            Mat4::from_rotation_translation(
                                Quat::from_rotation_x(p.angles[i]),
                                OFFSETS[i],
                            ),
                        )
                    })
                    .collect::<Vec<_>>();
                changes.push((id, serde_json::to_value(p)?, transforms));
            }
        }
        Ok(Box::new(move |w| {
            for (id, state, transforms) in changes {
                for (bone, local) in transforms {
                    if let Some(e) = w.entities.get_mut(&bone) {
                        e.local = local;
                    }
                }
                if let Some(e) = w.entities.get_mut(&id) {
                    e.components.insert(PLAYER.into(), state);
                }
            }
        }))
    }
}
pub fn set_goal(w: &mut World, speed: f32, turn: f32) -> Result<()> {
    ensure!(speed.is_finite() && turn.is_finite(), "invalid motion goal");
    let raw = w
        .entities
        .values_mut()
        .find_map(|e| e.components.get_mut(PLAYER))
        .context("this scene has no motion player")?;
    let mut player: Player = serde_json::from_value(raw.clone())?;
    player.speed = speed;
    player.turn = turn;
    *raw = serde_json::to_value(player)?;
    Ok(())
}
pub fn scene() -> World {
    let mut w = World::default();
    let mut bones = [0; BONE_COUNT];
    for i in 0..BONE_COUNT {
        bones[i] = w.spawn(Entity {
            name: [
                "Hips",
                "Left thigh",
                "Left shin",
                "Right thigh",
                "Right shin",
                "Left arm",
                "Right arm",
            ][i]
                .into(),
            parent: PARENTS[i].map(|j| bones[j]),
            local: Mat4::from_translation(OFFSETS[i]),
            ..Default::default()
        });
    }
    let rest = rest_pose([0.; BONE_COUNT]);
    let skin = w.spawn(Entity {
        skin: Some(Skin {
            joints: bones.to_vec(),
            inverse_bind: rest.iter().map(|m| m.inverse()).collect(),
        }),
        ..Default::default()
    });
    let mut mesh = Mesh {
        positions: vec![],
        indices: vec![],
        normals: vec![],
        uv: vec![],
        joints: vec![],
        weights: vec![],
    };
    // One skinned mesh with rigid segments and conventional joint/weight streams.
    for (bone, center, half) in [
        (0, [0., 0.26, 0.], [0.29, 0.32, 0.14]),
        (0, [0., 0.81, 0.], [0.15, 0.18, 0.15]),
        (1, [0., -0.22, 0.], [0.10, 0.22, 0.11]),
        (2, [0., -0.21, 0.], [0.08, 0.21, 0.09]),
        (3, [0., -0.22, 0.], [0.10, 0.22, 0.11]),
        (4, [0., -0.21, 0.], [0.08, 0.21, 0.09]),
        (2, [0., -0.42, 0.07], [0.09, 0.06, 0.15]),
        (4, [0., -0.42, 0.07], [0.09, 0.06, 0.15]),
        (5, [0., -0.29, 0.], [0.07, 0.29, 0.08]),
        (6, [0., -0.29, 0.], [0.07, 0.29, 0.08]),
    ] {
        let base = mesh.positions.len() as u32;
        for v in 0..8 {
            let corner = Vec3::new(
                if v & 1 == 0 { -1. } else { 1. },
                if v & 2 == 0 { -1. } else { 1. },
                if v & 4 == 0 { -1. } else { 1. },
            );
            mesh.positions.push(
                rest[bone]
                    .transform_point3(Vec3::from(center) + corner * Vec3::from(half))
                    .into(),
            );
            mesh.joints.push([bone as u16, 0, 0, 0]);
            mesh.weights.push([1., 0., 0., 0.]);
        }
        mesh.indices.extend(
            [
                0, 2, 1, 1, 2, 3, 4, 5, 6, 5, 7, 6, 0, 1, 4, 1, 5, 4, 2, 6, 3, 3, 6, 7, 0, 4, 2, 2,
                4, 6, 1, 3, 5, 3, 7, 5,
            ]
            .map(|v| v + base),
        );
    }
    let geometry = w.spawn(Entity {
        mesh: Some(Arc::new(mesh)),
        ..Default::default()
    });
    let material = w.spawn(Entity {
        material: Some(Material {
            shading: Shading::Pbr,
            base_color: [0.92, 0.5, 0.19, 1.],
            ..Default::default()
        }),
        ..Default::default()
    });
    let mut actor = Entity {
        name: "Motion matching mannequin".into(),
        instance: Some(Instance {
            mesh: geometry,
            material: Some(material),
            legacy_shading: None,
            outline: false,
        }),
        skin_binding: Some(skin),
        ..Default::default()
    };
    actor.components.insert(
        PLAYER.into(),
        serde_json::to_value(Player {
            bones,
            speed: 1.35,
            turn: 0.,
            sample: 0,
            fraction: 0.,
            angles: [0.; BONE_COUNT],
            clip: "Idle".into(),
            cost: 0.,
        })
        .unwrap(),
    );
    w.spawn(actor);
    w
}
