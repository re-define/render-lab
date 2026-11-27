//! Motion-bank manifest, binary pose tracks and searchable clip ranges.
use super::*;

pub(super) const INDEX: &str = include_str!("../../../../assets/motion/bank.json");
pub(super) const DATA: &[u8] = include_bytes!("../../../../assets/motion/poses.bin");
pub(super) const LOOKAHEAD: usize = 23;

#[derive(Deserialize)]
struct Entry {
    name: String,
    frames: usize,
}
#[derive(Deserialize)]
pub(super) struct Index {
    version: u32,
    fps: u32,
    pub(super) bones: Vec<String>,
    clips: Vec<Entry>,
}
type ClipRanges = Vec<(std::ops::Range<usize>, String)>;
impl Index {
    pub(super) fn clip_ranges(&self) -> Result<ClipRanges> {
        let mut clips = vec![];
        let mut offset: usize = 0;
        for clip in &self.clips {
            ensure!(clip.frames > LOOKAHEAD, "motion clip too short");
            let end = offset
                .checked_add(clip.frames)
                .context("motion frame overflow")?;
            clips.push((offset..end, clip.name.clone()));
            offset = end;
        }
        Ok(clips)
    }
}
pub(super) struct Pose {
    pub(super) feet: [f32; 12],
    pub(super) heading: f32,
    pub(super) hip: Vec3,
    pub(super) rotations: Vec<Quat>,
    pub(super) ground_velocity: Vec2,
    pub(super) root_step: Vec2,
    pub(super) turn_step: f32,
}
impl SilverMatcher {
    pub fn load() -> Result<Self> {
        let index: Index = serde_json::from_str(INDEX)?;
        ensure!(
            index.version == 1 && index.fps == 30 && !index.bones.is_empty(),
            "unknown motion bank format"
        );
        let width = 28 + 4 * index.bones.len();
        let clips = index.clip_ranges()?;
        let offset = clips.last().map_or(0, |(r, _)| r.end);
        ensure!(
            offset > 0 && Some(DATA.len()) == offset.checked_mul(width * 4),
            "invalid motion bank length"
        );
        let mut poses = Vec::with_capacity(offset);
        let mut raw_features: Vec<search::Feature> = Vec::with_capacity(offset);
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
                root_step: Vec2::ZERO,
                turn_step: 0.,
            });
        }
        // The first baked trajectory feature points six samples ahead (clamped at clip end).
        // Recover the root path up to a constant translation, then take exact one-frame deltas.
        for (range, _) in &clips {
            let mut positions = vec![Vec2::ZERO; range.len()];
            for i in (0..range.len() - 1).rev() {
                let row = range.start + i;
                let heading = poses[row].heading;
                let rot = glam::Mat2::from_angle(-heading);
                positions[i] = positions[(i + 6).min(range.len() - 1)]
                    - rot * Vec2::from_slice(&raw_features[row][..2]);
                poses[row].root_step = rot.transpose() * (positions[i + 1] - positions[i]);
                let angle = poses[row + 1].heading - heading;
                poses[row].turn_step = angle.sin().atan2(angle.cos());
            }
        }
        let search = search::Index::load(raw_features, &clips)?;
        Ok(Self {
            channels: index.bones,
            poses,
            clips,
            search,
        })
    }
}
