//! Bind shared graph assets to Silver Wolf's motion bank. Playback belongs to each actor.
use super::*;
use crate::animation::{PoseGraph, PoseProgram, RotationLayer};
use std::sync::LazyLock;

pub(super) const ASSET: &str = "lab.animation.pose_graph.asset";
static DEFAULT: LazyLock<PoseGraph> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../../../assets/motion/pose-graphs/default.json"
    ))
    .expect("bundled default pose graph")
});
static UPPER: LazyLock<PoseGraph> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../../../assets/motion/pose-graphs/upper-body.json"
    ))
    .expect("bundled upper-body pose graph")
});
pub(super) fn at_start(t: &f64) -> bool {
    *t == 0.
}

pub(super) struct Programs {
    default: Arc<PoseProgram>,
    assets: BTreeMap<Id, Arc<PoseProgram>>,
    version: Option<(Arc<()>, u64, u64)>,
}
impl Programs {
    pub fn new(bank: &SilverMatcher) -> Result<Self> {
        Ok(Self {
            default: Arc::new(DEFAULT.compile(&bank.channels, &bank.clips)?),
            assets: Default::default(),
            version: None,
        })
    }
    pub fn refresh(&mut self, w: &World) {
        let generation = w.binding_version(ASSET);
        let topology = w.entities.hierarchy_revision();
        if self.version.as_ref().is_none_or(|(src, v, h)| {
            *v != generation || *h != topology || !Arc::ptr_eq(src, w.entities.hierarchy_source())
        }) {
            self.assets.clear();
            self.version = Some((w.entities.hierarchy_source().clone(), generation, topology));
        }
    }
    pub fn resolve(
        &mut self,
        w: &World,
        id: Option<Id>,
        bank: &SilverMatcher,
    ) -> Result<Arc<PoseProgram>> {
        let Some(id) = id else {
            return Ok(self.default.clone());
        };
        if let Some(p) = self.assets.get(&id) {
            return Ok(p.clone());
        }
        let graph = w
            .typed_component::<PoseGraph>(id, ASSET)?
            .with_context(|| format!("missing pose graph asset {id}"))?;
        let p = Arc::new(
            graph
                .compile(&bank.channels, &bank.clips)
                .with_context(|| format!("bind pose graph asset {id}"))?,
        );
        self.assets.insert(id, p.clone());
        Ok(p)
    }
}

#[derive(Default)]
struct SampledFrame {
    key: Option<(usize, usize, u32)>,
    rotations: Vec<Option<Quat>>,
}

/// Two recent frame pairs let a job reuse both clip inputs of overlapping layers.
#[derive(Default)]
pub(super) struct SampleCache {
    frames: [SampledFrame; 2],
    recent: usize,
}
impl SampleCache {
    fn channels(&mut self, key: (usize, usize, u32), count: usize) -> &mut [Option<Quat>] {
        if self.frames[self.recent].key != Some(key) {
            self.recent ^= 1;
            let entry = &mut self.frames[self.recent];
            if entry.key != Some(key) {
                entry.key = Some(key);
                entry.rotations.clear();
                entry.rotations.resize(count, None);
            }
        }
        &mut self.frames[self.recent].rotations
    }
}

#[derive(Default)]
pub(super) struct Scratch {
    pub pose: rig::PoseBuffers,
    pub clips: SampleCache,
}

pub(super) fn blend(
    layers: &[RotationLayer],
    affected: &[usize],
    bank: &SilverMatcher,
    s: &Stage,
    pose: &mut rig::Pose<'_>,
    seconds: f64,
    cache: &mut SampleCache,
) {
    let map = s.binding.rig.as_deref().unwrap_or(&profile::DEFAULT.rig);
    let mut changed = false;
    for layer in layers {
        let envelope = layer.envelope(seconds);
        if envelope == 0. {
            continue;
        }
        changed = true;
        let phase = (seconds * f64::from(HZ) * f64::from(layer.speed))
            .rem_euclid(layer.frames.len() as f64);
        let i = phase.floor() as usize;
        let a = layer.frames.start + i;
        let b = layer.frames.start + (i + 1) % layer.frames.len();
        let fraction = phase.fract() as f32;
        let sampled = cache.channels((a, b, fraction.to_bits()), bank.channels.len());
        for &(channel, weight) in &layer.channels {
            let bone = s.rig.corrected[s.rig.source_slots[channel]];
            let (scale, base, offset) = pose.local[bone].to_scale_rotation_translation();
            let q = *sampled[channel].get_or_insert_with(|| {
                bank.poses[a].rotations[channel].slerp(bank.poses[b].rotations[channel], fraction)
            });
            let rotation = base
                .slerp(map.rotation(channel, q), weight * envelope)
                .normalize();
            pose.local[bone] = Mat4::from_scale_rotation_translation(scale, rotation, offset);
        }
    }
    if changed {
        pose.refresh(affected);
    }
}

pub(super) fn command(w: &mut World, id: Id, arg: &str) -> Result<String> {
    let mut c = w
        .typed_component::<Contact>(id, SETTING)?
        .context("missing contact controller")?
        .into_owned();
    if arg.is_empty() {
        let graph = c
            .graph
            .map(|asset| {
                w.typed_component::<PoseGraph>(asset, ASSET)?
                    .context("missing pose graph asset")
                    .map(|g| g.into_owned())
            })
            .transpose()?;
        let graph = graph.as_ref().unwrap_or(&DEFAULT);
        return Ok(format!(
            "Actor {id} · graph {} · {} nodes · output {} · {:.3} s",
            c.graph
                .map_or_else(|| "default".into(), |id| id.to_string()),
            graph.nodes.len(),
            graph.output,
            c.graph_seconds
        ));
    }
    let next = match arg {
        "default" => None,
        other => {
            let graph = if other == "upper" {
                UPPER.clone()
            } else {
                let file = other
                    .strip_prefix("load ")
                    .context("contact graph default | upper | load FILE.json")?
                    .trim()
                    .trim_matches('"');
                serde_json::from_slice(
                    &std::fs::read(file).with_context(|| format!("read pose graph {file}"))?,
                )?
            };
            // Validate before creating a world asset or changing any controller state.
            let index: Index = serde_json::from_str(INDEX)?;
            graph.compile(&index.bones, &index.clip_ranges()?)?;
            if graph == *DEFAULT {
                None
            } else {
                let mut found = None;
                for &asset in w.entities.component_owners(ASSET) {
                    if w.typed_component::<PoseGraph>(asset, ASSET)?
                        .is_some_and(|g| *g == graph)
                    {
                        found = Some(asset);
                        break;
                    }
                }
                Some(found.unwrap_or_else(|| {
                    let asset = w.spawn(crate::scene::Entity {
                        name: "Character pose graph".into(),
                        ..Default::default()
                    });
                    w.set_typed_component(asset, ASSET, graph);
                    asset
                }))
            }
        }
    };
    if c.graph != next {
        c.graph = next;
        c.graph_seconds = 0.;
        w.set_typed_component(id, SETTING, c);
    }
    Ok(format!(
        "Actor {id} · pose graph {}",
        next.map_or_else(|| "default".into(), |id| id.to_string())
    ))
}

#[cfg(test)]
mod tests;
