//! Motion matching on poses retargeted from Kimodo to the imported Silver Wolf rig.
use crate::experiments::assembly::{Primary, Registration as SceneModule};
pub(in crate::experiments) const MODULE: SceneModule = SceneModule {
    key: PLAYER,
    primary: Primary::Simulation,
    restore: Some(hydrate),
    install: |world, a| {
        if crate::experiments::composition::enabled(world, "actor") {
            a.schedule
                .add_system(std::sync::Arc::new(SilverMatcher::load()?));
        }
        Ok(())
    },
};
use super::*;
mod locomotion;
mod search;
mod trajectory_view;
use crate::experiments::cartoon;
pub(in crate::experiments) use trajectory_view::OVERLAY;
pub const PLAYER: &str = "lab.silver_wolf.motion";
const INDEX: &str = include_str!("../../../assets/motion/bank.json");
const DATA: &[u8] = include_bytes!("../../../assets/motion/poses.bin");
const LOOKAHEAD: usize = 23;

#[derive(Deserialize)]
struct Entry {
    name: String,
    frames: usize,
}
#[derive(Deserialize)]
struct Index {
    version: u32,
    fps: u32,
    bones: Vec<String>,
    clips: Vec<Entry>,
}
#[derive(Clone, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    mode: search::Mode,
    #[serde(default)]
    evaluated: usize,
    bones: Vec<Id>,
    hip_origin: Vec3,
    speed: f32,
    turn: f32,
    #[serde(default)]
    side: f32,
    #[serde(default)]
    move_root: bool,
    #[serde(default)]
    velocity: Vec2,
    #[serde(default)]
    acceleration: Vec2,
    #[serde(default)]
    yaw_rate: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transition: Option<locomotion::Transition>,
    sample: usize,
    clip: String,
    cost: f32,
    fraction: f32,
    search_in: u32,
    yaw: f32,
}
impl State {
    fn target_velocity(&self) -> Vec2 {
        Vec2::new(self.side, self.speed)
    }

    fn advance(&self, time: f32) -> locomotion::Travel {
        locomotion::travel(
            self.velocity,
            self.acceleration,
            self.yaw_rate,
            self.target_velocity(),
            self.turn,
            time,
        )
    }

    fn predict(&self, time: f32) -> (Vec2, Vec2) {
        if self.move_root {
            let travel = self.advance(time);
            return (
                travel.displacement,
                Vec2::new(travel.turn.sin(), travel.turn.cos()),
            );
        }
        let (sin, cos) = (self.turn * time).sin_cos();
        let displacement = if self.turn.abs() < 0.0001 {
            self.target_velocity() * time
        } else {
            (Vec2::new(1. - cos, sin) * self.speed + Vec2::new(sin, cos - 1.) * self.side)
                / self.turn
        };
        (displacement, Vec2::new(sin, cos))
    }
}

fn actor(world: &World) -> Option<Id> {
    world.first_raw_component(PLAYER).map(|(id, _)| id)
}

fn player(world: &World, id: Id) -> Result<std::borrow::Cow<'_, State>> {
    world
        .typed_component(id, PLAYER)?
        .context("no Silver Wolf motion player")
}

pub(in crate::experiments) fn hydrate(world: &mut World) -> Result<()> {
    world.hydrate_components::<State>(PLAYER)
}

pub struct Status {
    pub clip: String,
    pub sample: usize,
    pub cost: f32,
    pub search: &'static str,
    pub evaluated: usize,
    pub move_root: bool,
}

pub fn status(world: &World) -> Result<Option<Status>> {
    let Some(id) = actor(world) else {
        return Ok(None);
    };
    let state = player(world, id)?;
    Ok(Some(Status {
        clip: state.clip.clone(),
        sample: state.sample,
        cost: state.cost,
        search: match state.mode {
            search::Mode::Kd => "KD",
            search::Mode::Scan => "SCAN",
        },
        evaluated: state.evaluated,
        move_root: state.move_root,
    }))
}
struct Pose {
    feet: [f32; 12],
    heading: f32,
    hip: Vec3,
    rotations: Vec<Quat>,
    ground_velocity: Vec2,
}
pub struct SilverMatcher {
    search: search::Index,
    poses: Vec<Pose>,
    clips: Vec<(std::ops::Range<usize>, String)>,
}
impl SilverMatcher {
    pub fn load() -> Result<Self> {
        let index: Index = serde_json::from_str(INDEX)?;
        ensure!(
            index.version == 1 && index.fps == 30 && !index.bones.is_empty(),
            "unknown motion bank format"
        );
        let width = 28 + 4 * index.bones.len();
        let mut clips = vec![];
        let mut offset: usize = 0;
        for clip in index.clips {
            ensure!(clip.frames > LOOKAHEAD, "motion clip too short");
            let end = offset
                .checked_add(clip.frames)
                .context("motion frame overflow")?;
            clips.push((offset..end, clip.name));
            offset = end;
        }
        ensure!(
            offset > 0 && Some(DATA.len()) == offset.checked_mul(width * 4),
            "invalid motion bank length"
        );
        let mut poses = Vec::with_capacity(offset);
        let mut raw_features = Vec::with_capacity(offset);
        for bytes in DATA.chunks_exact(width * 4) {
            let f: Vec<_> = bytes
                .chunks_exact(4)
                .map(|x| f32::from_le_bytes(x.try_into().unwrap()))
                .collect();
            ensure!(f.iter().all(|v| v.is_finite()), "invalid motion sample");
            let rotations = f[28..]
                .chunks_exact(4)
                .map(|q| Quat::from_array(q.try_into().unwrap()))
                .collect::<Vec<_>>();
            ensure!(
                rotations.iter().all(|q| q.is_normalized()),
                "invalid motion rotation"
            );
            raw_features.push(f[..24].try_into()?);
            poses.push(Pose {
                feet: f[12..24].try_into()?,
                heading: f[24],
                hip: Vec3::from_slice(&f[25..28]),
                rotations,
                ground_velocity: Vec2::from_slice(&f[..2]) / HORIZONS[0],
            });
        }
        let search = search::Index::load(raw_features, &clips)?;
        Ok(Self {
            poses,
            clips,
            search,
        })
    }
    fn step(&self, p: &mut State) -> Result<Option<(usize, usize)>> {
        let outgoing = p.sample;
        let (span, current_clip) = self
            .clips
            .iter()
            .find(|(r, _)| r.contains(&p.sample))
            .context("invalid saved sample")?;
        let next = (p.sample + 1).min(span.end - 1);
        let predicted = &self.poses[next];
        if !p.move_root {
            let delta = predicted.heading - self.poses[p.sample].heading;
            p.yaw = (p.yaw + delta.sin().atan2(delta.cos())) % std::f32::consts::TAU;
        }
        // Once the controller settles, keep start/stop clips out of the steady
        // loop. During acceleration, braking and changes of direction all poses
        // remain available.
        let target = p.target_velocity();
        let class = if target == Vec2::ZERO {
            (!p.move_root || p.velocity.length_squared() < 0.08f32.powi(2))
                .then_some(search::Candidate::Idle)
        } else {
            (p.move_root && (p.velocity - target).length() < target.length() * 0.26)
                .then_some(search::Candidate::Steady)
        };
        if p.search_in > 0
            && next + LOOKAHEAD < span.end
            && class.is_none_or(|candidate| candidate == search::Candidate::of(current_clip))
        {
            p.sample = next;
            p.search_in -= 1;
            return Ok(None);
        }
        let mut raw = [0.; 24];
        for (i, time) in HORIZONS.into_iter().enumerate() {
            let (path, facing) = p.predict(time);
            raw[4 * i..4 * i + 2].copy_from_slice(&path.to_array());
            raw[4 * i + 2..4 * i + 4].copy_from_slice(&facing.to_array());
        }
        raw[12..].copy_from_slice(&predicted.feet);
        let hit = self
            .search
            .nearest(&raw, p.mode, next, span, &self.clips, class);
        ensure!(hit.cost.is_finite(), "motion query exceeds feature range");
        p.cost = hit.cost;
        p.sample = hit.sample;
        p.evaluated = hit.evaluated;
        let clip = self
            .clips
            .partition_point(|(range, _)| range.end <= hit.sample);
        p.clip.clone_from(&self.clips[clip].1);
        let jumped = hit.sample != next;
        p.search_in = if jumped { 7 } else { 4 };
        Ok(jumped.then_some((outgoing, next)))
    }
}
impl System for SilverMatcher {
    fn access(&self) -> SystemAccess {
        use SimulationData::{Component, LocalTransform};
        SystemAccess {
            name: "Silver Wolf motion matcher",
            reads: &[Component(PLAYER), LocalTransform],
            writes: &[Component(PLAYER), LocalTransform],
        }
    }
    fn update(&self, w: &World, dt: f32) -> Result<Edit> {
        ensure!(dt.is_finite() && dt >= 0., "invalid motion time");
        if dt == 0. {
            return Ok(Box::new(|_| {}));
        }
        let mut updates = vec![];
        for &id in w.entities.component_owners(PLAYER) {
            let mut p = player(w, id)?.into_owned();
            ensure!(
                p.sample < self.poses.len()
                    && p.bones.len() == self.poses[p.sample].rotations.len(),
                "invalid motion binding"
            );
            let time = dt.min(0.25);
            let mut root = w.entities[&id].local.w_axis.truncate();
            if p.move_root {
                let travel = p.advance(time);
                let delta = travel.displacement;
                root += Quat::from_rotation_y(p.yaw) * Vec3::new(delta.x, 0., delta.y);
                p.velocity = travel.velocity;
                p.acceleration = travel.acceleration;
                p.yaw = (p.yaw + travel.turn).rem_euclid(std::f32::consts::TAU);
                p.yaw_rate = travel.yaw_rate;
            }
            let mut playback = 1.;
            if p.move_root {
                let motion = self.poses[p.sample].ground_velocity;
                let speed = motion.length();
                if speed > 0.25 && p.velocity.dot(motion) > 0. {
                    // Adapt cadence within a small range while the controller
                    // and baked clip are moving in the same direction.
                    playback = (p.velocity.dot(motion) / speed.powi(2)).clamp(0.85, 1.15);
                }
            }
            p.fraction += time * HZ * playback;
            let mut jump = None;
            while p.fraction >= 1. {
                p.fraction -= 1.;
                jump = self.step(&mut p)?.or(jump);
            }
            let pose = &self.poses[p.sample];
            let end = self
                .clips
                .iter()
                .find(|(r, _)| r.contains(&p.sample))
                .unwrap()
                .0
                .end;
            let after = &self.poses[(p.sample + 1).min(end - 1)];
            let mut transition = jump.map(|(old, next)| {
                let old = &self.poses[old];
                let old_next = &self.poses[next];
                (
                    old,
                    old_next,
                    locomotion::Transition {
                        elapsed: 0.,
                        hip: locomotion::Offset {
                            value: Vec3::ZERO,
                            velocity: ((old_next.hip - old.hip) - (after.hip - pose.hip)) * HZ,
                        },
                        joints: Vec::with_capacity(p.bones.len()),
                    },
                )
            });
            if let Some(active) = &mut p.transition {
                active.elapsed += time;
                if active.elapsed >= 0.4 || active.joints.len() != p.bones.len() {
                    p.transition = None;
                }
            }
            let target_hip = p.hip_origin + pose.hip.lerp(after.hip, p.fraction);
            let mut joints = Vec::with_capacity(p.bones.len() + 1);
            joints.push((
                id,
                Mat4::from_rotation_translation(Quat::from_rotation_y(p.yaw), root),
            ));
            for (i, &bone) in p.bones.iter().enumerate() {
                let (scale, previous, mut translation) = w
                    .entities
                    .get(&bone)
                    .context("missing motion joint")?
                    .local
                    .to_scale_rotation_translation();
                let target = pose.rotations[i].slerp(after.rotations[i], p.fraction);
                let rotation = if let Some((old, next, blend)) = &mut transition {
                    blend.joints.push(locomotion::Offset {
                        value: locomotion::rotation_difference(target, previous),
                        velocity: (locomotion::rotation_difference(
                            old.rotations[i],
                            next.rotations[i],
                        ) - locomotion::rotation_difference(
                            pose.rotations[i],
                            after.rotations[i],
                        )) * HZ,
                    });
                    previous
                } else if let Some(blend) = &p.transition {
                    Quat::from_scaled_axis(blend.joints[i].at(blend.elapsed)) * target
                } else {
                    target
                };
                if i == 0 {
                    if let Some((_, _, blend)) = &mut transition {
                        blend.hip.value = translation - target_hip;
                    } else {
                        translation = target_hip
                            + p.transition
                                .as_ref()
                                .map_or(Vec3::ZERO, |blend| blend.hip.at(blend.elapsed));
                    }
                }
                joints.push((
                    bone,
                    Mat4::from_scale_rotation_translation(scale, rotation.normalize(), translation),
                ));
            }
            if let Some((_, _, blend)) = transition {
                p.transition = Some(blend);
            }
            updates.push((id, p, LocalPose::prepare(w, joints)?));
        }
        Ok(Box::new(move |w| {
            for (id, state, pose) in updates {
                pose.apply(w);
                if w.entities.contains_key(&id) {
                    w.set_typed_component(id, PLAYER, state);
                }
            }
        }))
    }
}
pub fn set_goal(w: &mut World, speed: f32, turn: f32, side: f32) -> Result<()> {
    ensure!(
        [speed, turn, side].iter().all(|n| n.is_finite()),
        "invalid motion query"
    );
    let id = actor(w).context("no Silver Wolf motion player")?;
    let mut p = player(w, id)?.into_owned();
    p.speed = speed;
    p.turn = turn;
    p.side = side;
    p.move_root = false;
    p.velocity = Vec2::ZERO;
    p.acceleration = Vec2::ZERO;
    p.yaw_rate = 0.;
    p.search_in = 0;
    w.set_typed_component(id, PLAYER, p);
    Ok(())
}
pub fn set_keyboard_goal(w: &mut World, side: i8, forward: i8) -> Result<()> {
    let id = actor(w).context("no Silver Wolf motion player")?;
    let mut p = player(w, id)?.into_owned();
    if forward > 0 && side != 0 {
        p.speed = 1.35;
        p.side = 0.;
        p.turn = f32::from(side) * 1.35;
    } else {
        let direction = Vec2::new(f32::from(side), f32::from(forward)).clamp_length_max(1.);
        p.side = direction.x * 1.2;
        p.speed = direction.y * if direction.y < 0. { 0.9 } else { 1.35 };
        p.turn = 0.;
    }
    p.move_root = true;
    p.search_in = 0;
    w.set_typed_component(id, PLAYER, p);
    Ok(())
}
pub fn set_search(w: &mut World, mode: &str) -> Result<()> {
    let mode = match mode {
        "kd" => search::Mode::Kd,
        "scan" => search::Mode::Scan,
        _ => anyhow::bail!("motionsearch kd/scan"),
    };
    let id = actor(w).context("no Silver Wolf motion player")?;
    let mut p = player(w, id)?.into_owned();
    p.mode = mode;
    p.search_in = 0;
    w.set_typed_component(id, PLAYER, p);
    Ok(())
}
pub fn scene() -> Result<World> {
    let mut w = cartoon::scene()?;
    let index: Index = serde_json::from_str(INDEX)?;
    let bones = index
        .bones
        .iter()
        .map(|name| {
            w.entities
                .iter()
                .find_map(|(&id, e)| (&e.name == name).then_some(id))
                .with_context(|| format!("missing Silver Wolf joint {name}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut root = bones[0];
    while let Some(parent) = w.entities[&root].parent {
        root = parent;
    }
    let bank = SilverMatcher::load()?;
    let (range, name) = bank
        .clips
        .iter()
        .find(|(_, name)| name == "idle_41")
        .context("missing idle pose")?;
    let sample = range.start + 10;
    let pose = &bank.poses[sample];
    let hip_origin = w.entities[&bones[0]].local.w_axis.truncate();
    for (i, &id) in bones.iter().enumerate() {
        let e = w.entities.get_mut(&id).unwrap();
        let (scale, _, mut t) = e.local.to_scale_rotation_translation();
        if i == 0 {
            t += pose.hip;
        }
        e.local = Mat4::from_scale_rotation_translation(scale, pose.rotations[i], t);
    }
    let state = State {
        mode: Default::default(),
        evaluated: 0,
        bones,
        hip_origin,
        speed: 0.,
        turn: 0.,
        side: 0.,
        move_root: true,
        velocity: Vec2::ZERO,
        acceleration: Vec2::ZERO,
        yaw_rate: 0.,
        transition: None,
        sample,
        clip: name.clone(),
        cost: 0.,
        fraction: 0.,
        search_in: 0,
        yaw: 0.,
    };
    let actor = w.spawn(Entity {
        name: "Silver Wolf motion matching".into(),
        ..Default::default()
    });
    w.set_typed_component(actor, PLAYER, state);
    w.entities.get_mut(&root).unwrap().parent = Some(actor);
    w.validate()?;
    Ok(w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_pose_keeps_the_support_foot_at_ground_level() -> Result<()> {
        let world = scene()?;
        let transforms = world.transforms()?;
        let support_height = world
            .entities
            .iter()
            .filter(|(_, entity)| {
                entity.name.starts_with("Left toe_") || entity.name.starts_with("Right toe_")
            })
            .map(|(id, _)| transforms[id].w_axis.y)
            .fold(f32::INFINITY, f32::min);
        assert!((-0.02..0.08).contains(&support_height), "{support_height}");
        Ok(())
    }

    #[test]
    fn keyboard_prediction_matches_root_motion_and_stops_on_release() -> Result<()> {
        let bank = SilverMatcher::load()?;
        let initial = scene()?;
        let id = *initial
            .entities
            .iter()
            .find(|(_, e)| e.components.contains_key(PLAYER))
            .unwrap()
            .0;
        for (side, forward, expected) in [
            (0, 1, "walk"),
            (0, -1, "backward"),
            (-1, 0, "strafe-left"),
            (1, 0, "strafe-right"),
        ] {
            let mut world = initial.clone();
            set_keyboard_goal(&mut world, side, forward)?;
            let state = player(&world, id)?.into_owned();
            let endpoint = state.predict(HORIZONS[2]).0;
            for _ in 0..LOOKAHEAD {
                bank.update(&world, 1. / HZ)?(&mut world);
            }
            let root = world.entities[&id].local.w_axis.truncate();
            assert!(root.abs_diff_eq(Vec3::new(endpoint.x, 0., endpoint.y), 1e-5));
            let mut matches = 0;
            for _ in 0..60 {
                bank.update(&world, 1. / HZ)?(&mut world);
                matches +=
                    usize::from(player(&world, id)?.clip.split('_').next() == Some(expected));
            }
            assert!(matches >= 30, "{expected} selected for {matches}/60 frames");

            set_keyboard_goal(&mut world, 0, 0)?;
            for _ in 0..30 {
                bank.update(&world, 1. / HZ)?(&mut world);
            }
            let stopped = world.entities[&id].local.w_axis.truncate();
            bank.update(&world, 1. / HZ)?(&mut world);
            assert!(
                world.entities[&id]
                    .local
                    .w_axis
                    .truncate()
                    .abs_diff_eq(stopped, 1e-4)
            );
            let clip = player(&world, id)?.clip.clone();
            assert!(clip.starts_with("idle"), "{expected} settled in {clip}");
        }
        Ok(())
    }

    #[test]
    #[ignore = "release timing with the Silver Wolf asset"]
    fn benchmark_silver_motion() -> Result<()> {
        use std::time::Instant;

        let bank = SilverMatcher::load()?;
        let mut world = scene()?;
        let id = *world
            .entities
            .iter()
            .find(|(_, e)| e.components.contains_key(PLAYER))
            .unwrap()
            .0;
        let mut state = player(&world, id)?.into_owned();
        let goals = [
            (0., 0., 0.),
            (1.35, 0., 0.),
            (3., 0., 0.),
            (1.35, -1., 0.),
            (0., 0., 1.2),
            (-0.9, 0., 0.),
        ];
        let mut kd_queries = Vec::new();
        let mut scan_queries = Vec::new();
        let mut kd_evaluated = Vec::new();
        let mut scan_evaluated = Vec::new();
        let mut updates_with_search = Vec::new();
        let mut updates_between_searches = Vec::new();
        for i in 0..2400 {
            let (speed, turn, side) = goals[i / 60 % goals.len()];
            (state.speed, state.turn, state.side, state.search_in) = (speed, turn, side, 0);
            let mut reference = state.clone();
            reference.mode = search::Mode::Scan;
            let measure = |player: &mut State| -> Result<_> {
                let start = Instant::now();
                bank.step(std::hint::black_box(player))?;
                Ok(start.elapsed())
            };
            let (kd_time, scan_time) = if i % 2 == 0 {
                (measure(&mut state)?, measure(&mut reference)?)
            } else {
                let scan_time = measure(&mut reference)?;
                (measure(&mut state)?, scan_time)
            };
            assert_eq!(
                (state.sample, state.cost),
                (reference.sample, reference.cost)
            );
            if i % 60 == 0 {
                set_goal(&mut world, speed, turn, side)?;
            }
            let start = Instant::now();
            bank.update(&world, 1. / HZ)?(&mut world);
            let elapsed = start.elapsed();
            if i >= 240 {
                kd_queries.push(kd_time);
                scan_queries.push(scan_time);
                kd_evaluated.push(state.evaluated);
                scan_evaluated.push(reference.evaluated);
                if player(&world, id)?.search_in == 4 {
                    updates_with_search.push(elapsed);
                } else {
                    updates_between_searches.push(elapsed);
                }
            }
        }
        for (label, mut samples) in [
            ("kd_query", kd_queries),
            ("scan_query", scan_queries),
            ("update_with_search", updates_with_search),
            ("update_between_searches", updates_between_searches),
        ] {
            samples.sort_unstable();
            let n = samples.len();
            let us = |percent: usize| {
                samples[((n - 1) * percent + 50usize) / 100].as_secs_f64() * 1_000_000.
            };
            println!(
                "{label}: n={n} median_us={:.2} p95_us={:.2}",
                us(50),
                us(95)
            );
        }
        for (method, mut counts) in [("kd", kd_evaluated), ("scan", scan_evaluated)] {
            counts.sort_unstable();
            println!(
                "{method}_evaluated: median={} p95={} max={}",
                counts[counts.len() / 2],
                counts[counts.len() * 95 / 100],
                counts.last().unwrap()
            );
        }
        Ok(())
    }

    #[test]
    fn tree_agrees_with_scan_for_perturbed_poses_and_clip_boundaries() -> Result<()> {
        let bank = SilverMatcher::load()?;
        let stride = DATA.len() / bank.poses.len();
        let mut random = 9173u32;
        let mut draw = || {
            random = random.wrapping_mul(1664525).wrapping_add(1013904223);
            random
        };
        for i in 0..512 {
            let (span, _) = &bank.clips[draw() as usize % bank.clips.len()];
            let source = draw() as usize % bank.poses.len();
            let magnitude = [0., 0.015, 0.25, 2.0][i % 4];
            let raw = std::array::from_fn(|k| {
                let at = source * stride + 4 * k;
                f32::from_le_bytes(DATA[at..at + 4].try_into().unwrap())
                    + magnitude * (draw() as f32 / u32::MAX as f32 - 0.5)
            });
            let next = match i % 3 {
                0 => span.start,
                1 => span.end - 1,
                _ => span.start + (draw() as usize % span.len()),
            };
            let class = match i % 4 {
                0 => Some(search::Candidate::Idle),
                1 => Some(search::Candidate::Steady),
                _ => None,
            };
            let kd = bank
                .search
                .nearest(&raw, search::Mode::Kd, next, span, &bank.clips, class);
            let scan =
                bank.search
                    .nearest(&raw, search::Mode::Scan, next, span, &bank.clips, class);
            assert_eq!((kd.sample, kd.cost), (scan.sample, scan.cost), "query {i}");
        }
        Ok(())
    }

    #[test]
    fn kimodo_search_tracks_direction_speed_and_strafe() -> Result<()> {
        let index: Index = serde_json::from_str(INDEX)?;
        let bank = SilverMatcher::load()?;
        assert_eq!(index.bones.len(), 21);
        assert_eq!(bank.clips.len(), 39);
        assert_eq!(bank.poses.len(), 4680);
        let mut p = State {
            mode: Default::default(),
            evaluated: 0,
            bones: vec![],
            hip_origin: Vec3::ZERO,
            speed: 0.,
            turn: 0.,
            side: 0.,
            move_root: false,
            velocity: Vec2::ZERO,
            acceleration: Vec2::ZERO,
            yaw_rate: 0.,
            transition: None,
            sample: bank
                .clips
                .iter()
                .find(|(_, name)| name == "idle_41")
                .unwrap()
                .0
                .start
                + 10,
            clip: String::new(),
            cost: 0.,
            fraction: 0.,
            search_in: 0,
            yaw: 0.,
        };
        for (speed, turn, side, expected) in [
            (0., 0., 0., "idle"),
            (1.35, 0., 0., "walk"),
            (3., 0., 0., "run"),
            (1.35, -1., 0., "walk-left"),
            (0., 0., 1.2, "strafe-right"),
            (-0.9, 0., 0., "backward"),
        ] {
            (p.speed, p.turn, p.side, p.search_in) = (speed, turn, side, 0);
            let mut matches = 0;
            for _ in 0..60 {
                bank.step(&mut p)?;
                let (range, _) = bank.clips.iter().find(|(_, name)| name == &p.clip).unwrap();
                assert!(range.contains(&p.sample) && p.sample + LOOKAHEAD < range.end);
                assert!(p.cost.is_finite());
                if p.clip.split('_').next() == Some(expected) {
                    matches += 1;
                }
            }
            assert!(matches >= 30, "{expected} selected for {matches}/60 frames");
        }
        Ok(())
    }
}
