use super::{solver::*, *};
use crate::{
    rdg::{Access, Dispatch, Op, PassPhase, Resource, Shader, rows::Rows},
    render::{FrameGraph, Snapshot, SnapshotComponent},
};
use bytemuck::{Pod, Zeroable};
use std::{
    collections::BTreeMap,
    hash::{DefaultHasher, Hash, Hasher},
    sync::LazyLock,
};

pub(crate) const STRETCH: &str = "Groom peak stretch (ppm)";
type Grooms = BTreeMap<Id, (Mount, Dynamics)>;
pub(in crate::experiments) const SNAPSHOT: SnapshotComponent = SnapshotComponent {
    key: STATE,
    copy_setting: false,
    related_settings: &[],
    extract: Some(|ctx| {
        let mounts: BTreeMap<_, _> = ctx.all_typed::<Mount>(SETTING)?.into_iter().collect();
        let mut all = Grooms::new();
        for (id, state) in ctx.all_typed::<Dynamics>(STATE)? {
            all.insert(
                id,
                (*mounts.get(&id).context("missing groom mount")?, state),
            );
        }
        Ok(Some(Arc::new(all)))
    }),
};

const N: usize = GUIDES * POINTS;
static REST: LazyLock<Rows> = LazyLock::new(|| {
    let mut nodes = Vec::with_capacity(N);
    for guide in (0..GUIDES).map(groom) {
        for (j, p) in guide.rest.iter().enumerate() {
            nodes.push([
                p.extend(1.).to_array(),
                [
                    guide.length.get(j).copied().unwrap_or_default(),
                    guide.chord.get(j).copied().unwrap_or_default(),
                    0.,
                    0.,
                ],
            ]);
        }
    }
    Rows::new(&nodes)
});
static INITIAL: LazyLock<Rows> = LazyLock::new(|| {
    let points: Vec<_> = (0..GUIDES)
        .flat_map(|i| groom(i).rest.map(|p| [p.extend(1.).to_array(); 2]))
        .collect();
    Rows::new(&points)
});

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Step {
    head: [f32; 16],
    force: [f32; 4],
}

fn solve(
    g: &mut FrameGraph,
    state: &Dynamics,
    start: Option<u64>,
    rest: Resource,
    particles: Resource,
) {
    const CHUNK: u64 = 8;
    let seed = if start.is_none()
        && let Some(strands) = &state.seed
    {
        let points: Vec<_> = strands
            .iter()
            .flat_map(|s| {
                s.points
                    .iter()
                    .zip(s.previous)
                    .map(|(p, prev)| [p.extend(1.).to_array(), prev.extend(1.).to_array()])
            })
            .collect();
        g.input(
            "Saved groom particles",
            bytemuck::cast_slice(&points).to_vec(),
        )
    } else {
        g.asset_table("Groom rest particles", &INITIAL, false)
    };
    let mut at = start.unwrap_or(0);
    let mut reset = start.is_none();
    while at < state.tick || reset {
        let end = state.tick.min(at.saturating_add(CHUNK));
        let steps: Vec<_> = (at..end)
            .map(|i| {
                let (head, c, time) = state.sample(i + 1);
                Step {
                    head: head.to_cols_array(),
                    force: [c.wind, c.stiffness, time, 0.],
                }
            })
            .collect();
        let params = g.input(
            "Groom fixed step inputs",
            bytemuck::cast_slice(&steps).to_vec(),
        );
        let mut push = bytemuck::bytes_of(&[steps.len() as u32, u32::from(reset)]).to_vec();
        push.extend_from_slice(bytemuck::bytes_of(&[
            SHOULDER_X,
            SHOULDER_Y,
            CENTER.x,
            CENTER.y,
            CENTER.z,
            HEAD + MARGIN,
            SHOULDER_RADIUS + MARGIN,
            1. / HZ as f32,
        ]));
        g.pass_in(
            PassPhase::Animation,
            "Groom XPBD",
            Op::Compute {
                program: Dispatch {
                    shader: Shader::Compute(include_bytes!(concat!(
                        env!("OUT_DIR"),
                        "/hair_solve.comp.spv"
                    ))),
                    bindings: vec![
                        (rest, Access::ComputeRead),
                        (seed, Access::ComputeRead),
                        (
                            particles,
                            if reset {
                                Access::ComputeWrite
                            } else {
                                Access::ComputeReadWrite
                            },
                        ),
                        (params, Access::ComputeRead),
                    ],
                    push,
                },
                groups: [(GUIDES as u32).div_ceil(8), 1, 1],
            },
        );
        at = end;
        reset = false;
    }
}

fn palette(
    g: &mut FrameGraph,
    rest: Resource,
    particles: Resource,
    label: &str,
    metric: Resource,
    measure: bool,
) -> Resource {
    let pose = g.local_buffer(label, N * 64);
    g.pass_in(
        PassPhase::Animation,
        label,
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/hair_palette.comp.spv"
                ))),
                bindings: vec![
                    (rest, Access::ComputeRead),
                    (particles, Access::ComputeRead),
                    (pose, Access::ComputeWrite),
                    (metric, Access::ComputeReadWrite),
                ],
                push: bytemuck::bytes_of(&u32::from(measure)).to_vec(),
            },
            groups: [(N as u32).div_ceil(64), 1, 1],
        },
    );
    pose
}

pub(crate) fn prepare(g: &mut FrameGraph, s: &Snapshot) -> Result<()> {
    let Some(grooms) = s.scene.derived::<Grooms>(STATE) else {
        return Ok(());
    };
    let previous = s
        .previous_deformation
        .as_ref()
        .and_then(|p| p.derived::<Grooms>(STATE));
    let rest = g.asset_table("Groom rest guides", &REST, false);
    for (&id, (mount, state)) in grooms {
        let mut h = DefaultHasher::new();
        ("groom particles", id, state.generation).hash(&mut h);
        let particles = g.geometry_history("Groom Verlet history", h.finish(), N * 32);
        let start = previous
            .and_then(|all| all.get(&id))
            .map(|p| &p.1)
            .filter(|p| p.generation == state.generation && p.tick <= state.tick)
            .map(|p| p.tick);
        let measure = grooms
            .first_key_value()
            .is_some_and(|(first, _)| *first == id);
        let metric = g.input("Groom stretch counter", vec![0; 16]);
        let prior = if start.is_some_and(|i| i < state.tick) && s.temporal.is_some() {
            Some(palette(
                g,
                rest,
                particles,
                "Groom previous palette",
                metric,
                false,
            ))
        } else {
            None
        };
        if start != Some(state.tick) {
            solve(g, state, start, rest, particles)
        }
        let current = palette(g, rest, particles, "Groom current palette", metric, measure);
        if measure {
            g.pass(
                "Read groom stretch",
                Op::Counters {
                    buffer: metric,
                    first: 0,
                    labels: vec![STRETCH],
                },
            );
        }
        let prior = if start == Some(state.tick) {
            Some(current)
        } else {
            prior
        };
        for part in std::iter::once(mount.display).chain(mount.followers) {
            ensure!(
                g.gpu_poses.insert(part, [Some(current), prior]).is_none(),
                "display mesh has two groom owners"
            );
        }
    }
    Ok(())
}
