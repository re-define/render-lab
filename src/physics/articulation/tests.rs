use super::*;

#[test]
fn unilateral_contact_catches_a_falling_wrist_and_leaves_overhead_platforms_alone() -> Result<()> {
    let pose = [
        Vec3::Y * 0.06,
        Vec3::new(0.3, 0.06, 0.),
        Vec3::new(0.6, 0.06, 0.),
    ];
    let platform = |y: f32| {
        SupportFace::from_box(
            1,
            glam::Mat4::from_scale_rotation_translation(
                Vec3::new(3., 0.1, 3.),
                glam::Quat::IDENTITY,
                Vec3::Y * (y - 0.05),
            ),
            0.65,
        )
        .unwrap()
    };
    let mut grounded = LimbIsland::new(pose);
    let mut below = LimbIsland::new(pose);
    let mut free = LimbIsland::new(pose);
    for _ in 0..120 {
        let on_ground = grounded.advance(pose, Vec3::NEG_Y * 9.81, &[platform(0.)], 1. / 60.)?;
        let under = below.advance(pose, Vec3::NEG_Y * 9.81, &[platform(2.)], 1. / 60.)?;
        let without = free.advance(pose, Vec3::NEG_Y * 9.81, &[], 1. / 60.)?;
        assert!(on_ground[2].y >= 0.0149);
        assert_eq!(under, without);
    }
    assert!((grounded.position[2].y - 0.015).abs() < 0.001);
    assert!(free.position[2].y < 0.);
    Ok(())
}

#[test]
fn limb_lengths_and_target_drives_remain_stable_after_reload() -> Result<()> {
    let pose = [Vec3::Y, Vec3::Y + 0.35 * Vec3::X, Vec3::Y + 0.7 * Vec3::X];
    let mut at_60 = LimbIsland::new(pose);
    let mut at_120 = at_60.clone();
    for _ in 0..120 {
        at_60.advance(pose, Vec3::NEG_Y * 9.81, &[], 1. / 60.)?;
    }
    for _ in 0..240 {
        at_120.advance(pose, Vec3::NEG_Y * 9.81, &[], 1. / 120.)?;
    }
    for i in 1..3 {
        assert!((at_60.position[i - 1].distance(at_60.position[i]) - 0.35).abs() < 0.001);
        assert!(at_60.position[i].abs_diff_eq(at_120.position[i], 1e-4));
    }
    assert!(pose[2].y - at_60.position[2].y > 0.02);
    let mut loaded: LimbIsland = serde_json::from_str(&serde_json::to_string(&at_60)?)?;
    let raised = pose.map(|p| p + 0.1 * Vec3::Y);
    assert_eq!(
        at_60.advance(raised, Vec3::NEG_Y * 9.81, &[], 1. / 60.)?,
        loaded.advance(raised, Vec3::NEG_Y * 9.81, &[], 1. / 60.)?
    );
    Ok(())
}

#[test]
fn flexion_limits_project_collinear_chains_and_preserve_bone_lengths() -> Result<()> {
    let straight = [Vec3::ZERO, Vec3::X * 0.3, Vec3::X * 0.6];
    let folded = [Vec3::ZERO, Vec3::X * 0.3, Vec3::ZERO];
    let drive = LimbDrive {
        compliance: 0.,
        bend: Some(BendRange {
            min_degrees: 30.,
            max_degrees: 95.,
        }),
        ..Default::default()
    };
    for (goal, expected) in [(straight, 30.), (folded, 95.)] {
        let mut island = LimbIsland::new(goal);
        for _ in 0..24 {
            let p = island.advance_with_drive(goal, Vec3::ZERO, &[], 1. / 60., drive)?;
            assert!((island.flexion_degrees().unwrap() - expected).abs() < 0.005);
            for i in 1..3 {
                assert!((p[i].distance(p[i - 1]) - 0.3).abs() < 0.0001);
            }
        }
        let mut reload: LimbIsland = serde_json::from_str(&serde_json::to_string(&island)?)?;
        assert_eq!(
            island.advance_with_drive(goal, Vec3::ZERO, &[], 1. / 120., drive)?,
            reload.advance_with_drive(goal, Vec3::ZERO, &[], 1. / 120., drive)?
        );
        let shifted = goal.map(|p| p + 4. * Vec3::Y);
        island.advance_with_drive(shifted, Vec3::ZERO, &[], 1. / 60., drive)?;
        assert!((island.flexion_degrees().unwrap() - expected).abs() < 0.005);
    }
    let mut invalid = LimbIsland::new(straight);
    let before = serde_json::to_value(&invalid)?;
    for bad in [
        LimbDrive {
            compliance: -1.,
            ..drive
        },
        LimbDrive {
            damping: f32::NAN,
            ..drive
        },
        LimbDrive {
            bend: Some(BendRange {
                min_degrees: 120.,
                max_degrees: 30.,
            }),
            ..drive
        },
    ] {
        assert!(
            invalid
                .advance_with_drive(straight, Vec3::ZERO, &[], 1. / 60., bad)
                .is_err()
        );
        assert_eq!(serde_json::to_value(&invalid)?, before);
    }
    Ok(())
}

#[test]
fn compliance_changes_sag_and_damping_changes_release_velocity() -> Result<()> {
    let pose = [Vec3::ZERO, Vec3::X * 0.3, Vec3::X * 0.6];
    let mut hard = LimbIsland::new(pose);
    let mut soft = hard.clone();
    for _ in 0..60 {
        hard.advance_with_drive(
            pose,
            Vec3::NEG_Y * 9.81,
            &[],
            1. / 60.,
            LimbDrive {
                compliance: 0.,
                ..Default::default()
            },
        )?;
        soft.advance(pose, Vec3::NEG_Y * 9.81, &[], 1. / 60.)?;
    }
    assert!(hard.position[2].y.abs() < 1e-5 && soft.position[2].y < -0.02);
    let mut damped = soft.clone();
    soft.velocity[2] = Vec3::Y;
    damped.velocity[2] = Vec3::Y;
    let drive = LimbDrive {
        compliance: 1.,
        damping: 0.,
        bend: None,
    };
    soft.advance_with_drive(pose, Vec3::ZERO, &[], 1. / 120., drive)?;
    damped.advance_with_drive(
        pose,
        Vec3::ZERO,
        &[],
        1. / 120.,
        LimbDrive {
            damping: 60.,
            ..drive
        },
    )?;
    assert!(soft.velocity[2].y > damped.velocity[2].y + 0.2);
    Ok(())
}
