use super::*;

#[test]
fn elbow_profile_limits_the_published_skeleton_and_resumes_after_reload() -> Result<()> {
    let w = scene()?;
    let id = actor(&w).unwrap();
    let rig = Rig::bind(&w, id, &rig::children(&w))?;
    let joints = rig.arm.map(|index| rig.ids[index]);
    let configure = |mut w: World, limited: bool| -> Result<_> {
        let mut c = w
            .typed_component::<Contact>(id, SETTING)?
            .unwrap()
            .into_owned();
        c.auto_walk = false;
        c.foot_ik = false;
        c.hand_ik = false;
        if limited {
            PhysicalPose::edit(&mut c.physical_pose, "arm", &["limit", "80", "90"])?;
            PhysicalPose::edit(&mut c.physical_pose, "arm", &["drive", "0.008", "5"])?;
            PhysicalPose::configure(&mut c.physical_pose, "off")?;
            assert!(!c.physical_pose.as_ref().unwrap().active());
        }
        PhysicalPose::configure(&mut c.physical_pose, "on")?;
        w.set_typed_component(id, SETTING, c);
        let mut schedule = Schedule::default();
        install(&w, &mut schedule)?;
        Ok((Arc::new(w), schedule))
    };
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let (mut control, mut a) = configure(w.clone(), false)?;
    let (mut driven, mut b) = configure(w, true)?;
    let angle = |w: &World| -> Result<f32> {
        let m = w.transforms()?;
        let [a, b, c] = joints.map(|id| m[&id].w_axis.truncate());
        Ok((b - a).angle_between(c - b).to_degrees())
    };
    let mut ranges = [(180.0f32, 0.0f32); 2];
    for _ in 0..90 {
        a.tick(&mut control, 1. / 60., &pool)?;
        b.tick(&mut driven, 1. / 60., &pool)?;
        for (range, deg) in ranges.iter_mut().zip([angle(&control)?, angle(&driven)?]) {
            range.0 = range.0.min(deg);
            range.1 = range.1.max(deg);
        }
        assert_eq!(control.entities[&id].local, driven.entities[&id].local);
    }
    eprintln!(
        "90 substeps, rendered elbow flexion, unrestricted {:?}, limited {:?}",
        ranges[0], ranges[1]
    );
    assert!(ranges[0].0 < 75. || ranges[0].1 > 95.);
    assert!(ranges[1].0 >= 79. && ranges[1].1 <= 91., "{ranges:?}");
    let c = driven.typed_component::<Contact>(id, SETTING)?.unwrap();
    let profile = c.physical_pose.as_ref().unwrap();
    assert_eq!(profile.drives[1..], [LimbDrive::default(); 2]);
    let mut edited = c.physical_pose.clone();
    assert!(PhysicalPose::edit(&mut edited, "arm", &["limit", "90", "30"]).is_err());
    assert!(Arc::ptr_eq(profile, edited.as_ref().unwrap()));
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("constrained.json");
    driven.save(&file)?;
    let mut restored = World::load(&file)?;
    crate::experiments::SceneConfiguration::restore(&mut restored)?;
    let mut replay = Schedule::default();
    install(&restored, &mut replay)?;
    replay.restore_clocks(&restored)?;
    let mut restored = Arc::new(restored);
    let one = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
    for _ in 0..12 {
        b.tick(&mut driven, 1. / 60., &pool)?;
        replay.tick(&mut restored, 1. / 60., &one)?;
        for j in joints {
            assert!(
                driven.entities[&j]
                    .local
                    .abs_diff_eq(restored.entities[&j].local, 1e-5)
            );
        }
    }
    Ok(())
}
