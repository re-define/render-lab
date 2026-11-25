//! Silver Wolf traverses a maintenance course and operates a contact station.
mod course;
#[cfg(test)]
mod crowd_profile;
mod dynamics;
mod events;
mod rig;
mod stage;
#[cfg(test)]
mod tests;
mod traversal;

use super::*;
use crate::{
    engine::{Engine, Schedule},
    render::Camera,
};
#[cfg(test)]
use rig::FINGERTIP;
use rig::Rig;
use std::{collections::BTreeMap, time::Duration};

pub const SETTING: &str = "lab.silver_wolf.contact";
pub(in crate::experiments) const MODULE: SceneModule = SceneModule {
    key: SETTING,
    primary: Primary::Simulation,
    restore: Some(hydrate_contact),
    install: |w, a| {
        if crate::experiments::composition::enabled(w, "actor") {
            install(w, &mut a.schedule)?;
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
    for actor in w.entities.component_owners(SETTING).to_vec() {
        let mut c = w
            .typed_component::<Contact>(actor, SETTING)?
            .context("missing saved rig")?
            .into_owned();
        if w.typed_component::<dynamics::Spring>(c.plate, dynamics::SPRING)?
            .is_none()
        {
            w.set_typed_component(
                c.plate,
                dynamics::SPRING,
                dynamics::Spring {
                    rest_y: c.rest_y,
                    velocity: c.velocity,
                },
            );
        }
        if w.typed_component::<dynamics::Body>(actor, dynamics::BODY)?
            .is_some()
        {
            continue;
        }
        let mut body = dynamics::Body::new(w.entities[&actor].local.w_axis.truncate());
        if let Some(play) = &mut c.play {
            c.section.migrate_flight(play, &mut body);
        }
        w.set_typed_component(actor, SETTING, c);
        w.set_typed_component(actor, dynamics::BODY, body);
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

struct Stage {
    binding: Arc<PoseBinding>,
    actor: Id,
    rig: Rig,
    action_rig: Option<traversal::ActionRig>,
    supports: Vec<Id>,
    rail: Option<Id>,
    plate: Id,
    target: Id,
}

fn install(w: &World, schedule: &mut Schedule) -> Result<()> {
    let bank = Arc::new(SilverMatcher::load()?);
    schedule.fixed_stage_for_component(Duration::from_secs_f64(1. / 60.), 8, w, SETTING, move |w| {
        let children = rig::children(w);
        let mut actors = BTreeMap::new();
        let mut platforms = BTreeMap::<Id, Vec<Arc<Stage>>>::new();
        for &actor in w.entities.component_owners(SETTING) {
            let stage = Arc::new(bind_actor(w, actor, &children)?);
            platforms
                .entry(stage.plate)
                .or_default()
                .push(stage.clone());
            actors.insert(actor, stage);
        }
        let bindings: Vec<_> = actors
            .values()
            .map(|s| (s.actor, s.binding.clone()))
            .collect();
        let current = move |world: &World| {
            for (id, binding) in &bindings {
                let p = player(world, *id)?;
                let c = world
                    .typed_component::<Contact>(*id, SETTING)?
                    .context("missing contact")?;
                if !binding.matches(&p, &c) {
                    return Ok(false);
                }
            }
            Ok(true)
        };
        Ok((
            vec![
                Arc::new(stage::Prepare {
                    actors: actors.clone(),
                    bank: bank.clone(),
                }),
                Arc::new(dynamics::Integrate(platforms.into_values().collect())),
                Arc::new(stage::Feedback(actors.into_values().collect())),
            ],
            Box::new(current),
        ))
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
        rail: c.course.map(|f| f.rail),
        plate: c.plate,
        target: c.target,
    })
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

pub fn command(engine: &mut Engine, arg: &str) -> Result<String> {
    let (id, _) = engine
        .world
        .first_raw_component(SETTING)
        .context("open the silver-contact experiment first")?;
    let mut c = engine
        .world
        .typed_component::<Contact>(id, SETTING)?
        .unwrap()
        .into_owned();
    match arg.split_whitespace().collect::<Vec<_>>().as_slice() {
        [] | ["status"] => {
            if c.source.is_empty() {
                return Ok("Waiting for the first contact substep".into());
            }
            let deflection = c.rest_y - engine.world.entities[&c.plate].local.w_axis.y;
            return Ok(format!(
                "{} · Finger {:.3} mm · reach {:.2} · planted {} · plate {:.1} mm",
                if c.course.is_some() {
                    c.section.label()
                } else {
                    "console"
                },
                c.error_m * 1000.,
                c.reach,
                c.feet.iter().filter(|f| f.weight >= 0.999).count(),
                deflection * 1000.
            ));
        }
        ["physics"] => {
            let b = engine
                .world
                .typed_component::<dynamics::Body>(id, dynamics::BODY)?
                .context("missing body")?;
            return Ok(format!(
                "{} · gravity {:.2} m/s² · mass {:.1} kg · jump {:.1} N·s · velocity {:?} m/s · momentum {:?} kg·m/s · flight {:.3} s · landing normal impulse {:.2} N·s · palm {:.2} N·s",
                if b.support.is_some() {
                    "Grounded"
                } else {
                    "Airborne"
                },
                b.gravity,
                b.particle.mass,
                b.jump_impulse,
                b.particle.velocity(),
                b.particle.momentum,
                b.flight_time,
                b.landing_impulse,
                b.palm_impulse
            ));
        }
        [param @ ("gravity" | "impulse"), v] => {
            let mut b = engine
                .world
                .typed_component::<dynamics::Body>(id, dynamics::BODY)?
                .context("missing body")?
                .into_owned();
            let value = v.parse()?;
            if *param == "gravity" {
                b.gravity = value;
            } else {
                b.jump_impulse = value;
            }
            b.validate()?;
            Arc::make_mut(&mut engine.world).set_typed_component(id, dynamics::BODY, b);
            return Ok(format!("Contact {arg}"));
        }
        ["auto"] => c.auto_walk = true,
        ["manual"] => {
            c.auto_walk = false;
            set_keyboard_goal(Arc::make_mut(&mut engine.world), 0, 0)?;
        }
        ["hand", v @ ("on" | "off")] => c.hand_ik = *v == "on",
        ["feet", v @ ("on" | "off")] => c.foot_ik = *v == "on",
        ["action", name] => {
            let section = match *name {
                "jump" => traversal::Section::Gap,
                "vault" => traversal::Section::Rail,
                "duck" => traversal::Section::Beam,
                _ => anyhow::bail!("contact action jump/vault/duck"),
            };
            section.practice(Arc::make_mut(&mut engine.world), id, &mut c)?;
            engine.camera = course::camera(name).unwrap();
            engine.post.reset();
        }
        ["target", x, y, z] => {
            let at = Vec3::new(x.parse()?, y.parse()?, z.parse()?);
            ensure!(at.is_finite(), "expected finite target coordinates");
            let target = &mut Arc::make_mut(&mut engine.world)
                .entities
                .get_mut(&c.target)
                .context("missing touch marker")?
                .local;
            // The coordinates select the contact point on the near face of the sphere.
            target.w_axis += (at - target_point(*target)).extend(0.);
        }
        ["view", name] => {
            engine.camera = if *name == "hand" {
                let point = target_point(engine.world.entities[&c.target].local);
                let mut shot = camera(Some(point));
                if c.course.is_some() {
                    shot.target = point + Vec3::new(-0.17, -0.02, -0.15);
                    shot.yaw = 0.02;
                }
                shot
            } else if c.course.is_some() {
                course::camera(name).context("contact view wide/jump/vault/duck/finish/hand")?
            } else {
                ensure!(*name == "wide", "contact view wide/hand");
                camera(None)
            };
            engine.post.reset();
            return Ok(format!("Contact {arg}"));
        }
        _ => anyhow::bail!(
            "contact status/physics/auto/manual · gravity M/S² · impulse N·s · action jump/vault/duck · hand on/off · feet on/off · target X Y Z · view wide/jump/vault/duck/finish/hand"
        ),
    }
    Arc::make_mut(&mut engine.world).set_typed_component(id, SETTING, c);
    Ok(format!("Contact {arg}"))
}
