//! Bind the imported Silver Wolf and initialize its motion player.
use super::*;

pub fn scene() -> Result<World> {
    let mut w = cartoon::scene()?;
    let index: Index = serde_json::from_str(INDEX)?;
    let bones = index
        .bones
        .iter()
        .map(|name| {
            w.entities
                .iter()
                .find_map(|(&id, e)| (&e.name == name).then_some(id))
                .with_context(|| format!("missing Silver Wolf joint {name}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut root = bones[0];
    while let Some(parent) = w.entities[&root].parent {
        root = parent;
    }
    let bank = SilverMatcher::shared()?;
    let (range, name) = bank
        .clips
        .iter()
        .find(|(_, name)| name == "idle_41")
        .context("missing idle pose")?;
    let sample = range.start + 10;
    let pose = &bank.poses[sample];
    let hip_origin = w.entities[&bones[0]].local.w_axis.truncate();
    for (i, &id) in bones.iter().enumerate() {
        let e = w.entities.get_mut(&id).unwrap();
        let (scale, _, mut t) = e.local.to_scale_rotation_translation();
        if i == 0 {
            t += pose.hip;
        }
        e.local = Mat4::from_scale_rotation_translation(scale, pose.rotations[i], t);
    }
    let state = State {
        mode: Default::default(),
        evaluated: 0,
        bones,
        rig: Some(Arc::new(profile::DEFAULT.rig.clone())),
        root_source: Default::default(),
        observed: None,
        layout: None,
        standalone_velocity: None,
        hip_origin,
        speed: 0.,
        turn: 0.,
        side: 0.,
        move_root: true,
        velocity: Vec2::ZERO,
        acceleration: Vec2::ZERO,
        yaw_rate: 0.,
        transition: None,
        sample,
        clip: name.clone(),
        cost: 0.,
        fraction: 0.,
        search_in: 0,
        yaw: 0.,
    };
    let actor = w.spawn(Entity {
        name: "Silver Wolf motion matching".into(),
        ..Default::default()
    });
    w.set_typed_component(actor, PLAYER, state);
    w.set_motion_policy(actor, MotionPolicy::KINEMATIC)?;
    w.entities.get_mut(&root).unwrap().parent = Some(actor);
    w.validate()?;
    Ok(w)
}
