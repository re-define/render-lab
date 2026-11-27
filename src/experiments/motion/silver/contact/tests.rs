use super::*;

#[test]
fn plain_and_contact_characters_follow_their_own_clocks_after_membership_changes() -> Result<()> {
    use crate::{experiments::assembly::Assembly, scene::TickRate};

    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut world = population::populated_world(2, &pool, false)?;
    let [a, b]: [Id; 2] = world.entities.component_owners(PLAYER).try_into().unwrap();
    let detach = |w: &mut World, id| -> Result<Contact> {
        let saved = w
            .typed_component::<Contact>(id, SETTING)?
            .unwrap()
            .into_owned();
        w.entities.get_mut(&id).unwrap().components.remove(SETTING);
        w.set_motion_policy(id, MotionPolicy::KINEMATIC)?;
        Ok(saved)
    };
    let attach = |w: &mut World, id, c| -> Result<()> {
        w.set_typed_component(id, SETTING, c);
        w.set_motion_policy(id, MotionPolicy::DYNAMIC)
    };
    let stamp = |w: &World, id| -> Result<_> {
        let p = player(w, id)?;
        Ok((
            p.sample,
            p.fraction,
            p.bones
                .iter()
                .map(|id| w.entities[id].local)
                .collect::<Vec<_>>(),
        ))
    };
    let saved_a = detach(&mut world, a)?;
    let saved_b = detach(&mut world, b)?;
    set_actor_goal(&mut world, b, 0, 1)?;
    let mut assembly = Assembly {
        caps: Default::default(),
        materials: Default::default(),
        schedule: Default::default(),
        movement: Default::default(),
        camera: None,
        technique: None,
        visibility: Default::default(),
    };
    // Install through the module registry with no initial contact actors, then attach the first one live.
    (super::super::MODULE.install)(&world, &mut assembly)?;
    let mut schedule = assembly.schedule;
    let mut world = Arc::new(world);
    attach(Arc::make_mut(&mut world), a, saved_a)?;
    schedule.set_tick_rate("silver-motion", TickRate::Hertz(1))?;
    let (a0, b0) = (stamp(&world, a)?, stamp(&world, b)?);
    schedule.tick(&mut world, 1. / 30., &pool)?;
    assert_ne!(stamp(&world, a)?, a0);
    assert_eq!(stamp(&world, b)?, b0);

    // With the opposite cadence only the plain actor may publish a pose.
    schedule.set_tick_rate("silver-motion", TickRate::Default)?;
    schedule.set_tick_rate("character", TickRate::Hertz(1))?;
    let a1 = stamp(&world, a)?;
    for _ in 0..12 {
        schedule.tick(&mut world, 1. / 30., &pool)?;
    }
    assert_eq!(stamp(&world, a)?, a1);
    assert_ne!(stamp(&world, b)?, b0);

    let b1 = stamp(&world, b)?;
    detach(Arc::make_mut(&mut world), a)?;
    attach(Arc::make_mut(&mut world), b, saved_b)?;
    schedule.tick(&mut world, 1. / 30., &pool)?;
    assert_ne!(stamp(&world, a)?, a1);
    assert_eq!(stamp(&world, b)?, b1);

    // A save can land between reattaching the component and its first physics tick.
    let at = world.entities[&b].local.w_axis.truncate();
    let old_body = world
        .typed_component::<dynamics::Body>(b, dynamics::BODY)?
        .unwrap();
    assert!(at.distance(old_body.particle.position) > 0.15);
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("handoff.json");
    world.save(&path)?;
    let mut loaded = World::load(&path)?;
    crate::experiments::SceneConfiguration::restore(&mut loaded)?;
    let mut schedule = Schedule::default();
    install(&loaded, &mut schedule)?;
    let mut loaded = Arc::new(loaded);
    schedule.tick(&mut loaded, 1. / 60., &pool)?;
    let body = loaded
        .typed_component::<dynamics::Body>(b, dynamics::BODY)?
        .unwrap();
    assert_eq!(
        body.particle.position,
        loaded.entities[&b].local.w_axis.truncate()
    );
    let dz = body.particle.position.z - at.z;
    assert!(
        (0.0..0.05).contains(&dz) && dz > 0.,
        "first contact step moved {dz} m"
    );
    Ok(())
}

fn tip_vertex(w: &World, finger: Id) -> Result<Vec3> {
    let mut last: Option<Vec3> = None;
    for e in w.entities.values() {
        let (Some(i), Some(skin)) = (
            &e.instance,
            e.skin_binding.and_then(|id| w.entities[&id].skin.as_ref()),
        ) else {
            continue;
        };
        let mesh = w.entities[&i.mesh].mesh.as_ref().unwrap();
        for (k, joints) in mesh.joints.iter().enumerate() {
            for (j, &slot) in joints.iter().enumerate() {
                if mesh.weights[k][j] > 0.9999 && skin.joints[slot as usize] == finger {
                    let p =
                        skin.inverse_bind[slot as usize].transform_point3(mesh.positions[k].into());
                    if last.is_none_or(|v| p.y > v.y) {
                        last = Some(p);
                    }
                }
            }
        }
    }
    last.context("missing fully weighted finger vertex")
}

// Render inputs for vertices dominated by the selected skin joints. Evaluate their
// weighted bind-space positions against the final transforms, independently of IK.
struct SurfaceProbe(Vec<Vec<(Id, glam::Vec4)>>);
impl SurfaceProbe {
    fn bind(w: &World, joints: &[Id]) -> Self {
        let mut points = Vec::new();
        for e in w.entities.values() {
            let (Some(instance), Some(skin)) = (
                &e.instance,
                e.skin_binding.and_then(|id| w.entities[&id].skin.as_ref()),
            ) else {
                continue;
            };
            let mesh = w.entities[&instance.mesh].mesh.as_ref().unwrap();
            for (k, slots) in mesh.joints.iter().enumerate() {
                let weights = mesh.weights[k];
                let part: f32 = slots
                    .iter()
                    .zip(weights)
                    .filter(|(slot, _)| joints.contains(&skin.joints[**slot as usize]))
                    .map(|(_, v)| v)
                    .sum();
                if part < 0.75 {
                    continue;
                }
                let vertex = Vec3::from_array(mesh.positions[k]).extend(1.);
                points.push(
                    slots
                        .iter()
                        .zip(weights)
                        .filter(|(_, w)| *w > 0.)
                        .map(|(&slot, w)| {
                            (
                                skin.joints[slot as usize],
                                (skin.inverse_bind[slot as usize] * vertex) * w,
                            )
                        })
                        .collect(),
                );
            }
        }
        assert!(!points.is_empty());
        Self(points)
    }
    fn sample<'a>(
        &'a self,
        transforms: &'a std::collections::BTreeMap<Id, Mat4>,
    ) -> impl Iterator<Item = Vec3> + 'a {
        self.0.iter().map(|weights| {
            weights
                .iter()
                .fold(glam::Vec4::ZERO, |p, (id, v)| p + transforms[id] * *v)
                .truncate()
        })
    }
}
#[test]
fn walks_loads_plate_and_touches_with_the_skinned_vertex() -> Result<()> {
    let mut w = Arc::new(scene()?);
    let actor = actor(&w).unwrap();
    let rig = Rig::bind(&w, actor, &rig::children(&w))?;
    let fingertip = tip_vertex(&w, rig.ids[rig.finger])?;
    assert!(fingertip.distance(FINGERTIP) < 1e-5);
    let mut schedule = Schedule::default();
    install(&w, &mut schedule)?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let course = w
        .typed_component::<Contact>(actor, SETTING)?
        .unwrap()
        .course
        .unwrap();
    let head = w
        .entities
        .iter()
        .find(|(_, e)| e.name == "Head_08")
        .unwrap()
        .0
        .to_owned();
    let legs = SurfaceProbe::bind(
        &w,
        &rig.legs
            .iter()
            .flat_map(|leg| leg[1..].iter().map(|&i| rig.ids[i]))
            .collect::<Vec<_>>(),
    );
    let head_surface = SurfaceProbe::bind(&w, &[head]);
    let mut touch_errors = Vec::new();
    let (mut planted_error, mut peak_sink, mut planted_count) = (0f32, 0f32, 0usize);
    let (mut jump_height, mut vault_clear, mut duck_clear, mut palm_error) =
        (0f32, 1f32, 1f32, 0f32);
    let (mut flight, mut vaulted, mut braced, mut ducked) = (0, 0, 0, 0);
    let mut surfaces = std::collections::BTreeSet::new();
    let mut phases = Vec::new();
    let mut palm_impulse = 0f32;
    let mut walk = false;
    let mut previous_legs: Option<[[Vec3; 3]; 2]> = None;
    let mut full_step = [0f32; 3];
    let mut walking_step = [0f32; 3];
    let mut settled_step = 0f32;
    for frame in 0..1080 {
        schedule.tick(&mut w, 1. / 60., &pool)?;
        let c = w.typed_component::<Contact>(actor, SETTING)?.unwrap();
        let m = w.transforms()?;
        let root = m[&actor].w_axis.truncate();
        // Measure jumps in the actual rendered joint chain in character space.
        let to_local = m[&actor].inverse();
        let current = rig.legs.map(|[_, knee, ankle, toe]| {
            [knee, ankle, toe].map(|j| to_local.transform_point3(m[&rig.ids[j]].w_axis.truncate()))
        });
        if let Some(previous) = previous_legs {
            for foot in 0..2 {
                for j in 0..3 {
                    let step = current[foot][j].distance(previous[foot][j]);
                    full_step[j] = full_step[j].max(step);
                    if frame < 61 {
                        walking_step[j] = walking_step[j].max(step);
                    }
                    if frame > 840 {
                        settled_step = settled_step.max(step);
                    }
                }
            }
        }
        previous_legs = Some(current);
        let err = m[&rig.ids[rig.finger]]
            .transform_point3(fingertip)
            .distance(target_point(m[&c.target]));
        assert!((err - c.error_m).abs() < 1e-5);
        if frame >= 960 {
            touch_errors.push(err);
        }
        if let Some(play) = c.play {
            if phases.last() != Some(&c.section) {
                phases.push(c.section);
                println!("{:?} starts at {:.2}s", c.section, frame as f32 / 60.);
            }
            let feet = rig
                .legs
                .map(|leg| m[&rig.ids[leg[3]]].w_axis.truncate() - Vec3::Y * 0.024);
            match c.section {
                traversal::Section::Gap if (root.z - course::GAP_Z).abs() < 0.15 => {
                    jump_height = jump_height.max(feet[0].y.min(feet[1].y));
                    assert!(c.feet.iter().all(|f| f.anchor.is_none()));
                    flight += 1;
                }
                traversal::Section::Rail => {
                    palm_impulse = palm_impulse.max(
                        w.typed_component::<dynamics::Body>(actor, dynamics::BODY)?
                            .unwrap()
                            .palm_impulse,
                    );
                    let inverse = m[&course.rail].inverse();
                    for v in legs.sample(&m) {
                        let in_bar = inverse.transform_point3(v);
                        if in_bar.x.abs() <= 0.5 && in_bar.z.abs() <= 0.5 {
                            vault_clear = vault_clear.min(v.y - course::RAIL_TOP);
                            vaulted += 1;
                        }
                    }
                    if (13.5..=17.5).contains(&play.frame) {
                        let palm = m[&rig.ids[rig.arm[2]]].transform_point3(rig.palm_offset);
                        let top = m[&course.rail].transform_point3(Vec3::new(-0.18, 0.5, 0.34));
                        palm_error = palm_error.max(palm.distance(top));
                        let hand = m[&rig.ids[rig.arm[2]]];
                        let normal = hand
                            .transform_vector3(Vec3::new(0.51, 0., -0.86))
                            .normalize();
                        assert!(
                            normal.dot(-Vec3::Y) > 0.999 && hand.y_axis.y.abs() < 0.002,
                            "palm must lie flat during support"
                        );
                        braced += 1;
                    }
                }
                traversal::Section::Beam => {
                    let slab = m[&course.gate].inverse();
                    for v in head_surface.sample(&m) {
                        let p = slab.transform_point3(v);
                        if p.x.abs() <= 0.5 && p.z.abs() <= 0.5 {
                            let bottom = m[&course.gate].transform_point3(-0.5 * Vec3::Y);
                            duck_clear = duck_clear.min(bottom.y - v.y);
                            ducked += 1;
                        }
                    }
                }
                _ => {}
            }
        }
        for (foot, leg) in c.feet.iter().zip(rig.legs) {
            if let Some((id, anchor)) = foot.anchor
                && foot.weight >= 0.999
            {
                surfaces.insert(id);
                let actual = m[&rig.ids[leg[3]]].w_axis.truncate() - 0.024 * Vec3::Y;
                planted_error = planted_error.max(actual.distance(m[&id].transform_point3(anchor)));
                planted_count += 1;
            }
        }
        peak_sink = peak_sink.max(c.rest_y - m[&c.plate].w_axis.y);
        walk |= player(&w, actor)?.clip.contains("walk");
    }
    let settled = w.typed_component::<Contact>(actor, SETTING)?.unwrap();
    let equilibrium = settled.rest_y - w.entities[&settled.plate].local.w_axis.y;
    println!(
        "joint max per 60 Hz step knee/ankle/toe (cm): walk {:?}, full {:?}; settled max {:.3} mm; plate resting {:.2} mm",
        walking_step.map(|v| 100. * v),
        full_step.map(|v| 100. * v),
        settled_step * 1000.,
        equilibrium * 1000.
    );
    assert!((equilibrium - 48. * 9.81 / 7200.).abs() < 0.001);
    assert!(
        walking_step.iter().all(|&v| v < 0.10)
            && full_step.iter().all(|&v| v < 0.15)
            && settled_step < 0.001
    );
    touch_errors.sort_by(f32::total_cmp);
    println!(
        "touch P95 {:.4} mm, jump sole peak {:.2} m ({}), vault lower-leg mesh clearance {:.3} m ({}), palm max {:.4} mm ({}), duck head mesh clearance {:.3} m ({}), spring {:.1} mm, foot max {:.2} mm ({})",
        touch_errors[113] * 1000.,
        jump_height,
        flight,
        vault_clear,
        vaulted,
        palm_error * 1000.,
        braced,
        duck_clear,
        ducked,
        peak_sink * 1000.,
        planted_error * 1000.,
        planted_count
    );
    assert_eq!(
        phases,
        [
            traversal::Section::Gap,
            traversal::Section::Rail,
            traversal::Section::Beam
        ]
    );
    assert!(
        jump_height > 0.25
            && flight > 3
            && vaulted > 0
            && vault_clear > 0.
            && braced > 8
            && palm_impulse > 1.
            && palm_error < 0.004
            && ducked > 3
            && duck_clear > 0.03
    );
    assert!(
        walk && surfaces.contains(&course.ramp)
            && surfaces.contains(&course.landing)
            && peak_sink > 0.045
    );
    assert!(planted_error < 0.012 && touch_errors.last().unwrap() < &0.001);
    // Fork the same settled animation to isolate the two end-effector corrections.
    let mut uncorrected = w.clone();
    let mut comparison = Schedule::default();
    install(&uncorrected, &mut comparison)?;
    let mut controls = uncorrected
        .typed_component::<Contact>(actor, SETTING)?
        .unwrap()
        .into_owned();
    controls.foot_ik = false;
    controls.hand_ik = false;
    Arc::make_mut(&mut uncorrected).set_typed_component(actor, SETTING, controls);
    for _ in 0..36 {
        schedule.tick(&mut w, 1. / 60., &pool)?;
        comparison.tick(&mut uncorrected, 1. / 60., &pool)?;
    }
    let mut paired = Vec::new();
    for sample in [&uncorrected, &w] {
        let c = sample.typed_component::<Contact>(actor, SETTING)?.unwrap();
        let m = sample.transforms()?;
        let foot = c.feet[0]
            .anchor
            .context("expected a supported foot at rest")?;
        let sole = m[&rig.ids[rig.legs[0][3]]].w_axis.truncate() - 0.024 * Vec3::Y;
        paired.push((
            m[&rig.ids[rig.finger]]
                .transform_point3(fingertip)
                .distance(target_point(m[&c.target])),
            sole.distance(m[&foot.0].transform_point3(foot.1)),
        ));
    }
    println!(
        "same pose mm: off hand={:.3} foot={:.3}; on hand={:.4} foot={:.4}",
        paired[0].0 * 1000.,
        paired[0].1 * 1000.,
        paired[1].0 * 1000.,
        paired[1].1 * 1000.
    );
    assert_eq!(
        player(&w, actor)?.sample,
        player(&uncorrected, actor)?.sample
    );
    assert!(
        paired[0].0 > 0.15
            && paired[0].1 > paired[1].1 + 0.01
            && paired[1].0 < 0.001
            && paired[1].1 < 0.001
    );
    // Move the marker while holding still, then put it outside the physical arm span.
    let mut c = w
        .typed_component::<Contact>(actor, SETTING)?
        .unwrap()
        .into_owned();
    c.auto_walk = false;
    Arc::make_mut(&mut w).set_typed_component(actor, SETTING, c.clone());
    let target = c.target;
    for (dy, reachable) in [(0.09, true), (1.5, false)] {
        Arc::make_mut(&mut w)
            .entities
            .get_mut(&target)
            .unwrap()
            .local
            .w_axis
            .y += dy;
        for _ in 0..36 {
            schedule.tick(&mut w, 1. / 60., &pool)?;
        }
        let m = w.transforms()?;
        let err = m[&rig.ids[rig.finger]]
            .transform_point3(fingertip)
            .distance(target_point(m[&target]));
        assert!(if reachable { err < 0.001 } else { err > 0.5 });
        let length = |i: usize, j: usize| m[&rig.ids[i]].w_axis.distance(m[&rig.ids[j]].w_axis);
        assert!((length(rig.arm[0], rig.arm[1]) - 0.22576).abs() < 0.001);
        assert!((length(rig.arm[1], rig.arm[2]) - 0.20828).abs() < 0.001);
    }
    Ok(())
}

#[test]
fn saved_contact_substep_continues_with_same_load_and_pose() -> Result<()> {
    let mut world = Arc::new(scene()?);
    let id = actor(&world).unwrap();
    let mut schedule = Schedule::default();
    install(&world, &mut schedule)?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    for _ in 0..217 {
        schedule.tick(&mut world, 1. / 60., &pool)?;
    }
    let saved = world.typed_component::<Contact>(id, SETTING)?.unwrap();
    assert!(saved.section == traversal::Section::Rail && saved.play.is_some());
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("silver.scene.json");
    world.save(&path)?;
    let mut loaded = World::load(&path)?;
    crate::experiments::SceneConfiguration::restore(&mut loaded)?;
    let mut replay = Schedule::default();
    install(&loaded, &mut replay)?;
    let mut loaded = Arc::new(loaded);
    for _ in 0..24 {
        schedule.tick(&mut world, 1. / 60., &pool)?;
        replay.tick(&mut loaded, 1. / 30., &pool)?;
        schedule.tick(&mut world, 1. / 60., &pool)?;
    }
    let dynamics = |w: &World| -> Result<dynamics::Body> {
        Ok(w.typed_component::<dynamics::Body>(id, dynamics::BODY)?
            .unwrap()
            .into_owned())
    };
    let (live_body, saved_body) = (dynamics(&world)?, dynamics(&loaded)?);
    assert_eq!(live_body.support, saved_body.support);
    assert!(
        live_body
            .particle
            .momentum
            .abs_diff_eq(saved_body.particle.momentum, 1e-5)
    );
    assert!((live_body.palm_impulse - saved_body.palm_impulse).abs() < 1e-5);
    let rig = Rig::bind(&world, id, &rig::children(&world))?;
    let a = world.transforms()?;
    let b = loaded.transforms()?;
    for joint in rig.ids {
        assert!(a[&joint].abs_diff_eq(b[&joint], 2e-5));
    }
    let plate = world
        .typed_component::<Contact>(id, SETTING)?
        .unwrap()
        .plate;
    assert!(a[&plate].abs_diff_eq(b[&plate], 1e-6));
    Ok(())
}

#[test]
fn practice_plays_each_requested_action_once_and_releases_the_root() -> Result<()> {
    use traversal::Section::{Beam, Gap, Rail};
    let mut world = Arc::new(scene()?);
    let actor = actor(&world).unwrap();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut schedule = Schedule::default();
    install(&world, &mut schedule)?;
    for section in [Beam, Gap, Rail] {
        let w = Arc::make_mut(&mut world);
        let mut c = w
            .typed_component::<Contact>(actor, SETTING)?
            .unwrap()
            .into_owned();
        section.practice(w, actor, &mut c)?;
        w.set_typed_component(actor, SETTING, c);
        let start = w.entities[&actor].local.w_axis;
        for _ in 0..140 {
            schedule.tick(&mut world, 1. / 60., &pool)?;
        }
        let state = world.typed_component::<Contact>(actor, SETTING)?.unwrap();
        assert!(!state.auto_walk && state.play.is_none() && state.section != section);
        let end = world.entities[&actor].local.w_axis;
        assert!(end.z - start.z > 1.25);
        schedule.tick(&mut world, 1. / 60., &pool)?;
        assert!(end.abs_diff_eq(world.entities[&actor].local.w_axis, 0.0001));
    }
    Ok(())
}

#[test]
fn jump_height_time_and_range_respond_to_gravity_and_impulse() -> Result<()> {
    let initial = scene()?;
    let actor = actor(&initial).unwrap();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    for (g, j) in [(9.81, 137.34), (7., 137.34), (9.81, 160.)] {
        let mut world = initial.clone();
        let mut c = world
            .typed_component::<Contact>(actor, SETTING)?
            .unwrap()
            .into_owned();
        traversal::Section::Gap.practice(&mut world, actor, &mut c)?;
        let landing = c.course.unwrap().landing;
        world.set_typed_component(actor, SETTING, c);
        let mut b = world
            .typed_component::<dynamics::Body>(actor, dynamics::BODY)?
            .unwrap()
            .into_owned();
        b.gravity = g;
        b.jump_impulse = j;
        let mass = b.particle.mass;
        world.set_typed_component(actor, dynamics::BODY, b);
        let mut w = Arc::new(world);
        let mut schedule = Schedule::default();
        install(&w, &mut schedule)?;
        let dt = 1. / 60.;
        let (mut apex, mut duration, mut vz, mut launch_z) = (0f32, 0f32, 0f32, 0f32);
        let mut previous = Vec3::ZERO;
        let mut count = 0;
        for _ in 0..132 {
            let start = w.entities[&actor].local.w_axis.truncate();
            schedule.tick(&mut w, dt, &pool)?;
            let body = w
                .typed_component::<dynamics::Body>(actor, dynamics::BODY)?
                .unwrap();
            let c = w.typed_component::<Contact>(actor, SETTING)?.unwrap();
            if c.play.is_some_and(|p| p.launched && !p.landed) {
                assert!(body.support.is_none());
                if count == 0 {
                    launch_z = start.z;
                    vz = body.particle.velocity().z;
                } else {
                    assert!(
                        body.particle
                            .momentum
                            .abs_diff_eq(previous - Vec3::Y * mass * g * dt, 0.001)
                    );
                }
                apex = apex.max(body.particle.position.y);
                previous = body.particle.momentum;
                count += 1;
            } else if count > 0 {
                assert_eq!(body.support, Some(landing));
                duration = body.flight_time;
                let range = body.particle.position.z - launch_z;
                assert!((range - vz * duration).abs() < vz * dt + 0.001);
                assert!(body.landing_impulse > 0.);
                println!(
                    "gravity {g}, impulse {j}: apex {apex:.4} m, flight {duration:.4} s, forward {range:.4} m"
                );
                break;
            }
        }
        assert!((apex - (j / mass).powi(2) / (2. * g)).abs() < 0.002);
        assert!((duration - 2. * j / (mass * g)).abs() < 0.002);
    }
    Ok(())
}

#[test]
fn a_missed_jump_hits_the_lower_floor_without_an_animation_event() -> Result<()> {
    let mut world = scene()?;
    let actor = actor(&world).unwrap();
    let mut c = world
        .typed_component::<Contact>(actor, SETTING)?
        .unwrap()
        .into_owned();
    let service = c.course.unwrap().service.unwrap();
    c.auto_walk = false;
    world.set_typed_component(actor, SETTING, c);
    let at = Vec3::new(0., 0., course::GAP_Z);
    world.set_local(actor, Mat4::from_translation(at))?;
    world.set_typed_component(actor, dynamics::BODY, dynamics::Body::new(at));
    let mut w = Arc::new(world);
    let mut schedule = Schedule::default();
    install(&w, &mut schedule)?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    for _ in 0..60 {
        schedule.tick(&mut w, 1. / 60., &pool)?;
    }
    let b = w
        .typed_component::<dynamics::Body>(actor, dynamics::BODY)?
        .unwrap();
    assert_eq!(b.support, Some(service));
    let height = w.entities[&service].local.transform_point3(0.5 * Vec3::Y).y;
    assert!((b.particle.position.y - height).abs() < 0.001);
    assert!((b.flight_time - (-2. * height / b.gravity).sqrt()).abs() < 0.002);
    assert!(b.landing_impulse > 100.);
    Ok(())
}

#[test]
fn translating_deck_carries_the_root_and_takeoff_inherits_its_velocity() -> Result<()> {
    let mut world = scene()?;
    let id = actor(&world).unwrap();
    let mut c = world
        .typed_component::<Contact>(id, SETTING)?
        .unwrap()
        .into_owned();
    traversal::Section::Gap.practice(&mut world, id, &mut c)?;
    let floor = c.floor;
    world.set_typed_component(id, SETTING, c);
    let start = world.entities[&id].local.w_axis.x;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut schedule = Schedule::default();
    install(&world, &mut schedule)?;
    let mut w = Arc::new(world);
    let mut airborne_steps = 0;
    for step in 0..32 {
        if step > 0 {
            let mut deck = w.entities[&floor].local;
            deck.w_axis.x += 0.01;
            Arc::make_mut(&mut w).set_local(floor, deck)?;
        }
        schedule.tick(&mut w, 1. / 60., &pool)?;
        let b = w
            .typed_component::<dynamics::Body>(id, dynamics::BODY)?
            .unwrap();
        if b.support.is_none() {
            airborne_steps += 1;
            assert!((b.particle.velocity().x - 0.6).abs() < 0.002);
        } else if step > 0 {
            assert!((b.particle.position.x - start - 0.01 * step as f32).abs() < 0.002);
        }
    }
    assert!(airborne_steps > 5);
    Ok(())
}

#[test]
fn rotating_deck_transfers_contact_point_velocity_and_replays_across_save_and_cadence() -> Result<()>
{
    let mut world = scene()?;
    let id = actor(&world).unwrap();
    let mut contact = world
        .typed_component::<Contact>(id, SETTING)?
        .unwrap()
        .into_owned();
    traversal::Section::Gap.practice(&mut world, id, &mut contact)?;
    let floor = contact.floor;
    let axis_center = world.entities[&floor].local.w_axis.truncate();
    let rate = 0.8;
    world.set_typed_component(floor, dynamics::TURNTABLE, rate);
    world.set_typed_component(id, SETTING, contact);
    let one = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
    let four = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let mut s = Schedule::default();
    install(&world, &mut s)?;
    let mut w = Arc::new(world);
    for _ in 0..4 {
        s.tick(&mut w, 1. / 60., &one)?;
    }
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("rotating.json");
    w.save(&path)?;
    let mut resumed_world = World::load(&path)?;
    crate::experiments::SceneConfiguration::restore(&mut resumed_world)?;
    let mut resumed_s = Schedule::default();
    install(&resumed_world, &mut resumed_s)?;
    let mut resumed = Arc::new(resumed_world);
    let mut launch = None;
    for _ in 0..16 {
        for _ in 0..2 {
            let before = w
                .typed_component::<dynamics::Body>(id, dynamics::BODY)?
                .unwrap();
            let grounded = before.support == Some(floor);
            let root = before.particle.position;
            s.tick(&mut w, 1. / 60., &one)?;
            let after = w
                .typed_component::<dynamics::Body>(id, dynamics::BODY)?
                .unwrap();
            if grounded && after.support.is_none() {
                let expected_x = rate * (root.z - axis_center.z);
                let actual_x = after.particle.velocity().x;
                assert!(expected_x.abs() > 0.1);
                assert!(
                    (actual_x - expected_x).abs() < 0.002,
                    "takeoff x: {actual_x}, expected {expected_x}"
                );
                launch = Some((actual_x, expected_x));
            }
        }
        resumed_s.tick(&mut resumed, 1. / 30., &four)?;
        let a = w
            .typed_component::<dynamics::Body>(id, dynamics::BODY)?
            .unwrap();
        let b = resumed
            .typed_component::<dynamics::Body>(id, dynamics::BODY)?
            .unwrap();
        assert_eq!(a.support, b.support);
        assert!(a.particle.position.abs_diff_eq(b.particle.position, 2e-5));
        assert!(a.particle.momentum.abs_diff_eq(b.particle.momentum, 2e-4));
        assert!(
            w.entities[&floor]
                .local
                .abs_diff_eq(resumed.entities[&floor].local, 2e-5)
        );
        for &joint in &player(&w, id)?.bones {
            assert!(
                w.entities[&joint]
                    .local
                    .abs_diff_eq(resumed.entities[&joint].local, 2e-5)
            );
        }
    }
    let (actual, expected) = launch.context("no takeoff from the driven deck")?;
    println!("rotating deck takeoff x: actual {actual:.6}, expected {expected:.6} m/s");
    Ok(())
}

#[test]
fn replacing_a_live_rig_discards_stale_sources_and_restores_after_save() -> Result<()> {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build()?;
    let mut world = Arc::new(scene()?);
    let id = actor(&world).unwrap();
    let mut schedule = Schedule::default();
    install(&world, &mut schedule)?;
    schedule.tick(&mut world, 1. / 60., &pool)?;
    let w = Arc::make_mut(&mut world);
    let mut p = player(w, id)?.into_owned();
    let mut contact = w
        .typed_component::<Contact>(id, SETTING)?
        .unwrap()
        .into_owned();
    let rig = Rig::bind(w, id, &rig::children(w))?;
    // These finite but incorrect previous transforms make unwanted scale reuse observable.
    Arc::make_mut(&mut contact.source).fill(Mat4::from_scale(Vec3::splat(7.)));
    let old_head = p.bones[4];
    let expected_scale = w.entities[&old_head]
        .local
        .to_scale_rotation_translation()
        .0;
    let replacement = w.spawn(Entity {
        name: "Retargeted head".into(),
        ..w.entities[&old_head].clone()
    });
    p.bones[4] = replacement;
    w.set_typed_component(id, PLAYER, p);
    contact.finger = Some(FingerMarker {
        joint: id,
        offset: FINGERTIP,
    });
    w.set_typed_component(id, SETTING, contact.clone());
    assert!(
        schedule
            .tick(&mut world, 1. / 60., &pool)
            .unwrap_err()
            .to_string()
            .contains("fingertip")
    );
    contact.finger = Some(FingerMarker {
        joint: rig.ids[rig.finger],
        offset: FINGERTIP + Vec3::Y * 0.001,
    });
    Arc::make_mut(&mut world).set_typed_component(id, SETTING, contact);
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("edited-rig.scene.json");
    world.save(&path)?;
    let mut restored = World::load(&path)?;
    crate::experiments::SceneConfiguration::restore(&mut restored)?;
    let mut replay = Schedule::default();
    install(&restored, &mut replay)?;
    let mut restored = Arc::new(restored);
    // A failed substep retains its pending time.
    schedule.tick(&mut world, 0., &pool)?;
    replay.tick(&mut restored, 1. / 60., &pool)?;
    for result in [&*world, &*restored] {
        let c = result.typed_component::<Contact>(id, SETTING)?.unwrap();
        assert_eq!(c.source_binding.as_ref().unwrap().bones[4], replacement);
        let scale = result.entities[&replacement]
            .local
            .to_scale_rotation_translation()
            .0;
        assert!(
            scale.abs_diff_eq(expected_scale, 1e-5),
            "stale base pose: {scale}"
        );
    }
    assert!(
        world.entities[&replacement]
            .local
            .abs_diff_eq(restored.entities[&replacement].local, 1e-5)
    );
    // An authored marker change with identical actors, bone IDs, hierarchy and component ownership also rebinds.
    let mut c = world
        .typed_component::<Contact>(id, SETTING)?
        .unwrap()
        .into_owned();
    c.finger.as_mut().unwrap().offset.y += 0.01;
    let offset = c.finger.as_ref().unwrap().offset;
    Arc::make_mut(&mut world).set_typed_component(id, SETTING, c);
    schedule.tick(&mut world, 1. / 60., &pool)?;
    let c = world.typed_component::<Contact>(id, SETTING)?.unwrap();
    assert_eq!(
        c.source_binding
            .as_ref()
            .unwrap()
            .finger
            .as_ref()
            .unwrap()
            .offset,
        offset
    );
    Ok(())
}

#[test]
fn fixed_kinematic_and_dynamic_roots_pass_actual_contacts_to_ik() -> Result<()> {
    let mut world = scene()?;
    let id = actor(&world).unwrap();
    let mut c = world
        .typed_component::<Contact>(id, SETTING)?
        .unwrap()
        .into_owned();
    c.auto_walk = false;
    let (floor, plate) = (c.floor, c.plate);
    world.set_typed_component(id, SETTING, c);
    set_actor_goal(&mut world, id, 0, 1)?;
    world.set_motion_policy(id, MotionPolicy::FIXED_COLLIDER)?;
    world.set_motion_policy(plate, MotionPolicy::FIXED_COLLIDER)?;
    world.set_motion_policy(floor, MotionPolicy::FIXED_COLLIDER)?;
    world.set_typed_component(floor, dynamics::TURNTABLE, 1_f32);
    let (actor_start, plate_start, floor_start) = (
        world.entities[&id].local,
        world.entities[&plate].local,
        world.entities[&floor].local,
    );
    let hip = player(&world, id)?.bones[0];
    let hip_bind = world.entities[&hip].local;
    let mut schedule = Schedule::default();
    install(&world, &mut schedule)?;
    let workers = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut w = Arc::new(world);
    for _ in 0..24 {
        schedule.tick(&mut w, 1. / 60., &workers)?;
    }
    assert_eq!(w.entities[&id].local, actor_start);
    assert_ne!(w.entities[&hip].local, hip_bind);
    assert_eq!(w.entities[&floor].local, floor_start);
    assert_eq!(w.entities[&plate].local, plate_start);
    assert_eq!(
        w.typed_component::<dynamics::Body>(id, dynamics::BODY)?
            .unwrap()
            .support,
        Some(floor)
    );
    Arc::make_mut(&mut w).set_motion_policy(id, MotionPolicy::KINEMATIC)?;
    for _ in 0..24 {
        schedule.tick(&mut w, 1. / 60., &workers)?;
    }
    assert!(w.entities[&id].local.w_axis.z > actor_start.w_axis.z + 0.1);
    let body = w
        .typed_component::<dynamics::Body>(id, dynamics::BODY)?
        .unwrap();
    assert!(
        w.typed_component::<ResolvedMotion>(id, RESOLVED)?
            .unwrap()
            .surfaces
            .is_empty()
            && body.support.is_none()
    );
    assert_eq!(
        body.particle.position,
        w.entities[&id].local.w_axis.truncate()
    );
    Arc::make_mut(&mut w).set_motion_policy(id, MotionPolicy::MOVING_COLLIDER)?;
    schedule.tick(&mut w, 1. / 60., &workers)?;
    assert_eq!(
        w.typed_component::<dynamics::Body>(id, dynamics::BODY)?
            .unwrap()
            .support,
        Some(floor)
    );
    Arc::make_mut(&mut w).set_motion_policy(id, MotionPolicy::DYNAMIC)?;
    Arc::make_mut(&mut w).set_motion_policy(floor, MotionPolicy::STATIC)?;
    let from = w.entities[&id].local.w_axis.y;
    for _ in 0..4 {
        schedule.tick(&mut w, 1. / 60., &workers)?;
    }
    let body = w
        .typed_component::<dynamics::Body>(id, dynamics::BODY)?
        .unwrap();
    assert!(body.support.is_none() && body.particle.position.y < from);
    assert!(
        w.typed_component::<ResolvedMotion>(id, RESOLVED)?
            .unwrap()
            .surfaces
            .iter()
            .all(|face| face.id != floor)
    );
    assert_eq!(
        body.particle.position,
        w.entities[&id].local.w_axis.truncate()
    );
    assert_eq!(
        w.typed_component::<ProposedMotion>(id, PROPOSED)?
            .unwrap()
            .step,
        w.typed_component::<ResolvedMotion>(id, RESOLVED)?
            .unwrap()
            .step
    );
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("modes.json");
    w.save(&file)?;
    let restored = World::load(&file)?;
    assert_eq!(
        restored.motion_policy(id, MotionPolicy::STATIC)?,
        MotionPolicy::DYNAMIC
    );
    assert_eq!(
        restored.motion_policy(floor, MotionPolicy::FIXED_COLLIDER)?,
        MotionPolicy::STATIC
    );
    // A pre-policy save has no policy on any root or fixture, and regenerates the request.
    let mut legacy = restored;
    for owned in legacy.entities.component_owners(MOTION_POLICY).to_vec() {
        legacy
            .entities
            .get_mut(&owned)
            .unwrap()
            .components
            .remove(MOTION_POLICY);
    }
    legacy.save(&file)?;
    let mut legacy = World::load(&file)?;
    hydrate_contact(&mut legacy)?;
    let mut replay = Schedule::default();
    install(&legacy, &mut replay)?;
    let mut legacy = Arc::new(legacy);
    let y = legacy.entities[&id].local.w_axis.y;
    replay.tick(&mut legacy, 1. / 60., &workers)?;
    let b = legacy
        .typed_component::<dynamics::Body>(id, dynamics::BODY)?
        .unwrap();
    assert!(b.particle.position.y < y);
    assert_eq!(
        b.particle.position,
        legacy.entities[&id].local.w_axis.truncate()
    );
    Ok(())
}

#[test]
fn rotating_deck_restores_a_low_frequency_clock_without_reversing_its_drive() -> Result<()> {
    use crate::engine::TickRate;
    let mut world = scene()?;
    let actor = actor(&world).unwrap();
    let floor = world
        .typed_component::<Contact>(actor, SETTING)?
        .unwrap()
        .floor;
    let speed = -1.75 * std::f32::consts::TAU;
    world.set_typed_component(floor, dynamics::TURNTABLE, speed);
    let mut original = Schedule::default();
    install(&world, &mut original)?;
    original.set_tick_rate(character_clock().name, TickRate::Hertz(1))?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
    let mut world = Arc::new(world);
    original.tick(&mut world, 0.75, &pool)?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("low-frequency.json");
    world.save_with_clocks(&path, &original.checkpoints(&world))?;
    let mut loaded = World::load(&path)?;
    crate::experiments::SceneConfiguration::restore(&mut loaded)?;
    let mut resumed = Schedule::default();
    install(&loaded, &mut resumed)?;
    resumed.restore_clocks(&loaded)?;
    assert_eq!(resumed.fixed_seconds(character_clock().name), Some(1.));
    let mut loaded = Arc::new(loaded);
    let initial = loaded.entities[&floor].local;
    resumed.tick(&mut loaded, 0.25, &pool)?;
    assert_ne!(initial, loaded.entities[&floor].local);
    let resolved = loaded
        .typed_component::<ResolvedMotion>(actor, RESOLVED)?
        .unwrap();
    let face = resolved
        .surfaces
        .iter()
        .find(|f| f.id == floor)
        .context("missing driven support")?;
    assert_eq!(face.angular_velocity, Vec3::Y * speed);
    let before = loaded.entities[&floor].local;
    Arc::make_mut(&mut loaded).set_typed_component(floor, dynamics::TURNTABLE, f32::INFINITY);
    assert!(resumed.tick(&mut loaded, 1., &pool).is_err());
    assert_eq!(loaded.entities[&floor].local, before);
    Arc::make_mut(&mut loaded).set_typed_component(floor, dynamics::TURNTABLE, 0_f32);
    resumed.tick(&mut loaded, 0., &pool)?;
    let clock = resumed.checkpoints(&loaded)[character_clock().name];
    assert_eq!(clock.completed, 2);
    assert_eq!(clock.pending_seconds, 0.);
    Ok(())
}

#[test]
fn legacy_spring_is_restored_once_and_a_missing_plate_is_rejected() -> Result<()> {
    let world = scene()?;
    let actor = actor(&world).unwrap();
    let plate = world
        .typed_component::<Contact>(actor, SETTING)?
        .unwrap()
        .plate;
    let mut raw = serde_json::to_value(world)?;
    let actors = &mut raw["entities"];
    let contact = actors[actor.to_string()]["components"]
        .as_object_mut()
        .unwrap();
    contact.remove(dynamics::BODY);
    contact.get_mut(SETTING).unwrap()["velocity"] = serde_json::json!(0.75);
    actors[plate.to_string()]["components"]
        .as_object_mut()
        .unwrap()
        .remove(dynamics::SPRING);
    let mut legacy: World = serde_json::from_value(raw.clone())?;
    hydrate_contact(&mut legacy)?;
    assert_eq!(
        legacy
            .typed_component::<dynamics::Spring>(plate, dynamics::SPRING)?
            .unwrap()
            .velocity,
        0.75
    );
    assert!(
        legacy
            .typed_component::<dynamics::Body>(actor, dynamics::BODY)?
            .is_some()
    );
    let mut migrated: World = serde_json::from_value(serde_json::to_value(legacy)?)?;
    hydrate_contact(&mut migrated)?;
    assert_eq!(
        migrated
            .typed_component::<dynamics::Spring>(plate, dynamics::SPRING)?
            .unwrap()
            .velocity,
        0.75
    );
    raw["entities"]
        .as_object_mut()
        .unwrap()
        .remove(&plate.to_string());
    let mut broken: World = serde_json::from_value(raw)?;
    assert!(
        hydrate_contact(&mut broken)
            .unwrap_err()
            .to_string()
            .contains("missing contact plate")
    );
    Ok(())
}
