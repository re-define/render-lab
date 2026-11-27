use super::*;
use crate::animation::{Node, Operation};

#[test]
fn graph_links_validate_types_cycles_masks_and_state_ownership() -> Result<()> {
    let bank = SilverMatcher::load()?;
    let compile = |g: &PoseGraph| g.compile(&bank.channels, &bank.clips);
    let mut graph = UPPER.clone();
    assert!(matches!(
        compile(&graph)?.operations(),
        [
            Operation::Blend(_),
            Operation::PhysicalLimbs,
            Operation::FootPlacement,
            Operation::HandIk
        ]
    ));
    let mut timed = graph.clone();
    if let Node::MaskedBlend { window, .. } = timed.nodes.get_mut("upper_body").unwrap() {
        *window = Some([0.5, 1., 2., 2.5]);
    }
    let compiled = compile(&timed)?;
    let Operation::Blend(layer) = &compiled.operations()[0] else {
        unreachable!()
    };
    for (t, expected) in [(0., 0.), (0.75, 0.5), (1.5, 1.), (2.25, 0.5), (2.5, 0.)] {
        assert_eq!(layer[0].envelope(t), expected);
    }
    if let Node::MaskedBlend { window, .. } = timed.nodes.get_mut("upper_body").unwrap() {
        *window = Some([1., 0., 2., 2.5]);
    }
    assert!(compile(&timed).is_err());
    let mut zero = graph.clone();
    let Node::MaskedBlend { weight, .. } = zero.nodes.get_mut("upper_body").unwrap() else {
        unreachable!()
    };
    *weight = 0.;
    assert!(matches!(
        compile(&zero)?.operations(),
        [
            Operation::PhysicalLimbs,
            Operation::FootPlacement,
            Operation::HandIk
        ]
    ));
    for (at, node) in [
        ("upper_clip", Node::Input),
        (
            "upper_clip",
            Node::Clip {
                clip: "unknown".into(),
                speed: 1.,
            },
        ),
        (
            "limbs",
            Node::PhysicalLimbs {
                input: "feet".into(),
            },
        ),
        (
            "limbs",
            Node::HandIk {
                input: "upper_body".into(),
            },
        ),
        ("extra", Node::Input),
    ] {
        let mut wrong = graph.clone();
        wrong.nodes.insert(at.into(), node);
        assert!(compile(&wrong).is_err(), "{at}");
    }
    let Node::MaskedBlend { mask, .. } = graph.nodes.get_mut("upper_body").unwrap() else {
        unreachable!()
    };
    mask.insert("nonexistent".into(), 1.);
    assert!(compile(&graph).is_err());
    Ok(())
}

#[test]
fn masked_display_preserves_root_and_legs_and_resumes_after_reload() -> Result<()> {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let one = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
    let w = scene()?;
    let id = actor(&w).unwrap();
    let bones = player(&w, id)?.bones.clone();
    let make = |mut w: World, upper: bool| -> Result<_> {
        let mut c = w
            .typed_component::<Contact>(id, SETTING)?
            .unwrap()
            .into_owned();
        traversal::Section::Rail.practice(&mut w, id, &mut c)?;
        w.set_typed_component(id, SETTING, c);
        if upper {
            command(&mut w, id, "upper")?;
        }
        let mut schedule = Schedule::default();
        install(&w, &mut schedule)?;
        Ok((Arc::new(w), schedule))
    };
    let (mut authored, mut a) = make(w.clone(), false)?;
    let (mut layered, mut b) = make(w, true)?;
    let dt = 1. / 60.;
    let mut different = false;
    for _ in 0..90 {
        a.tick(&mut authored, dt, &one)?;
        b.tick(&mut layered, dt, &pool)?;
        assert_eq!(authored.entities[&id].local, layered.entities[&id].local);
        for i in [0, 13, 14, 15, 16, 17, 18, 19, 20] {
            assert_eq!(
                authored.entities[&bones[i]].local,
                layered.entities[&bones[i]].local
            );
        }
        assert_eq!(
            authored
                .typed_component::<Contact>(id, SETTING)?
                .unwrap()
                .palm_load,
            layered
                .typed_component::<Contact>(id, SETTING)?
                .unwrap()
                .palm_load
        );
        different |= !authored.entities[&bones[11]]
            .local
            .abs_diff_eq(layered.entities[&bones[11]].local, 0.02);
    }
    assert!(
        different,
        "upper-body blend never reached the published skeleton"
    );
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("graph-scene.json");
    Arc::make_mut(&mut layered).clocks = b.checkpoints(&layered);
    layered.save(&file)?;
    let mut reload = World::load(&file)?;
    crate::experiments::SceneConfiguration::restore(&mut reload)?;
    let mut replay = Schedule::default();
    install(&reload, &mut replay)?;
    replay.restore_clocks(&reload)?;
    let mut reload = Arc::new(reload);
    for _ in 0..12 {
        b.tick(&mut layered, dt, &pool)?;
        replay.tick(&mut reload, dt, &one)?;
        for &joint in &bones {
            assert!(
                layered.entities[&joint]
                    .local
                    .abs_diff_eq(reload.entities[&joint].local, 1e-4)
            );
        }
    }
    command(Arc::make_mut(&mut layered), id, "default")?;
    b.tick(&mut layered, dt, &pool)?;
    let c = layered.typed_component::<Contact>(id, SETTING)?.unwrap();
    assert!(c.graph.is_none());
    assert_eq!(c.graph_seconds, 0.);
    Ok(())
}

#[test]
fn one_asset_has_private_playback_and_invalid_load_is_atomic() -> Result<()> {
    let mut w = duo_scene()?;
    let ids = w.entities.component_owners(SETTING).to_vec();
    command(&mut w, ids[0], "upper")?;
    let mut schedule = Schedule::default();
    install(&w, &mut schedule)?;
    let mut w = Arc::new(w);
    let threads = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    schedule.tick(&mut w, 1. / 30., &threads)?;
    let dir = tempfile::tempdir()?;
    let custom = dir.path().join("saved upper graph.json");
    std::fs::write(&custom, serde_json::to_vec(&*UPPER)?)?;
    command(
        Arc::make_mut(&mut w),
        ids[1],
        &format!("load {}", custom.display()),
    )?;
    assert_eq!(w.entities.component_owners(ASSET).len(), 1);
    schedule.tick(&mut w, 1. / 60., &threads)?;
    let a = w
        .typed_component::<Contact>(ids[0], SETTING)?
        .unwrap()
        .into_owned();
    let b = w
        .typed_component::<Contact>(ids[1], SETTING)?
        .unwrap()
        .into_owned();
    assert_eq!(a.graph, b.graph);
    assert!(a.graph_seconds > b.graph_seconds && b.graph_seconds > 0.);
    let bank = SilverMatcher::load()?;
    let mut compiled = Programs::new(&bank)?;
    assert!(Arc::ptr_eq(
        &compiled.resolve(&w, a.graph, &bank)?,
        &compiled.resolve(&w, b.graph, &bank)?
    ));
    std::fs::write(&custom, r#"{"output":"missing","nodes":{}}"#)?;
    let n = w.entities.len();
    assert!(
        command(
            Arc::make_mut(&mut w),
            ids[1],
            &format!("load {}", custom.display())
        )
        .is_err()
    );
    assert_eq!(w.entities.len(), n);
    let unchanged = w.typed_component::<Contact>(ids[1], SETTING)?.unwrap();
    assert_eq!(
        (unchanged.graph, unchanged.graph_seconds),
        (b.graph, b.graph_seconds)
    );
    Ok(())
}

#[test]
fn changing_graph_links_in_place_changes_the_final_ik_pose() -> Result<()> {
    let mut w = scene()?;
    let id = actor(&w).unwrap();
    let mut state = w
        .typed_component::<Contact>(id, SETTING)?
        .unwrap()
        .into_owned();
    traversal::Section::Rail.practice(&mut w, id, &mut state)?;
    w.set_typed_component(id, SETTING, state);
    command(&mut w, id, "upper")?;
    let asset = w
        .typed_component::<Contact>(id, SETTING)?
        .unwrap()
        .graph
        .unwrap();
    let arm = Rig::bind(&w, id, &rig::children(&w))?;
    let wrist = arm.ids[arm.arm[2]];
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut schedule = Schedule::default();
    install(&w, &mut schedule)?;
    let mut w = Arc::new(w);
    schedule.tick(&mut w, 1. / 60., &pool)?;
    let mut control = w.clone();
    let mut reference = Schedule::default();
    install(&control, &mut reference)?;
    let mut changed = UPPER.clone();
    if let Node::PhysicalLimbs { input } = changed.nodes.get_mut("limbs").unwrap() {
        *input = "authored".into();
    }
    if let Node::MaskedBlend { input, .. } = changed.nodes.get_mut("upper_body").unwrap() {
        *input = "hand".into();
    }
    changed.output = "upper_body".into();
    Arc::make_mut(&mut w).set_typed_component(asset, ASSET, changed);
    let mut diverged = false;
    for _ in 0..75 {
        schedule.tick(&mut w, 1. / 60., &pool)?;
        reference.tick(&mut control, 1. / 60., &pool)?;
        assert_eq!(w.entities[&id].local, control.entities[&id].local);
        diverged |= !w.entities[&wrist]
            .local
            .abs_diff_eq(control.entities[&wrist].local, 0.01);
    }
    assert!(
        diverged,
        "editing the shared graph did not change the final wrist pose"
    );
    Ok(())
}
