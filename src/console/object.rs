use super::*;
use crate::{
    movement::linear::{self, LinearBody},
    scene::{Id, Mobility, MotionPolicy, PhysicsParticipation},
};
use glam::Vec3;

pub(super) fn command(engine: &mut Engine, args: &str) -> Result<String> {
    let words: Vec<_> = args.split_whitespace().collect();
    let Some((&first, tail)) = words.split_first() else {
        let ids = engine
            .world
            .entities
            .component_owners(crate::scene::MOTION_POLICY);
        return Ok(format!(
            "Explicit motion policies: {ids:?}. Inspect with object ID"
        ));
    };
    let id = first.parse::<Id>()?;
    let e = engine.world.entities.get(&id).context("unknown entity")?;
    let body = engine.world.linear_body(id)?;
    let provider = engine.world.root_provider(e);
    let fallback = provider.unwrap_or(if body.is_some() {
        MotionPolicy::KINEMATIC
    } else {
        MotionPolicy::STATIC
    });
    let policy = engine.world.motion_policy(id, fallback)?;
    if tail == ["drive", "off"] {
        Arc::make_mut(&mut engine.world).disable_linear_control(id)?;
        return Ok(format!("Entity {id} linear control released"));
    }
    if let ["drive", speed] = tail {
        let speed = speed.parse()?;
        Arc::make_mut(&mut engine.world).enable_linear_control(id, speed)?;
        return Ok(format!("Entity {id} linear control speed {speed} m/s"));
    }
    if let ["impulse", x, y, z, location @ ..] = tail {
        let point = match location {
            [] => None,
            ["at", px, py, pz] => Some(Vec3::new(px.parse()?, py.parse()?, pz.parse()?)),
            _ => bail!("object ID impulse JX JY JZ [at PX PY PZ]"),
        };
        let j = Vec3::new(x.parse()?, y.parse()?, z.parse()?);
        Arc::make_mut(&mut engine.world).apply_body_impulse(id, j, point)?;
        return Ok(format!("Entity {id} · impulse {j:?} N·s"));
    }
    let mut next_body = None;
    let next_policy = match tail {
        [] => {
            let binding = e.instance.as_ref().and_then(|i| i.geometry_assets);
            let assets =
                binding.and_then(|a| engine.world.entities.get(&a)?.geometry_assets.as_deref());
            return Ok(format!(
                "{} ({id}) · {:?}/{:?} · linear {:?} · angular momentum {:?} · geometry assets {:?} · bounds {:?} · shadow mesh {:?}",
                e.name,
                policy.mobility,
                policy.physics,
                body,
                engine.world.angular_body(id)?.map(|a| a.momentum),
                binding,
                assets.and_then(|a| a.local_bounds),
                assets.and_then(|a| a.shadow_mesh)
            ));
        }
        ["velocity", x, y, z] => {
            let mut value = body.context("this entity has no linear body")?;
            value.target_velocity = Vec3::new(x.parse()?, y.parse()?, z.parse()?);
            if policy.physics == PhysicsParticipation::Simulated {
                value.momentum = value.target_velocity * value.mass;
            }
            value.validate()?;
            next_body = Some(value);
            policy
        }
        [mobility, physics] => {
            let mobility = match *mobility {
                "static" => Mobility::Static,
                "movable" => Mobility::Movable,
                _ => bail!("use static or movable"),
            };
            let physics = match *physics {
                "off" => PhysicsParticipation::Off,
                "query" => PhysicsParticipation::QueryOnly,
                "simulated" => PhysicsParticipation::Simulated,
                _ => bail!("use off, query or simulated"),
            };
            let policy = MotionPolicy { mobility, physics };
            policy.validate()?;
            if physics == PhysicsParticipation::Simulated
                && provider.is_some_and(|p| p.physics != PhysicsParticipation::Simulated)
            {
                bail!("entity {id} uses a kinematic root provider; a dynamic solver is required");
            }
            if mobility == Mobility::Movable && body.is_none() && provider.is_none() {
                next_body = Some(LinearBody::default());
            }
            policy
        }
        _ => bail!(
            "object ID [static|movable off|query|simulated] or velocity X Y Z | impulse JX JY JZ [at PX PY PZ]"
        ),
    };
    if next_body.is_some() {
        ensure!(
            provider.is_none(),
            "entity {id} already has a root motion provider"
        );
        if engine.schedule.fixed_seconds(linear::CLOCK).is_none() {
            linear::register(&mut engine.schedule)?;
            engine.movement.register(linear::DRIVER)?;
        }
    }
    // The ID, existing policy, next body and root ownership were checked above.
    let world = Arc::make_mut(&mut engine.world);
    if let Some(body) = next_body {
        world.set_linear_body(id, body)?;
    }
    world.set_motion_policy(id, next_policy)?;
    Ok(format!("Updated object {id}"))
}
