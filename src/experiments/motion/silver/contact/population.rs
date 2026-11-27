//! Populate separate simulation lanes while sharing meshes, materials and animation data.
use super::*;
use std::collections::BTreeSet;

pub(super) fn populated_world(
    n: usize,
    pool: &rayon::ThreadPool,
    show_lanes: bool,
) -> Result<World> {
    ensure!(n > 0, "population must have at least one actor");
    let mut initial = Arc::new(scene()?);
    let mut schedule = Schedule::default();
    install(&initial, &mut schedule)?;
    let actor = super::actor(&initial).unwrap();
    let prototype = initial
        .typed_component::<Contact>(actor, SETTING)?
        .unwrap()
        .into_owned();
    let f = prototype.course.unwrap();
    let supports = [
        prototype.floor,
        prototype.plate,
        f.landing,
        f.station,
        f.ramp,
        f.service.unwrap(),
    ];
    // The course reuses a cube and an indicator mesh for all visible props.
    let course_meshes = [
        initial.entities[&prototype.floor]
            .instance
            .as_ref()
            .unwrap()
            .mesh,
        initial.entities[&prototype.target]
            .instance
            .as_ref()
            .unwrap()
            .mesh,
    ];
    let mut nodes = BTreeSet::new();
    for (&id, ent) in &initial.entities {
        let mut at = Some(id);
        while let Some(k) = at {
            if k == actor {
                nodes.insert(id);
                break;
            }
            at = initial.entities[&k].parent;
        }
        if ent.skin.is_some()
            || ent.skin_binding.is_some()
            || show_lanes
                && ent
                    .instance
                    .as_ref()
                    .is_some_and(|part| course_meshes.contains(&part.mesh))
        {
            nodes.insert(id);
        }
    }
    nodes.extend(supports);
    nodes.extend([f.rail, f.gate, prototype.target]);
    // Stagger up to twenty states sampled from the real course, including vault and duck.
    let prototype_parts = initial
        .entities
        .values()
        .filter(|e| e.instance.is_some() && e.skin_binding.is_some())
        .count();
    let phases = n.min(20);
    let mut seeds = vec![initial.clone()];
    for _ in 1..phases {
        for _ in 0..24 {
            schedule.tick(&mut initial, 1. / 60., pool)?;
        }
        seeds.push(initial.clone());
    }
    let mut w = (*seeds[0]).clone();
    // Offset the rendered lanes in world space. Each collider and actor simulate in the same lane-local frame.
    let cols = (n as f32 * 1.8).sqrt().ceil() as usize;
    let rows = n.div_ceil(cols);
    for i in 0..n {
        let seed = &seeds[i % phases];
        let mut ids = BTreeMap::new();
        for &k in &nodes {
            let dst = if i == 0 {
                k
            } else {
                w.spawn(Entity::default())
            };
            ids.insert(k, dst);
        }
        let map = |id| ids[&id];
        let shift = Vec3::new(
            (if show_lanes { 4.6 } else { 2.7 }) * ((i % cols) as f32 - (cols - 1) as f32 * 0.5),
            0.,
            9.8 * ((i / cols) as f32 - (rows - 1) as f32 * 0.5),
        );
        let lane = w.spawn(Entity {
            local: Mat4::from_translation(shift),
            ..Default::default()
        });
        for &k in &nodes {
            let mut row = seed.entities[&k].clone();
            row.parent = Some(
                row.parent
                    .map_or(lane, |v| ids.get(&v).copied().unwrap_or(v)),
            );
            if let Some(s) = &mut row.skin {
                for j in &mut s.joints {
                    *j = map(*j);
                }
            }
            row.skin_binding = row.skin_binding.map(map);
            // Other lanes share asset data and carry independent, invisible collision fixtures.
            if !show_lanes
                && i != 0
                && (supports.contains(&k) || [f.rail, f.gate, prototype.target].contains(&k))
            {
                row.instance = None;
            }
            // Runtime-typed components keep a null marker on the serialized entity.
            let policy = row
                .components
                .contains_key(MOTION_POLICY)
                .then(|| seed.motion_policy(k, MotionPolicy::STATIC))
                .transpose()?;
            if policy.is_some() {
                row.components.remove(MOTION_POLICY);
            }
            w.entities.insert(map(k), row);
            if let Some(policy) = policy {
                w.set_motion_policy(map(k), policy)?;
            }
        }
        let mut p = player(seed, actor)?.into_owned();
        p.bones.iter_mut().for_each(|b| *b = map(*b));
        let mut c = seed
            .typed_component::<Contact>(actor, SETTING)?
            .unwrap()
            .into_owned();
        let mut b = seed
            .typed_component::<dynamics::Body>(actor, dynamics::BODY)?
            .unwrap()
            .into_owned();
        b.remap_supports(map);
        if let Some(old) = seed.typed_component::<ResolvedMotion>(actor, RESOLVED)? {
            let mut resolved = old.into_owned();
            resolved.support = resolved.support.map(map);
            resolved.surfaces = Arc::default();
            w.set_typed_component(map(actor), RESOLVED, resolved);
        }
        if let Some(old) = seed.typed_component::<ProposedMotion>(actor, PROPOSED)? {
            let mut proposed = *old;
            if let Some(grip) = &mut proposed.grip {
                grip.support = map(grip.support);
            }
            w.set_typed_component(map(actor), PROPOSED, proposed);
        }
        c.finger.iter_mut().for_each(|m| m.joint = map(m.joint));
        let spring = seed
            .typed_component::<dynamics::Spring>(prototype.plate, dynamics::SPRING)?
            .unwrap()
            .into_owned();
        w.set_typed_component(map(prototype.plate), dynamics::SPRING, spring);
        for foot in &mut c.feet {
            foot.anchor = foot.anchor.map(|(k, p)| (map(k), p));
        }
        c.floor = map(c.floor);
        c.plate = map(c.plate);
        c.target = map(c.target);
        c.course = Some(course::Fixtures {
            landing: map(f.landing),
            station: map(f.station),
            ramp: map(f.ramp),
            service: f.service.map(map),
            rail: map(f.rail),
            gate: map(f.gate),
        });
        if let Some(binding) = &mut c.source_binding {
            let binding = Arc::make_mut(binding);
            binding.bones.clone_from(&p.bones);
            for (joint, parent) in &mut binding.hierarchy {
                *joint = map(*joint);
                *parent = w.entities[joint].parent;
            }
            binding.finger.clone_from(&c.finger);
            (binding.floor, binding.plate, binding.target, binding.course) =
                (c.floor, c.plate, c.target, c.course);
        }
        w.set_typed_component(map(actor), PLAYER, p);
        w.set_typed_component(map(actor), SETTING, c);
        w.set_typed_component(map(actor), dynamics::BODY, b);
    }
    w.validate()?;
    let skinned = w
        .entities
        .values()
        .filter(|e| e.instance.is_some() && e.skin_binding.is_some())
        .count();
    eprintln!(
        "population={n}, skinned instances={skinned}, entities={}, players={}",
        w.entities.len(),
        w.entities.component_owners(PLAYER).len()
    );
    anyhow::ensure!(
        prototype_parts > 0
            && skinned == prototype_parts * n
            && w.entities.component_owners(PLAYER).len() == n
            && w.entities.component_owners(dynamics::BODY).len() == n
    );
    Ok(w)
}

pub(super) fn alternate_rig(w: &mut World, id: Id) -> Result<(Id, FingerMarker)> {
    let children = rig::children(w);
    let alternate = Rig::bind(w, id, &children)?;
    let fingertip = alternate.ids[alternate.finger];
    // A second skeleton keeps the bank's semantic joints, with an extra spacer between
    // forearm and wrist and a different arm length. Exercise traversal by ID, not by name.
    let wrist = alternate.ids[alternate.arm[2]];
    let parent = w.entities[&wrist].parent.unwrap();
    let correction = Quat::from_rotation_y(0.43);
    let spacer = w.spawn(Entity {
        name: "Wrist adapter".into(),
        parent: Some(parent),
        local: Mat4::from_rotation_translation(correction, Vec3::new(0.01, 0., 0.)),
        ..Default::default()
    });
    let entity = w.entities.get_mut(&wrist).unwrap();
    entity.parent = Some(spacer);
    entity.local = Mat4::from_quat(correction.conjugate()) * entity.local;
    let mut player = player(w, id)?.into_owned();
    let mut semantics = player
        .rig
        .as_deref()
        .unwrap_or(&profile::DEFAULT.rig)
        .clone();
    semantics
        .basis
        .resize(player.bones.len(), Default::default());
    semantics.basis[semantics.reach_arm[2]].parent = correction.conjugate();
    player.rig = Some(Arc::new(semantics));
    player.transition = None;
    w.set_typed_component(id, PLAYER, player);
    for &joint in &alternate.ids {
        w.entities.get_mut(&joint).unwrap().name = format!("alternative rig joint {joint}");
    }
    let marker = FingerMarker {
        joint: fingertip,
        offset: profile::DEFAULT.rig.fingertip_offset,
    };
    let mut contact = w
        .typed_component::<Contact>(id, SETTING)?
        .context("missing contact actor")?
        .into_owned();
    contact.finger = Some(marker.clone());
    w.set_typed_component(id, SETTING, contact);
    Ok((spacer, marker))
}
