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
    // Principal moments of the unit-radius solid octahedron at 1 kg.
    world.set_angular_body(
        falling,
        crate::physics::AngularBody {
            inertia: Vec3::splat(0.2),
            momentum: Vec3::ZERO,
            torque: Vec3::ZERO,
        },
    )?;
    world.enable_linear_control(walker, 1.5)?;
    world.validate()?;
    Ok(world)
}

/// Three independently preloaded groups around the persistent movement actors.
pub fn partitioned() -> Result<World> {
    use crate::scene::partition::{Cell, Layout};
    let mut w = scene()?;
    let template = w
        .entities
        .values()
        .find(|e| e.name == "Gravity body")
        .unwrap();
    let mut cells = Vec::new();
    let base = crate::assets::cache_dir().join("world-cells");
    std::fs::create_dir_all(&base)?;
    for i in 0..3 {
        let center = glam::Vec3::new(20. * i as f32, 0., 0.);
        let rows:Vec<_>=(0..6).map(|n|serde_json::json!({
            "name":format!("Cell {i} body {n}"),"instance":template.instance,"components":template.components,
            "local":Mat4::from_translation(center+Vec3::new((n%3) as f32-1.,4.+(n/3) as f32,0.))
        })).collect();
        let data = serde_json::to_vec(&rows)?;
        let file = std::path::PathBuf::from(format!(
            "{}.json",
            crate::assets::key_name(&crate::assets::content_key("movement-cell", 1, &data))
        ));
        if !base.join(&file).is_file() {
            use std::io::Write;
            let mut tmp = tempfile::NamedTempFile::new_in(&base)?;
            tmp.write_all(&data)?;
            let target = base.join(&file);
            if let Err(e) = tmp.persist_noclobber(&target)
                && !target.is_file()
            {
                return Err(e.into());
            }
        }
        cells.push(Cell {
            first: 1000 + i * 16,
            count: rows.len(),
            center,
            file,
            save_state: true,
            assets: Default::default(),
        });
    }
    w.partition = Some(Layout::new(base, cells, 9., 14., 18.)?.into());
    Ok(w)
}
