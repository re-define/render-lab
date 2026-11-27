//! Inspect or edit the selected contact controller and its fixtures.
use super::*;
use crate::engine::Engine;

pub fn command(engine: &mut Engine, arg: &str) -> Result<String> {
    let id = engine
        .movement
        .target(&engine.world)
        .or_else(|| engine.world.first_raw_component(SETTING).map(|(id, _)| id))
        .context("open the silver-contact experiment first")?;
    if let Some(mode) = arg.strip_prefix("mode ") {
        let active = match mode.trim() {
            "free" => false,
            "contact" => true,
            _ => anyhow::bail!("contact mode free|contact"),
        };
        modes::switch(Arc::make_mut(&mut engine.world), id, active)?;
        return Ok(format!("Character {id} mode {}", mode.trim()));
    }
    let (verb, rest) = arg.split_once(char::is_whitespace).unwrap_or((arg, ""));
    if verb == "graph" {
        return pose_graph::command(Arc::make_mut(&mut engine.world), id, rest.trim());
    }
    let c = engine
        .world
        .typed_component::<Contact>(id, SETTING)?
        .context("selected entity has no contact controller")?;
    match arg.split_whitespace().collect::<Vec<_>>().as_slice() {
        [] | ["status"] => {
            if c.source.is_empty() {
                return Ok("Waiting for the first contact substep".into());
            }
            let spring = engine
                .world
                .typed_component::<dynamics::Spring>(c.plate, dynamics::SPRING)?
                .context("missing spring plate")?;
            let deflection = spring.rest_y - engine.world.entities[&c.plate].local.w_axis.y;
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
        ["spin", speed] => {
            let rate = speed.parse::<f32>()?.to_radians();
            let policy = engine
                .world
                .motion_policy(c.floor, MotionPolicy::MOVING_COLLIDER)?;
            ensure!(
                rate == 0.
                    || (policy.mobility == Mobility::Movable
                        && policy.physics != PhysicsParticipation::Simulated),
                "spinning the deck requires a movable, nonsimulated support"
            );
            ensure!(rate.is_finite(), "invalid turntable speed");
            let floor = c.floor;
            Arc::make_mut(&mut engine.world).set_typed_component(floor, dynamics::TURNTABLE, rate);
            return Ok(format!("Launch deck spinning at {speed} degrees/s"));
        }
        ["target", x, y, z] => {
            let at = Vec3::new(x.parse()?, y.parse()?, z.parse()?);
            ensure!(at.is_finite(), "expected finite target coordinates");
            let target = c.target;
            move_target(Arc::make_mut(&mut engine.world), id, target, at)?;
            return Ok(format!("Contact {arg}"));
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
        words => {
            let contact = c.into_owned();
            edit_controller(engine, id, contact, words)?;
        }
    }
    Ok(format!("Contact {arg}"))
}

fn edit_controller(engine: &mut Engine, id: Id, mut c: Contact, words: &[&str]) -> Result<()> {
    match words {
        ["auto"] => c.auto_walk = true,
        ["manual"] => {
            c.auto_walk = false;
            set_actor_goal(Arc::make_mut(&mut engine.world), id, 0, 0)?;
        }
        ["hand", v @ ("on" | "off")] => c.hand_ik = *v == "on",
        ["limbs", value] => physical_pose::PhysicalPose::configure(&mut c.physical_pose, value)?,
        ["grip", "off"] => c.grip_fraction = 0.,
        ["grip", "on"] => c.grip_fraction = default_grip_fraction(),
        ["grip", v] => {
            let fraction = v.parse::<f32>()?;
            ensure!(
                (0.0..=1.0).contains(&fraction),
                "grip requires a weight fraction in 0..=1"
            );
            c.grip_fraction = fraction;
        }
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
        _ => anyhow::bail!(
            "contact status/physics/auto/manual · spin DEG/S · gravity M/S² · impulse N·s · action jump/vault/duck · hand on/off · grip on/off/FRACTION · limbs on/off/WEIGHT · feet on/off · target X Y Z · view wide/jump/vault/duck/finish/hand"
        ),
    }
    Arc::make_mut(&mut engine.world).set_typed_component(id, SETTING, c);
    Ok(())
}

fn move_target(world: &mut World, actor: Id, target: Id, point: Vec3) -> Result<()> {
    let frame = fixture::Fixture::bind(world, actor, target)?
        .parent_to_actor(|id| Ok(world.entities[&id].local))?;
    ensure!(
        frame.determinant().abs() > 1e-12,
        "singular interaction target parent"
    );
    let point = frame.inverse().transform_point3(point);
    let mut local = world
        .entities
        .get(&target)
        .context("missing touch marker")?
        .local;
    // The requested point lies on the near face of the marker.
    local.w_axis += (point - target_point(local)).extend(0.);
    world.set_local(target, local)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_the_touch_marker_only_invalidates_its_local_transform() -> Result<()> {
        let mut world = World::default();
        let actor = world.spawn(Entity::default());
        let target = world.spawn(Entity::default());
        let epoch = world.entities.cursor();
        let hierarchy = world.entities.hierarchy_revision();
        let point = Vec3::new(1., 2., 3.);
        move_target(&mut world, actor, target, point)?;
        assert!(target_point(world.entities[&target].local).abs_diff_eq(point, 1e-5));
        let edits = world.entities.changes(&epoch).unwrap();
        assert!(
            edits
                .writes()
                .eq([(target, crate::scene::EntityWrite::Local)])
        );
        assert_eq!(world.entities.hierarchy_revision(), hierarchy);
        Ok(())
    }
}
