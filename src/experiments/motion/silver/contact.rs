//! Silver Wolf traverses a maintenance course and operates a contact station.
mod commands;
pub use commands::command;
mod course;
#[cfg(test)]
mod crowd_profile;
mod dynamics;
pub(crate) use dynamics::{SPRING, TURNTABLE};
mod events;
mod population;
mod rig;
mod stage;
#[cfg(test)]
mod tests;
mod traversal;

use super::*;
use crate::{engine::Schedule, render::Camera};
#[cfg(test)]
use rig::FINGERTIP;
use rig::Rig;
use std::{collections::BTreeMap, time::Duration};

pub const SETTING: &str = "lab.silver_wolf.contact";
pub(in crate::experiments) const MODULE: SceneModule = SceneModule {
    root_writers: &[
        crate::scene::RootWriter::new(SETTING, MotionPolicy::DYNAMIC),
        crate::scene::RootWriter::new(SPRING, MotionPolicy::DYNAMIC),
        crate::scene::RootWriter::new(TURNTABLE, MotionPolicy::MOVING_COLLIDER),
    ],
    key: SETTING,
    primary: Primary::Simulation,
    restore: Some(hydrate_contact),
    install: |w, a| {
        let players = w.entities.component_owners(PLAYER);
        for id in w.entities.component_owners(SETTING) {
            ensure!(
                players.binary_search(id).is_ok(),
                "contact entity {id} needs a Silver Wolf motion player"
            );
        }
        let (id, _) = w
            .first_raw_component(SETTING)
            .context("missing contact rig")?;
        let c = w
            .typed_component::<Contact>(id, SETTING)?
            .context("missing contact state")?;
        a.camera = if c.course.is_some() {
            course::camera("wide")
        } else {
            Some(camera(None))
        };
        Ok(())
    },
};

fn hydrate_contact(w: &mut World) -> Result<()> {
    w.hydrate_components::<Contact>(SETTING)?;
    w.hydrate_components::<dynamics::Body>(dynamics::BODY)?;
    w.hydrate_components::<dynamics::Spring>(dynamics::SPRING)?;
    w.hydrate_components::<f32>(dynamics::TURNTABLE)?;
    for actor in w.entities.component_owners(SETTING).to_vec() {
        let c = w
            .typed_component::<Contact>(actor, SETTING)?
            .context("missing saved rig")?;
        let plate = c.plate;
        let plate_entity = w.entities.get(&plate).context("missing contact plate")?;
        let legacy_spring =
            (!plate_entity.components.contains_key(dynamics::SPRING)).then_some(dynamics::Spring {
                rest_y: c.rest_y,
                velocity: c.velocity,
            });
        if !w.entities[&actor].components.contains_key(dynamics::BODY) {
            let mut body = dynamics::Body::new(w.entities[&actor].local.w_axis.truncate());
            let mut c = c.into_owned();
            if let Some(play) = &mut c.play {
                c.section.migrate_flight(play, &mut body);
            }
            w.set_typed_component(actor, SETTING, c);
            w.set_typed_component(actor, dynamics::BODY, body);
        }
        if let Some(spring) = legacy_spring {
            w.set_typed_component(plate, dynamics::SPRING, spring);
        }
    }
    Ok(())
}

fn camera(contact: Option<Vec3>) -> Camera {
    if let Some(point) = contact {
        Camera {
            target: point + Vec3::new(0.15, -0.02, -0.17),
            distance: 0.91,
            yaw: -1.80,
            pitch: 0.10,
        }
    } else {
        Camera {
            target: Vec3::new(0., 0.78, -0.09),
            distance: 2.52,
            yaw: -1.08,
            pitch: 0.12,
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Foot {
    // Locked in the support's local coordinates so it rides the plate.
    anchor: Option<(Id, Vec3)>,
    #[serde(default)]
    weight: f32,
    #[serde(default)]
    releasing: bool,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct FingerMarker {
    joint: Id,
    offset: Vec3,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct PoseBinding {
    bones: Vec<Id>,
    hip_origin: Vec3,
    hierarchy: Vec<(Id, Option<Id>)>,
    finger: Option<FingerMarker>,
    floor: Id,
    plate: Id,
    target: Id,
    course: Option<course::Fixtures>,
}

impl PoseBinding {
    fn matches(&self, player: &State, contact: &Contact) -> bool {
        self.bones == player.bones
            && self.hip_origin == player.hip_origin
            && self.finger == contact.finger
            && self.floor == contact.floor
            && self.plate == contact.plate
            && self.target == contact.target
            && self.course == contact.course
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Contact {
    /// Scratch produced before dynamics in the same character-clock transaction.
    #[serde(skip)]
    request: Option<ContactMotion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_binding: Option<Arc<PoseBinding>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finger: Option<FingerMarker>,
    #[serde(default)]
    course: Option<course::Fixtures>,
    #[serde(default)]
    section: traversal::Section,
    #[serde(default)]
    play: Option<traversal::Playback>,
    floor: Id,
    plate: Id,
    target: Id,
    rest_y: f32,
    // Read legacy saves whose spring velocity lived on the actor.
    #[serde(default, skip_serializing)]
    velocity: f32,
    feet: [Foot; 2],
    #[serde(default)]
    palm_load: f32,
    pelvis: f32,
    reach: f32,
    auto_walk: bool,
    foot_ik: bool,
    hand_ik: bool,
    source: Vec<Mat4>,
    error_m: f32,
}

#[derive(Clone, Copy)]
struct ContactMotion {
    root: RootMotionRequest,
    inherited_velocity: Option<Vec3>,
}

struct Stage {
    binding: Arc<PoseBinding>,
    actor: Id,
    rig: Rig,
    action_rig: Option<traversal::ActionRig>,
    supports: Vec<Id>,
}

/// Character motion, contacts and pose feedback advance on this clock. Additional
/// gameplay systems can order themselves relative to these producers by system key.
pub fn character_clock() -> crate::engine::FixedDomain {
    crate::engine::FixedDomain {
        name: "character",
        step: Duration::from_secs_f64(1. / 60.),
        max_steps: 8,
    }
}

type Actors = BTreeMap<Id, Arc<Stage>>;
type CharacterSystems = crate::engine::BoundSystems<Actors>;

pub(super) fn register(w: &World, schedule: &mut Schedule, bank: Arc<SilverMatcher>) -> Result<()> {
    let mut systems = CharacterSystems::default();
    stage::register_motion(&mut systems, bank);
    dynamics::register(&mut systems, schedule)?;
    stage::register_ik(&mut systems);
    let cached = std::sync::Mutex::new((None::<(Arc<()>, u64)>, Actors::new()));
    systems.install(schedule, character_clock(), w, SETTING, move |w| {
        let topology = w.entities.hierarchy_revision();
        let mut prior = cached.lock().expect("character binding cache");
        let mut actors = Actors::new();
        let children = std::cell::OnceCell::new();
        for &actor in w.entities.component_owners(SETTING) {
            if prior.0.as_ref().is_some_and(|(source, rev)| {
                *rev == topology && Arc::ptr_eq(source, w.entities.hierarchy_source())
            }) && let Some(stage) = prior.1.get(&actor)
                && stage.binding.matches(
                    player(w, actor)?.as_ref(),
                    w.typed_component::<Contact>(actor, SETTING)?
                        .context("missing contact")?
                        .as_ref(),
                )
            {
                actors.insert(actor, stage.clone());
                continue;
            }
            actors.insert(
                actor,
                Arc::new(bind_actor(
                    w,
                    actor,
                    children.get_or_init(|| rig::children(w)),
                )?),
            );
        }
        *prior = (
            Some((w.entities.hierarchy_source().clone(), topology)),
            actors.clone(),
        );
        let versions = [w.binding_version(PLAYER), w.binding_version(SETTING)];
        let current = move |w: &World| {
            Ok(versions == [w.binding_version(PLAYER), w.binding_version(SETTING)])
        };
        Ok((actors, Box::new(current)))
    })
}

fn bind_actor(w: &World, actor: Id, children: &BTreeMap<Id, Vec<Id>>) -> Result<Stage> {
    let c = w
        .typed_component::<Contact>(actor, SETTING)?
        .context("missing contact state")?;
    ensure!(
        c.play
            .is_none_or(|p| c.course.is_some() && p.valid(c.section)),
        "invalid saved traversal playback"
    );
    w.typed_component::<dynamics::Body>(actor, dynamics::BODY)?
        .context("missing character body")?
        .validate()?;
    let mut supports = vec![c.floor, c.plate];
    if let Some(f) = c.course {
        supports.extend([f.landing, f.station, f.ramp]);
        supports.extend(f.service);
    }
    for id in supports
        .iter()
        .copied()
        .chain([c.target])
        .chain(c.course.into_iter().flat_map(|f| [f.rail, f.gate]))
    {
        ensure!(
            w.entities
                .get(&id)
                .is_some_and(|e| e.parent == w.entities[&actor].parent),
            "invalid contact fixture {id}"
        );
    }
    let rig = Rig::bind(w, actor, children)?;
    let action_rig = c
        .course
        .map(|_| traversal::ActionRig::bind(w, actor, &rig))
        .transpose()?;
    ensure!(
        c.source.is_empty()
            || c.source.len() == rig.corrected.len()
            || (c.course.is_none() && c.source.len() == 10),
        "invalid saved contact pose"
    );
    let state = player(w, actor)?;
    let binding = Arc::new(PoseBinding {
        bones: state.bones.clone(),
        hip_origin: state.hip_origin,
        hierarchy: rig
            .ids
            .iter()
            .map(|&id| (id, w.entities[&id].parent))
            .collect(),
        finger: c.finger.clone(),
        floor: c.floor,
        plate: c.plate,
        target: c.target,
        course: c.course,
    });
    Ok(Stage {
        binding,
        actor,
        rig,
        action_rig,
        supports,
    })
}

/// Both lanes use Silver Wolf geometry. The second rig has a longer wrist chain and explicit fingertip.
pub fn duo_scene() -> Result<World> {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
    let mut world = population::populated_world(2, &pool, true)?;
    let second = world.entities.component_owners(PLAYER)[1];
    population::alternate_rig(&mut world, second)?;
    world.validate()?;
    Ok(world)
}

pub fn scene() -> Result<World> {
    let mut w = super::scene()?;
    let actor = actor(&w).context("missing Silver Wolf")?;
    w.entities.get_mut(&actor).unwrap().local = Mat4::from_translation(Vec3::new(0., 0., -3.89));
    w.set_typed_component(
        actor,
        dynamics::BODY,
        dynamics::Body::new(w.entities[&actor].local.w_axis.truncate()),
    );
    let (floor, plate, target, fixtures) = course::build(&mut w);
    w.set_motion_policy(actor, MotionPolicy::DYNAMIC)?;
    w.set_motion_policy(plate, MotionPolicy::DYNAMIC)?;
    w.set_motion_policy(floor, MotionPolicy::MOVING_COLLIDER)?;
    for id in [fixtures.landing, fixtures.station, fixtures.ramp]
        .into_iter()
        .chain(fixtures.service)
    {
        w.set_motion_policy(id, MotionPolicy::FIXED_COLLIDER)?;
    }
    w.set_typed_component(
        plate,
        dynamics::SPRING,
        dynamics::Spring {
            rest_y: 0.10,
            velocity: 0.,
        },
    );
    w.set_typed_component(
        actor,
        SETTING,
        Contact {
            request: None,
            source_binding: None,
            finger: None,
            floor,
            plate,
            target,
            course: Some(fixtures),
            section: Default::default(),
            play: None,
            rest_y: 0.10,
            velocity: 0.,
            feet: Default::default(),
            palm_load: 0.,
            pelvis: 0.,
            reach: 0.,
            auto_walk: true,
            foot_ik: true,
            hand_ik: true,
            source: vec![],
            error_m: 0.,
        },
    );
    w.validate()?;
    Ok(w)
}

fn target_point(model: Mat4) -> Vec3 {
    model.transform_point3(Vec3::NEG_Z * 0.5)
}
