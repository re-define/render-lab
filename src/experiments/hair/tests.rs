use super::*;
use crate::experiments::assembly::SceneConfiguration;

#[test]
fn moving_roots_hold_and_free_points_stay_outside_the_proxies() {
    let controls = Controls::default();
    let mut stretch = 0_f32;
    let mut penetration = 0_f32;
    let mut lag = 0_f32;
    for i in 0..GUIDES {
        let g = solver::groom(i);
        assert!((g.rest[0].distance(solver::CENTER) - solver::HEAD - 0.001).abs() < 1e-5);
        let mut s = g.initial();
        for step in 1..=360 {
            let t = step as f32 / HZ as f32;
            let head = solver::head_frame(2.6 * t);
            g.advance(&mut s, head, controls, t, 1. / HZ as f32);
            for j in 0..2 {
                assert_eq!(s.points[j], head.transform_point3(g.rest[j]));
            }
            assert!(s.points.iter().all(|p| p.is_finite()));
            for &p in &s.points[2..] {
                penetration = penetration.max(
                    solver::HEAD + solver::MARGIN
                        - p.distance(head.transform_point3(solver::CENTER)),
                );
                penetration = penetration.max(
                    solver::SHOULDER_RADIUS + solver::MARGIN - p.distance(solver::shoulder_axis(p)),
                );
            }
            stretch = stretch.max(g.max_stretch(&s));
            lag = lag.max(s.points[POINTS - 1].distance(head.transform_point3(g.rest[POINTS - 1])));
        }
    }
    println!(
        "360 steps: maximum stretch {:.2}%, contact penetration {:.5} mm, tip displacement {:.2} cm",
        100. * stretch,
        1000. * penetration,
        100. * lag
    );
    assert!(stretch < 0.045, "peak stretch {stretch}");
    assert!(penetration < 0.0001 && lag > 0.04);
}

#[test]
fn reversing_wind_changes_tip_displacement_and_stiffness_restores_shape() {
    let run = |wind, stiffness| {
        let mut sum = Vec3::ZERO;
        for i in (0..GUIDES).step_by(4) {
            let g = solver::groom(i);
            let mut s = g.initial();
            for step in 1..=360 {
                g.advance(
                    &mut s,
                    Mat4::IDENTITY,
                    Controls {
                        wind,
                        stiffness,
                        motion: false,
                    },
                    step as f32 / HZ as f32,
                    1. / HZ as f32,
                );
                if step > 300 {
                    sum += s.points[POINTS - 1] - g.rest[POINTS - 1];
                }
            }
        }
        sum / (60 * (GUIDES / 4)) as f32
    };
    let left = run(-10., 0.5);
    let right = run(10., 0.5);
    let stiff = run(10., 1.);
    println!(
        "mean tip x: left {:.2}, right {:.2}, stiff {:.2} cm",
        100. * left.x,
        100. * right.x,
        100. * stiff.x
    );
    assert!(left.x < -0.01 && right.x > 0.01 && stiff.x < right.x);
}

#[test]
fn reload_and_different_outer_cadences_preserve_pose_and_history() -> Result<()> {
    let mut w = scene()?;
    let owner = w.first_raw_component(SETTING).unwrap().0;
    let binding = Binding::new(&w, owner)?;
    command(&mut w, "view guides")?;
    let hidden = super::super::composition::HIDDEN;
    assert!(
        !w.entities[&binding.mount.display]
            .components
            .contains_key(hidden)
    );
    assert!(
        binding
            .mount
            .followers
            .iter()
            .all(|id| w.entities[id].components.contains_key(hidden))
    );
    command(&mut w, "wind -8")?;
    assert!(command(&mut w, "stiffness NaN").is_err());
    let mut a = Schedule::default();
    install(&w, &mut a)?;
    let one = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
    let four = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let mut w = Arc::new(w);
    // Checkpoint between fixed ticks so the serialized clock carries a real remainder.
    a.tick(&mut w, 1. / 50., &one)?;
    let path = tempfile::tempdir()?;
    let save = path.path().join("groom.json");
    Arc::make_mut(&mut w).clocks = a.checkpoints(&w);
    w.save(&save)?;
    let mut loaded = World::load(&save)?;
    SceneConfiguration::restore(&mut loaded)?;
    assert!(
        !loaded.entities[&binding.mount.display]
            .components
            .contains_key(hidden)
    );
    command(&mut loaded, "view strands")?;
    assert!(
        loaded.entities[&binding.mount.display]
            .components
            .contains_key(hidden)
    );
    assert!(
        binding
            .mount
            .followers
            .iter()
            .all(|id| !loaded.entities[id].components.contains_key(hidden))
    );
    let mut b = Schedule::default();
    install(&loaded, &mut b)?;
    b.restore_clocks(&loaded)?;
    let mut loaded = Arc::new(loaded);
    for _ in 0..12 {
        a.tick(&mut w, 1. / 30., &one)?;
        for _ in 0..4 {
            b.tick(&mut loaded, 1. / 120., &four)?;
        }
    }
    let state = |w: &World| -> Result<Dynamics> {
        Ok(w.typed_component::<Dynamics>(owner, STATE)?
            .unwrap()
            .into_owned())
    };
    let left = state(&w)?;
    let right = state(&loaded)?;
    assert_eq!(left.tick, right.tick);
    for tick in 1..=left.tick {
        let (a, ca, ta) = left.sample(tick);
        let (b, cb, tb) = right.sample(tick);
        assert_eq!(a, b);
        assert!(ca == cb);
        assert_eq!(ta, tb);
    }
    assert_ne!(left.generation, right.generation);
    command(Arc::make_mut(&mut loaded), "reset")?;
    assert_eq!(state(&loaded)?.tick, 0);
    assert_eq!(loaded.entities[&binding.mount.head].local, Mat4::IDENTITY);
    Ok(())
}

#[test]
#[ignore = "release timing of the hair scene simulation and pose commit"]
fn profile_groom_tick() -> Result<()> {
    let w = scene()?;
    let owner = w.first_raw_component(SETTING).unwrap().0;
    let mut schedule = Schedule::default();
    install(&w, &mut schedule)?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(8).build()?;
    let mut w = Arc::new(w);
    let mut samples = Vec::new();
    for i in 0..300 {
        let start = std::time::Instant::now();
        schedule.tick(&mut w, 1. / 60., &pool)?;
        let elapsed = start.elapsed().as_secs_f64() * 1000.;
        if i >= 60 {
            samples.push(elapsed);
        }
    }
    samples.sort_by(f64::total_cmp);
    let state = w.typed_component::<Dynamics>(owner, STATE)?.unwrap();
    assert!((state.time_at(state.tick) - 5.).abs() < 1. / HZ);
    println!(
        "64 guides, 896 points, two 120 Hz substeps, 8 workers, 240 samples: tick P50 {:.3} ms P95 {:.3} ms",
        samples[120], samples[227]
    );
    Ok(())
}

#[test]
#[ignore = "requires Vulkan descriptor heaps and timestamps"]
fn vulkan_particles_and_display_palettes_match_reference() -> Result<()> {
    use crate::rdg::{Access, Dispatch, Op, Resource, Shader};
    use crate::render::{Forward, FrameGraph, MaterialPrograms, SceneExtractor, Technique, post};
    let mut world = scene()?;
    let id = world.first_raw_component(SETTING).unwrap().0;
    let mount = *world.typed_component::<Mount>(id, SETTING)?.unwrap();
    let mut state = Dynamics::default();
    let guides: Vec<_> = (0..GUIDES).map(solver::groom).collect();
    let mut reference: Vec<_> = guides.iter().map(|g| g.initial()).collect();
    let mut backend = crate::rhi::Backend::open()?;
    let workers = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut seq = post::Sequence::default();
    let mut extract = SceneExtractor::default();
    let tech: Arc<dyn Technique> = Arc::new(Forward::default());
    let materials = Arc::new(MaterialPrograms::default());
    let n = GUIDES * POINTS;
    let mut worst = [0_f32; 4];
    let mut peak_stretch = 0_f32;
    let mut times = vec![];
    for frame in 0..125 {
        let cold = matches!(frame, 0 | 60 | 90 | 110);
        if frame == 60 {
            seq = Default::default();
        }
        if frame == 90 {
            state = Dynamics::default();
            reference = guides.iter().map(|g| g.initial()).collect();
        }
        if frame == 110 {
            // Deserialize a running CPU-era checkpoint, including the previous Verlet positions.
            state = serde_json::from_value(serde_json::json!({
                "time":state.time_at(state.tick),"phase":state.phase_at(state.tick),
                "remainder":0.0,"strands":reference,
            }))?;
            state.validate()?;
        }
        let before = reference.clone();
        let steps = match frame {
            0 | 55 | 90 | 110 => 0,
            30 => 17,
            _ => 2,
        };
        for _ in 0..steps {
            let c = Controls {
                wind: if frame < 45 { 4. } else { -8. },
                stiffness: 0.55,
                motion: frame < 78,
            };
            state.advance(c, 1. / HZ as f32)?;
            let (head, c, t) = state.sample(state.tick);
            for (g, s) in guides.iter().zip(&mut reference) {
                g.advance(s, head, c, t, 1. / HZ as f32);
            }
        }
        world.set_typed_component(id, STATE, state.clone());
        let mut view = extract.snapshot(
            &world,
            Camera::default(),
            if frame < 40 {
                [n as u32, 40]
            } else {
                [n as u32 * 2, 20]
            },
            materials.clone(),
        )?;
        let mut settings = post::Settings::default();
        settings.select(post::Mode::Taa);
        seq.assign(settings, &tech, &mut view);
        let mut g = FrameGraph::for_scene(&view)?;
        let particles = Resource(
            g.resources
                .iter()
                .position(|(name, _)| name == "Groom Verlet history")
                .unwrap(),
        );
        let [current, previous] = g.gpu_poses[&mount.display];
        assert_eq!(previous.is_none(), cold);
        for part in mount.followers {
            assert_eq!(g.gpu_poses[&part], [current, previous]);
        }
        let output = g.input(
            "Read groom test positions and matrices",
            vec![0; n * 40 * 4],
        );
        for (source, base, count) in [
            (Some(particles), 0, n * 8),
            (current, n * 8, n * 16),
            (previous, n * 24, n * 16),
        ] {
            if let Some(source) = source {
                g.pass(
                    "Read groom probe",
                    Op::Compute {
                        program: Dispatch {
                            shader: Shader::Compute(include_bytes!(concat!(
                                env!("OUT_DIR"),
                                "/surface_tail.comp.spv"
                            ))),
                            bindings: vec![
                                (source, Access::ComputeRead),
                                (output, Access::ComputeReadWrite),
                            ],
                            push: bytemuck::bytes_of(&[base as u32, count as u32]).to_vec(),
                        },
                        groups: [(count as u32).div_ceil(64), 1, 1],
                    },
                );
            }
        }
        g.set_output(output);
        let result = backend.execute(g.compile()?, &workers)?;
        if steps == 2 && !cold && frame > 8 {
            let time = |name: &str| result.passes.iter().find(|p| p.name == name).unwrap().ms;
            times.push([
                time("Groom XPBD"),
                time("Groom previous palette") + time("Groom current palette"),
            ]);
        }
        let actual: Vec<f32> = result
            .pixels
            .chunks_exact(4)
            .map(|b| f32::from_ne_bytes(b.try_into().unwrap()))
            .collect();
        let head = solver::head_frame(state.phase_at(state.tick) as f32);
        for i in 0..n {
            let j = i % POINTS;
            let guide = i / POINTS;
            let expected = reference[guide].points[j];
            let p = Vec3::from_slice(&actual[i * 8..]);
            let prev = Vec3::from_slice(&actual[i * 8 + 4..]);
            assert!(p.is_finite() && prev.is_finite());
            worst[0] = worst[0]
                .max(p.distance(expected))
                .max(prev.distance(reference[guide].previous[j]));
            if j < 2 {
                assert!(p.distance(head.transform_point3(guides[guide].rest[j])) < 2e-6);
            } else if state.tick > 0 {
                worst[3] = worst[3]
                    .max(
                        solver::HEAD + solver::MARGIN
                            - p.distance(head.transform_point3(solver::CENTER)),
                    )
                    .max(
                        solver::SHOULDER_RADIUS + solver::MARGIN
                            - p.distance(solver::shoulder_axis(p)),
                    );
            }
            for (k, base, oracle) in [(1, n * 8, &reference), (2, n * 24, &before)] {
                if k == 2 && cold {
                    continue;
                }
                let offset = base + i * 16;
                let m = Mat4::from_cols_array(actual[offset..offset + 16].try_into().unwrap());
                assert!(m.is_finite());
                let rest = &guides[guide].rest;
                let target = &oracle[guide].points;
                let tangent = solver::tangent(rest, j);
                worst[k] = worst[k]
                    .max(m.transform_point3(rest[j]).distance(target[j]))
                    .max(
                        0.01 * m
                            .transform_vector3(tangent)
                            .distance(solver::tangent(target, j)),
                    );
            }
        }
        peak_stretch = peak_stretch.max(result.counters[gpu::STRETCH] as f32 / 1e6);
        if frame == 124 {
            println!("GPU: {}", result.gpu);
        }
    }
    println!(
        "Worst particle/current/previous/contact errors in metres: {worst:?}, peak stretch {:.3}%",
        100. * peak_stretch
    );
    for i in 0..2 {
        times.sort_by(|a, b| a[i].total_cmp(&b[i]));
        println!(
            "{} ({} frames): P50 {:.4} ms, P95 {:.4} ms",
            ["two GPU substeps", "two GPU palettes"][i],
            times.len(),
            times[times.len() / 2][i],
            times[(times.len() * 95).div_ceil(100) - 1][i]
        );
    }
    assert!(worst[..3].iter().all(|v| *v < 0.002) && worst[3] < 1e-5 && peak_stretch < 0.055);
    Ok(())
}
