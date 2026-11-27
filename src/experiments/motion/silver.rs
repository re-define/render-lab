//! Motion matching on poses retargeted from Kimodo to the imported Silver Wolf rig.
use crate::experiments::assembly::{Primary, Registration as SceneModule};
pub(in crate::experiments) const MODULE: SceneModule = SceneModule {
    root_writers: &[
        crate::scene::RootWriter::new(PLAYER, crate::scene::MotionPolicy::KINEMATIC)
            .delegates_to(contact::SETTING),
    ],
    key: PLAYER,
    primary: Primary::Simulation,
    restore: Some(hydrate),
    install: |world, a| {
        if !crate::experiments::composition::enabled(world, "actor") {
            return Ok(());
        }
        for &id in world.entities.component_owners(PLAYER) {
            if !world.entities[&id]
                .components
                .contains_key(contact::SETTING)
            {
                ensure!(
                    world.motion_policy(id, MotionPolicy::KINEMATIC)?.physics
                        != PhysicsParticipation::Simulated,
                    "Silver Wolf entity {id} requires a physics solver for simulated root motion; attach its contact body"
                );
            }
        }
        a.movement.register(MOVEMENT)?;
        install(world, &mut a.schedule)
    },
};
use super::*;
use crate::movement::{MovementDriver, RootMotionRequest};
use crate::scene::{MOTION_POLICY, Mobility, MotionPolicy, PhysicsParticipation};
mod bank;
pub mod contact;
mod playback;
mod prediction;
mod setup;
use bank::{DATA, INDEX, Index, LOOKAHEAD, Pose};
pub use setup::scene;
mod locomotion;
mod profile;
mod search;
mod trajectory_view;
use crate::experiments::cartoon;
pub(in crate::experiments) use trajectory_view::OVERLAY;
pub(super) const MOVEMENT: MovementDriver = MovementDriver {
    component: PLAYER,
    camera_follows: |world, id| {
        !world.entities[&id]
            .components
            .contains_key(contact::SETTING)
    },
    active: |world, id| Ok(player(world, id)?.move_root),
    apply: |world, id, intent| set_analog_goal(world, id, intent.side, intent.forward),
};
pub(crate) fn cosmetic_pose(w: &World, actor: Id) -> bool {
    w.entities.get(&actor).is_some_and(|e| {
        e.components.contains_key(PLAYER) && !e.components.contains_key(contact::SETTING)
    }) && w
        .motion_policy(actor, MotionPolicy::KINEMATIC)
        .is_ok_and(|p| p.physics == PhysicsParticipation::Off)
}
pub const PLAYER: &str = "lab.silver_wolf.motion";
fn install(world: &World, schedule: &mut crate::engine::Schedule) -> Result<()> {
    let bank = Arc::new(SilverMatcher::load()?);
    schedule.add_frame_system("silver-motion", bank.clone())?;
    contact::register(world, schedule, bank)
}

/// Component-owner indexes are sorted. The two common homogeneous scenes need no allocation.
fn standalone_actors<'a>(players: &'a [Id], contacts: &[Id]) -> std::borrow::Cow<'a, [Id]> {
    if contacts.is_empty() {
        return players.into();
    }
    if players == contacts {
        return (&[][..]).into();
    }
    players
        .iter()
        .copied()
        .filter(|id| contacts.binary_search(id).is_err())
        .collect()
}

#[derive(Clone, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    mode: search::Mode,
    #[serde(default)]
    evaluated: usize,
    bones: Vec<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rig: Option<Arc<profile::RigMap>>,
    #[serde(default)]
    root_source: profile::RootSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed: Option<locomotion::Observed>,
    #[serde(skip)]
    layout: Option<crate::scene::PoseLayout>,
    /// Actual parent-space velocity to hand off if contact dynamics takes the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    standalone_velocity: Option<Vec3>,
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
fn actor(world: &World) -> Option<Id> {
    world.first_raw_component(PLAYER).map(|(id, _)| id)
}

fn player(world: &World, id: Id) -> Result<std::borrow::Cow<'_, State>> {
    world
        .typed_component(id, PLAYER)?
        .context("no Silver Wolf motion player")
}

pub(in crate::experiments) fn hydrate(world: &mut World) -> Result<()> {
    world.hydrate_components::<State>(PLAYER)?;
    for &actor in world.entities.component_owners(PLAYER) {
        let state = player(world, actor)?;
        state
            .rig
            .as_deref()
            .unwrap_or(&profile::DEFAULT.rig)
            .validate(state.bones.len())?;
    }
    Ok(())
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
    status_for(world, actor(world))
}

pub fn status_for(world: &World, id: Option<Id>) -> Result<Option<Status>> {
    let Some(id) = id else {
        return Ok(None);
    };
    let Some(state) = world.typed_component::<State>(id, PLAYER)? else {
        return Ok(None);
    };
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
pub struct SilverMatcher {
    channels: Vec<String>,
    search: search::Index,
    poses: Vec<Pose>,
    clips: Vec<(std::ops::Range<usize>, String)>,
}
impl SimulationSystem for SilverMatcher {
    fn access(&self) -> SystemAccess {
        use SimulationData::{Component, LocalTransformsById};
        SystemAccess {
            name: "Silver Wolf motion matcher",
            reads: &[
                Component(PLAYER),
                Component(contact::SETTING),
                Component(crate::movement::character::RESOLVED),
                Component(MOTION_POLICY),
                LocalTransformsById,
            ],
            writes: &[Component(PLAYER), LocalTransformsById],
        }
    }
    fn transform_targets(&self, bindings: &TransformBindings<'_>) -> Result<TransformTargets> {
        let mut joints = Vec::new();
        let actors = standalone_actors(
            bindings.component_owners(PLAYER)?,
            bindings.component_owners(contact::SETTING)?,
        );
        for &id in actors.iter() {
            let player = bindings
                .component::<State>(id, PLAYER)?
                .context("missing Silver Wolf motion player")?;
            joints.push(id);
            joints.extend_from_slice(&player.bones);
        }
        Ok(TransformTargets {
            reads: joints.clone(),
            writes: joints,
        })
    }
    fn run(&self, cx: &mut SystemContext<'_>, dt: f32) -> Result<()> {
        ensure!(dt.is_finite() && dt >= 0., "invalid motion time");
        if dt == 0. {
            return Ok(());
        }
        let actors = standalone_actors(
            cx.component_owners(PLAYER)?,
            cx.component_owners(contact::SETTING)?,
        );
        cx.for_each_parallel(&actors, |&id, cx| {
            let mut p: State = cx
                .component::<State>(id, PLAYER)?
                .context("missing Silver Wolf motion player")?
                .into_owned();
            self.publish_locomotion(id, &mut p, cx, dt)?;
            cx.set_bound_state(id, PLAYER, p)
        })
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
    set_actor_goal(w, id, side, forward)
}
fn set_actor_goal(w: &mut World, id: Id, side: i8, forward: i8) -> Result<()> {
    set_analog_goal(w, id, f32::from(side), f32::from(forward))
}
fn set_analog_goal(w: &mut World, id: Id, side: f32, forward: f32) -> Result<()> {
    let mut p = player(w, id)?.into_owned();
    if forward >= 0. {
        p.speed = 1.35 * forward;
        p.side = 1.2 * side * (1. - forward);
        p.turn = side * forward * 1.35;
    } else {
        let direction = Vec2::new(side, forward).clamp_length_max(1.);
        p.side = direction.x * 1.2;
        p.speed = direction.y * 0.9;
        p.turn = 0.;
    }
    p.move_root = true;
    p.search_in = 0;
    w.set_typed_component(id, PLAYER, p);
    Ok(())
}
pub fn set_root_source(w: &mut World, target: Option<Id>, mode: &str) -> Result<()> {
    let source = match mode {
        "controller" => profile::RootSource::Controller,
        "clip" => profile::RootSource::Clip,
        _ => anyhow::bail!("motionroot controller/clip"),
    };
    let id = target
        .or_else(|| actor(w))
        .context("no Silver Wolf motion player")?;
    let mut p = player(w, id)?.into_owned();
    p.root_source = source;
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
#[cfg(test)]
mod tests;
