use super::*;

struct Sample {
    root: Mat4,
    palm_impulse: f32,
    wrist: Mat4,
}

fn vault(visual: bool, grip: f32) -> Result<Vec<Sample>> {
    let mut w = scene()?;
    let id = actor(&w).unwrap();
    let mut contact = w
        .typed_component::<Contact>(id, SETTING)?
        .unwrap()
        .into_owned();
    traversal::Section::Rail.practice(&mut w, id, &mut contact)?;
    contact.hand_ik = visual;
    contact.grip_fraction = grip;
    w.set_typed_component(id, SETTING, contact);
    let stage = bind_actor(&w, id, &rig::children(&w))?;
    let wrist = stage.rig.ids[stage.rig.arm[2]];
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut schedule = Schedule::default();
    install(&w, &mut schedule)?;
    let mut w = Arc::new(w);
    let mut samples = Vec::new();
    for step in 1..=110 {
        schedule.tick(&mut w, 1. / 60., &pool)?;
        let proposal = w.typed_component::<ProposedMotion>(id, PROPOSED)?.unwrap();
        let resolved = w.typed_component::<ResolvedMotion>(id, RESOLVED)?.unwrap();
        let body = w
            .typed_component::<dynamics::Body>(id, dynamics::BODY)?
            .unwrap();
        assert_eq!((proposal.step, resolved.step), (step, step));
        assert_eq!(resolved.root_local, w.entities[&id].local);
        samples.push(Sample {
            root: resolved.root_local,
            palm_impulse: body.palm_impulse,
            wrist: w.entities[&wrist].local,
        });
    }
    Ok(samples)
}

#[test]
fn vault_hand_alignment_and_mechanical_support_are_independent() -> Result<()> {
    let both = vault(true, default_grip_fraction())?;
    let physical_only = vault(false, default_grip_fraction())?;
    let visual_only = vault(true, 0.)?;
    assert!(
        both.iter()
            .zip(&physical_only)
            .all(|(a, b)| a.root.abs_diff_eq(b.root, 1e-6)
                && (a.palm_impulse - b.palm_impulse).abs() < 1e-6)
    );
    assert!(
        both.iter()
            .zip(&physical_only)
            .any(|(a, b)| !a.wrist.abs_diff_eq(b.wrist, 1e-3))
    );
    assert!(both.iter().any(|a| a.palm_impulse > 1.));
    assert!(visual_only.iter().all(|a| a.palm_impulse == 0.));
    assert!(
        both.iter()
            .zip(&visual_only)
            .any(|(a, b)| (a.root.w_axis.y - b.root.w_axis.y).abs() > 0.001)
    );
    Ok(())
}

#[test]
fn fixture_query_tracks_an_independent_moving_parent() -> Result<()> {
    let mut w = World::default();
    let common = w.spawn(Entity::default());
    let carrier = w.spawn(Entity {
        parent: Some(common),
        local: Mat4::from_rotation_y(0.9),
        ..Default::default()
    });
    let actor = w.spawn(Entity {
        parent: Some(carrier),
        ..Default::default()
    });
    let arm = w.spawn(Entity {
        parent: Some(common),
        local: Mat4::from_translation(Vec3::new(1., 2., 3.)),
        ..Default::default()
    });
    let marker = w.spawn(Entity {
        parent: Some(arm),
        local: Mat4::from_translation(Vec3::Z),
        ..Default::default()
    });
    let target = fixture::Fixture::bind(&w, actor, marker)?;
    assert!(!target.reads().any(|id| id == common));
    for angle in [0., 0.6, 1.3] {
        w.set_local(
            arm,
            Mat4::from_rotation_z(angle) * Mat4::from_translation(Vec3::X),
        )?;
        let matrices = w.transforms()?;
        let expected = matrices[&carrier].inverse() * matrices[&marker];
        assert!(expected.abs_diff_eq(target.relative(|id| Ok(w.entities[&id].local))?, 1e-5));
    }
    Ok(())
}

#[test]
fn driven_limbs_preserve_controller_root_and_resume_from_a_save() -> Result<()> {
    let w = scene()?;
    let id = actor(&w).unwrap();
    let rig = bind_actor(&w, id, &rig::children(&w))?.rig;
    let elbow = rig.ids[rig.arm[1]];
    let make = |mut w: World, weight: &str| -> Result<_> {
        let mut contact = w
            .typed_component::<Contact>(id, SETTING)?
            .unwrap()
            .into_owned();
        contact.auto_walk = false;
        contact.hand_ik = false;
        contact.foot_ik = false;
        physical_pose::PhysicalPose::configure(&mut contact.physical_pose, weight)?;
        w.set_typed_component(id, SETTING, contact);
        let mut schedule = Schedule::default();
        install(&w, &mut schedule)?;
        Ok((Arc::new(w), schedule))
    };
    let (mut base, mut control) = make(w.clone(), "off")?;
    let (mut driven, mut simulate) = make(w, "on")?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    for _ in 0..45 {
        control.tick(&mut base, 1. / 60., &pool)?;
        simulate.tick(&mut driven, 1. / 60., &pool)?;
        assert!(
            base.entities[&id]
                .local
                .abs_diff_eq(driven.entities[&id].local, 1e-6)
        );
    }
    assert!(
        !base.entities[&elbow]
            .local
            .abs_diff_eq(driven.entities[&elbow].local, 1e-3)
    );
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("driven.scene.json");
    driven.save(&path)?;
    let mut loaded = World::load(&path)?;
    crate::experiments::SceneConfiguration::restore(&mut loaded)?;
    let mut replay = Schedule::default();
    install(&loaded, &mut replay)?;
    let mut loaded = Arc::new(loaded);
    for _ in 0..6 {
        replay.tick(&mut loaded, 1. / 60., &pool)?;
        simulate.tick(&mut driven, 1. / 60., &pool)?;
        assert!(
            driven.entities[&elbow]
                .local
                .abs_diff_eq(loaded.entities[&elbow].local, 1e-4)
        );
        assert!(
            driven.entities[&id]
                .local
                .abs_diff_eq(loaded.entities[&id].local, 1e-5)
        );
    }
    Ok(())
}
