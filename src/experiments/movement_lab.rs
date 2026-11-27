//! Minimal rigid movement scene. All three objects share one visual mesh.
use super::assembly::{Primary, Registration};
use crate::{
    movement::linear::{LINEAR_BODY, LinearBody},
    scene::{Entity, MotionPolicy, World},
};
use anyhow::{Context, Result};
use glam::{Mat4, Vec3};

pub(super) const MODULE: Registration = Registration {
    root_writers: &[],
    key: LINEAR_BODY,
    primary: Primary::Simulation,
    restore: None,
    install: |_, assembly| {
        crate::movement::linear::register(&mut assembly.schedule)?;
        assembly.movement.register(crate::movement::linear::DRIVER)
    },
};

/// Left: stationary, middle: commanded horizontal motion, right: free fall.
pub fn scene() -> Result<World> {
    let mut world = World::demo();
    let mut ids = world
        .entities
        .iter()
        .filter_map(|(&id, e)| e.instance.is_some().then_some(id));
    let fixed = ids.next().context("missing demo mesh")?;
    let walker = ids.next().context("missing second mesh")?;
    let falling = world.spawn(Entity {
        name: "Free body".into(),
        instance: world.entities[&fixed].instance.clone(),
        ..Default::default()
    });
    for (id, name, x, policy) in [
        (fixed, "Fixed object", -3., MotionPolicy::STATIC),
        (walker, "Kinematic object", 0., MotionPolicy::KINEMATIC),
        (falling, "Gravity body", 3., MotionPolicy::DYNAMIC),
    ] {
        world.entities.get_mut(&id).unwrap().name = name.into();
        world.set_local(id, Mat4::from_translation(Vec3::new(x, 2., 0.)))?;
        world.set_linear_body(
            id,
            LinearBody {
                target_velocity: 0.6 * Vec3::X,
                ..Default::default()
            },
        )?;
        world.set_motion_policy(id, policy)?;
    }
    world.enable_linear_control(walker, 1.5)?;
    world.validate()?;
    Ok(world)
}
