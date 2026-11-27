//! Suspend a rig without destroying its authored bindings or duplicating its root writer.
use super::*;

pub(super) const PARKED: &str = "lab.silver_wolf.suspended_contact";

#[derive(Clone, Serialize, Deserialize)]
struct Suspended {
    contact: Contact,
    policy: MotionPolicy,
}

pub(super) fn switch(w: &mut World, actor: Id, active: bool) -> Result<()> {
    let e = w.entities.get(&actor).context("unknown character")?;
    ensure!(
        e.components.contains_key(PLAYER),
        "entity has no motion player"
    );
    let attached = e.components.contains_key(SETTING);
    if active == attached {
        ensure!(
            active || e.components.contains_key(PARKED),
            "entity has no contact rig"
        );
        return Ok(());
    }
    if active {
        let suspended = w
            .typed_component::<Suspended>(actor, PARKED)?
            .context("entity has no suspended rig")?
            .into_owned();
        ensure!(
            !e.components
                .contains_key(crate::movement::linear::LINEAR_BODY),
            "another solver owns the character root"
        );
        suspended.policy.validate()?;
        w.set_motion_policy(actor, suspended.policy)?;
        w.set_typed_component(actor, SETTING, suspended.contact);
        w.remove_typed_component(actor, PARKED);
    } else {
        let contact = w
            .typed_component::<Contact>(actor, SETTING)?
            .context("missing active rig")?
            .into_owned();
        let policy = w.motion_policy(actor, MotionPolicy::DYNAMIC)?;
        let free = if policy.mobility == Mobility::Static {
            policy
        } else {
            MotionPolicy::KINEMATIC
        };
        w.set_motion_policy(actor, free)?;
        w.set_typed_component(actor, PARKED, Suspended { contact, policy });
        w.remove_typed_component(actor, SETTING);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movement::character::{RESOLVED, ResolvedMotion};

    #[test]
    fn free_and_contact_handoff_keeps_carried_and_airborne_velocity_across_save() -> Result<()> {
        let one = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
        let many = rayon::ThreadPoolBuilder::new().num_threads(16).build()?;
        let mut w = scene()?;
        let id = actor(&w).unwrap();
        let mut rig = w
            .typed_component::<Contact>(id, SETTING)?
            .unwrap()
            .into_owned();
        traversal::Section::Gap.practice(&mut w, id, &mut rig)?;
        w.set_typed_component(rig.floor, dynamics::TURNTABLE, 0.8f32);
        w.set_typed_component(id, SETTING, rig);
        let mut source_schedule = Schedule::default();
        install(&w, &mut source_schedule)?;
        let mut w = Arc::new(w);
        let dt = 1. / 60.;
        let mut checked = [false; 2];
        for _ in 0..120 {
            source_schedule.tick(&mut w, dt, &one)?;
            let solved = w.typed_component::<ResolvedMotion>(id, RESOLVED)?.unwrap();
            let airborne = solved.support.is_none();
            let case = usize::from(airborne);
            if checked[case]
                || (!airborne && solved.support_velocity.length() < 0.05)
                || (airborne && solved.velocity.y < 0.5)
            {
                continue;
            }
            let at = w.entities[&id].local.w_axis.truncate();
            let expected = solved.velocity;
            let mut branch = w.as_ref().clone();
            switch(&mut branch, id, false)?;
            assert!(branch.entities.component_owners(SETTING).is_empty());
            let dir = tempfile::tempdir()?;
            let path = dir.path().join("detached.json");
            branch.save_with_clocks(&path, &source_schedule.checkpoints(&branch))?;
            let mut restored = World::load(&path)?;
            crate::experiments::SceneConfiguration::restore(&mut restored)?;
            assert!(restored.typed_component::<Suspended>(id, PARKED)?.is_some());
            for (mut world, pool) in [(branch, &one), (restored, &many)] {
                let mut schedule = Schedule::default();
                install(&world, &mut schedule)?;
                schedule.restore_clocks(&world)?;
                let mut running = Arc::new(world);
                schedule.tick(&mut running, dt, pool)?;
                let reached = running.entities[&id].local.w_axis.truncate();
                assert!(
                    reached.abs_diff_eq(at + dt * expected, 2e-5),
                    "carried={}; actual={reached:?}, velocity={expected:?}, previous={at:?}",
                    !airborne
                );
                world = Arc::try_unwrap(running).ok().unwrap();
                switch(&mut world, id, true)?;
                assert!(world.entities.component_owners(PARKED).is_empty());
                let mut running = Arc::new(world);
                schedule.tick(&mut running, dt, pool)?;
                let resumed = running
                    .typed_component::<ResolvedMotion>(id, RESOLVED)?
                    .unwrap();
                assert_eq!(running.entities[&id].local, resumed.root_local);
                assert!(
                    resumed.velocity.is_finite()
                        && (resumed.root_local.w_axis.truncate() - reached).length() < 0.3
                );
            }
            checked[case] = true;
            if checked == [true, true] {
                break;
            }
        }
        assert_eq!(checked, [true, true]);
        Ok(())
    }
}
