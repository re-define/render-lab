//! Motion matching on poses retargeted from Kimodo to the imported Silver Wolf rig.
use super::*;
mod search;
use crate::experiments::cartoon;
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
    sample: usize,
    clip: String,
    cost: f32,
    fraction: f32,
    search_in: u32,
    yaw: f32,
}
struct Pose {
    feet: [f32; 12],
    heading: f32,
    hip: Vec3,
    rotations: Vec<Quat>,
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
            });
        }
        let search = search::Index::load(raw_features, &clips)?;
        Ok(Self {
            poses,
            clips,
            search,
        })
    }
    fn step(&self, p: &mut State) -> Result<()> {
        let (span, _) = self
            .clips
            .iter()
            .find(|(r, _)| r.contains(&p.sample))
            .context("invalid saved sample")?;
        let next = (p.sample + 1).min(span.end - 1);
        let predicted = &self.poses[next];
        let delta = predicted.heading - self.poses[p.sample].heading;
        p.yaw = (p.yaw + delta.sin().atan2(delta.cos())) % std::f32::consts::TAU;
        if p.search_in > 0 && next + LOOKAHEAD < span.end {
            p.sample = next;
            p.search_in -= 1;
            return Ok(());
        }
        let (mut path, facing) = trajectory(p.speed, p.turn);
        for (i, t) in HORIZONS.iter().enumerate() {
            let side = if p.turn.abs() < 0.0001 {
                Vec2::new(*t, 0.)
            } else {
                Vec2::new((p.turn * t).sin(), (p.turn * t).cos() - 1.) / p.turn
            };
            path[i] += side * p.side;
        }
        let mut raw = [0.; 24];
        for i in 0..3 {
            raw[4 * i..4 * i + 2].copy_from_slice(&path[i].to_array());
            raw[4 * i + 2..4 * i + 4].copy_from_slice(&facing[i].to_array());
        }
        raw[12..].copy_from_slice(&predicted.feet);
        let hit = self.search.nearest(&raw, p.mode, next, span, &self.clips);
        ensure!(hit.cost.is_finite(), "motion query exceeds feature range");
        p.cost = hit.cost;
        p.sample = hit.sample;
        p.evaluated = hit.evaluated;
        let clip = self
            .clips
            .partition_point(|(range, _)| range.end <= hit.sample);
        p.clip.clone_from(&self.clips[clip].1);
        p.search_in = 4;
        Ok(())
    }
}
impl System for SilverMatcher {
    fn update(&self, w: &World, dt: f32) -> Result<Edit> {
        ensure!(dt.is_finite() && dt >= 0., "invalid motion time");
        if dt == 0. {
            return Ok(Box::new(|_| {}));
        }
        let mut updates = vec![];
        for (&id, e) in &w.entities {
            let Some(raw) = e.components.get(PLAYER) else {
                continue;
            };
            let mut p: State = serde_json::from_value(raw.clone())?;
            ensure!(
                p.sample < self.poses.len()
                    && p.bones.len() == self.poses[p.sample].rotations.len(),
                "invalid motion binding"
            );
            p.fraction += dt.min(0.25) * HZ;
            while p.fraction >= 1. {
                p.fraction -= 1.;
                self.step(&mut p)?;
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
            let blend = 1. - (-22. * dt).exp();
            let mut joints = Vec::with_capacity(p.bones.len());
            for (i, &bone) in p.bones.iter().enumerate() {
                let (scale, previous, mut translation) = w
                    .entities
                    .get(&bone)
                    .context("missing motion joint")?
                    .local
                    .to_scale_rotation_translation();
                let target = pose.rotations[i].slerp(after.rotations[i], p.fraction);
                if i == 0 {
                    translation = translation
                        .lerp(p.hip_origin + pose.hip.lerp(after.hip, p.fraction), blend);
                }
                joints.push((
                    bone,
                    Mat4::from_scale_rotation_translation(
                        scale,
                        previous.normalize().slerp(target, blend),
                        translation,
                    ),
                ));
            }
            updates.push((
                id,
                Mat4::from_rotation_y(p.yaw),
                serde_json::to_value(p)?,
                joints,
            ));
        }
        Ok(Box::new(move |w| {
            for (id, root, state, joints) in updates {
                let actor = w.entities.get_mut(&id).unwrap();
                actor.local = root;
                actor.components.insert(PLAYER.into(), state);
                for (id, m) in joints {
                    w.entities.get_mut(&id).unwrap().local = m;
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
    let raw = w
        .entities
        .values_mut()
        .find_map(|e| e.components.get_mut(PLAYER))
        .context("no Silver Wolf motion player")?;
    let mut p: State = serde_json::from_value(raw.clone())?;
    p.speed = speed;
    p.turn = turn;
    p.side = side;
    p.search_in = 0;
    *raw = serde_json::to_value(p)?;
    Ok(())
}
pub fn set_search(w: &mut World, mode: &str) -> Result<()> {
    let mode = match mode {
        "kd" => search::Mode::Kd,
        "scan" => search::Mode::Scan,
        _ => anyhow::bail!("motionsearch kd/scan"),
    };
    let raw = w
        .entities
        .values_mut()
        .find_map(|e| e.components.get_mut(PLAYER))
        .context("no Silver Wolf motion player")?;
    raw["mode"] = serde_json::to_value(mode)?;
    raw["search_in"] = 0.into();
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
        speed: 1.35,
        turn: 0.,
        side: 0.,
        sample,
        clip: name.clone(),
        cost: 0.,
        fraction: 0.,
        search_in: 0,
        yaw: 0.,
    };
    let actor = w.spawn(Entity {
        name: "Silver Wolf motion matching".into(),
        components: std::collections::BTreeMap::from([(
            PLAYER.into(),
            serde_json::to_value(state)?,
        )]),
        ..Default::default()
    });
    w.entities.get_mut(&root).unwrap().parent = Some(actor);
    w.validate()?;
    Ok(w)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut state: State =
            serde_json::from_value(world.entities[&id].components[PLAYER].clone())?;
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
                if world.entities[&id].components[PLAYER]["search_in"] == 4 {
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
            let kd = bank
                .search
                .nearest(&raw, search::Mode::Kd, next, span, &bank.clips);
            let scan = bank
                .search
                .nearest(&raw, search::Mode::Scan, next, span, &bank.clips);
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
