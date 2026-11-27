use super::*;
use crate::animation::{Node, Operation, PoseGraph, RotationLayer};
use std::time::Instant;

fn setup(two_layers: bool) -> Result<(World, Stage, SilverMatcher)> {
    let mut w = scene()?;
    let actor = actor(&w).unwrap();
    pose_graph::command(&mut w, actor, "upper")?;
    if two_layers {
        let asset = w
            .typed_component::<Contact>(actor, SETTING)?
            .unwrap()
            .graph
            .unwrap();
        let mut graph = w
            .typed_component::<PoseGraph>(asset, pose_graph::ASSET)?
            .unwrap()
            .into_owned();
        let mut layer = graph.nodes["upper_body"].clone();
        graph.nodes.insert(
            "walk".into(),
            Node::Clip {
                clip: "walk_41".into(),
                speed: 0.8,
            },
        );
        if let Node::MaskedBlend {
            input,
            overlay,
            weight,
            window,
            ..
        } = &mut layer
        {
            *overlay = "walk".into();
            *input = "upper_body".into();
            *weight = 0.35;
            *window = Some([0., 0.2, 0.7, 1.]);
        }
        graph.nodes.insert("second".into(), layer);
        if let Node::PhysicalLimbs { input } = graph.nodes.get_mut("limbs").unwrap() {
            *input = "second".into();
        }
        w.set_typed_component(asset, pose_graph::ASSET, graph);
    }
    let s = bind_actor(&w, actor, &children(&w))?;
    Ok((w, s, SilverMatcher::load()?))
}

// Independent scalar reference: sample and propagate the complete pose after every layer.
fn reference(
    layers: &[RotationLayer],
    bank: &SilverMatcher,
    s: &Stage,
    pose: &mut Pose<'_>,
    t: f64,
) {
    let map = s.binding.rig.as_deref().unwrap_or(&profile::DEFAULT.rig);
    for layer in layers {
        let envelope = layer.envelope(t);
        if envelope == 0. {
            continue;
        }
        let phase =
            (t * f64::from(HZ) * f64::from(layer.speed)).rem_euclid(layer.frames.len() as f64);
        let i = phase.floor() as usize;
        let a = &bank.poses[layer.frames.start + i];
        let b = &bank.poses[layer.frames.start + (i + 1) % layer.frames.len()];
        for &(channel, weight) in &layer.channels {
            let bone = s.rig.corrected[s.rig.source_slots[channel]];
            let (scale, base, offset) = pose.local[bone].to_scale_rotation_translation();
            let sampled = a.rotations[channel].slerp(b.rotations[channel], phase.fract() as f32);
            let rotation = base
                .slerp(map.rotation(channel, sampled), weight * envelope)
                .normalize();
            pose.local[bone] = Mat4::from_scale_rotation_translation(scale, rotation, offset);
        }
        pose.forward();
    }
}

fn study(two_layers: bool, profile: bool) -> Result<()> {
    let (w, s, bank) = setup(two_layers)?;
    let r = &s.rig;
    let Operation::Blend(layers) = &s.program.operations()[0] else {
        unreachable!()
    };
    assert_eq!(layers.len(), if two_layers { 2 } else { 1 });
    if two_layers {
        for (layer, name) in layers.iter().zip(["run_41", "walk_41"]) {
            assert_eq!(
                &layer.frames,
                &bank.clips.iter().find(|(_, id)| id == name).unwrap().0
            );
        }
    }
    assert_eq!(s.blend_joints.len(), 1);
    let base: Vec<_> = r.ids.iter().map(|id| w.entities[id].local).collect();
    let mut local = base.clone();
    let mut world = vec![Mat4::IDENTITY; base.len()];
    let mut pose = Pose {
        rig: r,
        local: &mut local,
        world: &mut world,
    };
    pose.forward();
    let start_world = pose.world.to_vec();
    let affected = &s.blend_joints[0];
    assert!(affected.len() < r.ids.len());
    let mut cache = pose_graph::SampleCache::default();
    for t in [0., 0.11, 0.11, 0.2, 0.71, 1., 4.33, 12.502] {
        pose.local.copy_from_slice(&base);
        pose.world.copy_from_slice(&start_world);
        reference(layers, &bank, &s, &mut pose, t);
        let expected = (pose.local.to_vec(), pose.world.to_vec());
        pose.local.copy_from_slice(&base);
        pose.world.copy_from_slice(&start_world);
        pose_graph::blend(layers, affected, &bank, &s, &mut pose, t, &mut cache);
        assert_eq!(&*pose.local, expected.0);
        assert_eq!(&*pose.world, expected.1);
    }
    if profile {
        for staggered in [false, true] {
            let mut times: [Vec<f64>; 2] = Default::default();
            for frame in 0..116 {
                for k in 0..2 {
                    let optimized = (frame + k) % 2 == 1;
                    let start = Instant::now();
                    for actor in 0..500 {
                        let t =
                            (frame as f64 + if staggered { actor as f64 * 0.37 } else { 0. }) / 60.;
                        let t = 0.25 + t % 0.4; // Both layers are fully active throughout the benchmark.
                        pose.local.copy_from_slice(&base);
                        pose.world.copy_from_slice(&start_world);
                        if optimized {
                            pose_graph::blend(
                                layers, affected, &bank, &s, &mut pose, t, &mut cache,
                            );
                        } else {
                            reference(layers, &bank, &s, &mut pose, t);
                        }
                        std::hint::black_box((&*pose.local, &*pose.world));
                    }
                    if frame >= 20 {
                        times[usize::from(optimized)].push(start.elapsed().as_secs_f64() * 1000.);
                    }
                }
            }
            for t in &mut times {
                t.sort_by(f64::total_cmp);
            }
            println!(
                "layers={} staggered={staggered} FK {}/{}; scalar/compiled P50 {:.3}/{:.3} ms P95 {:.3}/{:.3} ms",
                layers.len(),
                affected.len(),
                r.ids.len(),
                times[0][48],
                times[1][48],
                times[0][91],
                times[1][91]
            );
        }
    }
    Ok(())
}

#[test]
fn fused_local_layers_and_sparse_fk_match_full_fk_at_each_boundary() -> Result<()> {
    study(false, false)?;
    study(true, false)
}

#[test]
#[ignore = "release pose kernel benchmark"]
fn silver_masked_evaluation_profile() -> Result<()> {
    study(false, true)?;
    study(true, true)
}
