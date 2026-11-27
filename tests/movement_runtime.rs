use anyhow::Result;
use glam::{Mat4, Vec3};
use render_lab::{
    engine::Schedule,
    experiments::movement_lab,
    movement::linear::{self, LINEAR_BODY, LinearBody},
    scene::{Mobility, MotionPolicy, PhysicsParticipation, World},
};
use std::sync::Arc;

fn advance(world: &mut Arc<World>, steps: usize, dt: f32, workers: usize) -> Result<()> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()?;
    let mut clock = Schedule::default();
    linear::register(&mut clock)?;
    for _ in 0..steps {
        clock.tick(world, dt, &pool)?;
    }
    Ok(())
}

#[test]
fn motion_authority_survives_save_and_numeric_integration_is_frame_rate_independent() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let mut world = Arc::new(movement_lab::scene()?);
    let ids = world.entities.component_owners(LINEAR_BODY).to_vec();
    let mut alternate = world.clone();
    let before: Vec<_> = ids.iter().map(|id| world.entities[id].local).collect();
    advance(&mut world, 30, 1. / 60., 1)?;
    advance(&mut alternate, 15, 1. / 30., 4)?;
    for id in &ids {
        assert_eq!(world.entities[id].local, alternate.entities[id].local);
        assert_eq!(world.linear_body(*id)?, alternate.linear_body(*id)?);
    }
    assert_eq!(world.entities[&ids[0]].local, before[0]);
    assert!((world.entities[&ids[1]].local.w_axis.x - before[1].w_axis.x - 0.3).abs() < 1e-5);
    assert!((world.entities[&ids[2]].local.w_axis.y - (2. - 0.5 * 9.81 * 0.5 * 0.5)).abs() < 1e-5);
    assert!((world.linear_body(ids[2])?.unwrap().momentum.y + 9.81 * 0.5).abs() < 1e-5);
    let path = dir.path().join("movement.json");
    world.save(&path)?;
    let mut resumed = Arc::new(World::load(&path)?);
    advance(&mut resumed, 1, 1. / 60., 4)?;
    advance(&mut world, 1, 1. / 60., 1)?;
    for id in ids {
        assert_eq!(resumed.entities[&id].local, world.entities[&id].local);
    }
    Ok(())
}

#[test]
fn changing_participation_and_teleporting_uses_current_transform() -> Result<()> {
    let mut w = movement_lab::scene()?;
    let id = w.entities.component_owners(LINEAR_BODY)[2];
    let invalid = MotionPolicy {
        mobility: Mobility::Static,
        physics: PhysicsParticipation::Simulated,
    };
    assert!(w.set_motion_policy(id, invalid).is_err());
    assert_eq!(
        w.motion_policy(id, MotionPolicy::STATIC)?,
        MotionPolicy::DYNAMIC
    );
    assert!(
        w.set_linear_body(
            id,
            LinearBody {
                mass: 0.,
                ..Default::default()
            }
        )
        .is_err()
    );
    w.set_motion_policy(id, MotionPolicy::FIXED_COLLIDER)?;
    let start = Mat4::from_translation(Vec3::new(10., 3., 4.));
    w.set_local(id, start)?;
    let mut w = Arc::new(w);
    advance(&mut w, 2, 1. / 60., 2)?;
    assert_eq!(w.entities[&id].local, start);
    Arc::make_mut(&mut w).set_motion_policy(id, MotionPolicy::MOVING_COLLIDER)?;
    advance(&mut w, 1, 1. / 60., 2)?;
    assert!((w.entities[&id].local.w_axis.x - 10.01).abs() < 1e-5);
    assert_eq!(w.entities[&id].local.w_axis.y, 3.);
    Ok(())
}

#[test]
fn absent_policy_preserves_legacy_provider_defaults() -> Result<()> {
    let w = World::demo();
    let id = *w.entities.keys().next().unwrap();
    assert_eq!(
        w.motion_policy(id, MotionPolicy::STATIC)?,
        MotionPolicy::STATIC
    );
    assert_eq!(
        w.motion_policy(id, MotionPolicy::KINEMATIC)?,
        MotionPolicy::KINEMATIC
    );
    assert!(w.motion_policy(u64::MAX, MotionPolicy::STATIC).is_err());
    Ok(())
}

#[test]
fn movable_linear_modes_transfer_velocity_and_keep_vertical_momentum() -> Result<()> {
    let mut w = movement_lab::scene()?;
    let id = w.entities.component_owners(linear::CONTROL)[0];
    let inputs = render_lab::movement::MovementControls::new(Some(linear::DRIVER));
    inputs.apply_analog(
        &mut w,
        id,
        render_lab::movement::AnalogMove {
            side: 0.5,
            forward: 0.0,
        },
    )?;
    let kinematic = w.linear_body(id)?.unwrap();
    w.set_motion_policy(id, MotionPolicy::DYNAMIC)?;
    assert_eq!(
        w.linear_body(id)?.unwrap().momentum,
        kinematic.target_velocity * kinematic.mass
    );
    let mut w = Arc::new(w);
    advance(&mut w, 12, 1. / 60., 1)?;
    let before = w.linear_body(id)?.unwrap().momentum;
    inputs.apply_analog(
        Arc::make_mut(&mut w),
        id,
        render_lab::movement::AnalogMove {
            side: 0.0,
            forward: 0.25,
        },
    )?;
    let body = w.linear_body(id)?.unwrap();
    assert!(before.y < 0.);
    assert_eq!(body.momentum.y, before.y);
    Arc::make_mut(&mut w).set_motion_policy(id, MotionPolicy::KINEMATIC)?;
    let at = w.entities[&id].local.w_axis.truncate();
    advance(&mut w, 1, 1. / 60., 16)?;
    assert!(
        w.entities[&id]
            .local
            .w_axis
            .truncate()
            .abs_diff_eq(at + body.momentum / (body.mass * 60.), 1e-6)
    );
    Ok(())
}

#[test]
fn deleting_the_selected_actor_releases_control_across_id_reuse_and_save() -> Result<()> {
    let mut world = movement_lab::scene()?;
    let initial = world.entities.component_owners(linear::CONTROL)[0];
    let mut controls = render_lab::movement::MovementControls::new(Some(linear::DRIVER));
    let add = |w: &mut World| -> Result<_> {
        let id = w.spawn(Default::default());
        w.set_linear_body(id, LinearBody::default())?;
        w.enable_linear_control(id, 1.)?;
        Ok(id)
    };
    assert_eq!(controls.target(&world), Some(initial));
    let selected = add(&mut world)?;
    controls.select(&mut world, selected)?;
    assert_eq!(controls.target(&world), Some(selected));
    world.entities.remove(&selected);
    assert_eq!(controls.target(&world), None);
    let replacement = add(&mut world)?;
    assert_eq!(replacement, selected);
    assert_eq!(controls.target(&world), None);
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("selection.json");
    world.save(&path)?;
    assert_eq!(controls.target(&World::load(&path)?), None);
    controls.select(&mut world, replacement)?;
    world.save(&path)?;
    assert_eq!(controls.target(&World::load(&path)?), Some(replacement));
    Ok(())
}
