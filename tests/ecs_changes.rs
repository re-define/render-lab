use anyhow::Result;
use glam::{Mat4, Vec3};
use render_lab::{
    render::{Camera, MaterialId, SceneExtractor, Snapshot},
    scene::{Entity, Light, World},
};
use std::sync::Arc;

fn extract(extractor: &mut SceneExtractor, world: &World) -> Result<Snapshot> {
    extractor.snapshot(world, Camera::default(), [128, 80], Default::default())
}

#[test]
fn sparse_edits_are_visible_to_each_reader_and_preserve_in_flight_snapshots() -> Result<()> {
    let mut world = World::demo();
    let instance = world
        .entities
        .iter()
        .find(|(_, e)| e.instance.is_some())
        .unwrap();
    let id = *instance.0;
    let material = instance.1.instance.as_ref().unwrap().material.unwrap();
    let another = world.spawn(Entity {
        instance: world.entities[&id].instance.clone(),
        ..Default::default()
    });
    let mut fast = SceneExtractor::default();
    let mut slow = SceneExtractor::default();
    let first = extract(&mut fast, &world)?;
    extract(&mut slow, &world)?;
    assert_eq!(first.scene.stats.entities_visited, world.entities.len());
    assert_eq!(extract(&mut fast, &world)?.scene.stats.entities_visited, 0);

    let mut edited = world.entities[&material].material.clone().unwrap();
    edited.roughness = 0.123;
    world.set_material(material, edited.clone())?;
    let bark = extract(&mut fast, &world)?;
    assert_eq!(bark.scene.stats.entities_visited, 1);
    assert_eq!(bark.scene.stats.instances_updated, 0);
    assert!(bark.scene.materials()[&MaterialId(material)].as_ref() == &edited);
    assert!(first.scene.materials()[&MaterialId(material)].as_ref() != &edited);
    assert!(std::ptr::eq(
        bark.scene.instances(),
        first.scene.instances()
    ));
    for bound in [id, another] {
        assert_eq!(
            bark.scene
                .instances()
                .iter()
                .find(|i| i.id == bound)
                .unwrap()
                .material,
            Some(MaterialId(material))
        );
    }

    let local = Mat4::from_translation(Vec3::Y);
    world.set_local(id, Mat4::IDENTITY)?;
    world.entities.get_mut(&id).unwrap().local = local;
    let moved = extract(&mut fast, &world)?;
    assert_eq!(
        (
            moved.scene.stats.entities_visited,
            moved.scene.stats.instances_updated
        ),
        (1, 1)
    );
    for (old, new) in first.scene.instances().iter().zip(moved.scene.instances()) {
        assert_eq!(Arc::ptr_eq(old, new), old.id != id);
        if old.id == id {
            assert_ne!(old.model, local);
            assert_eq!(new.model, local);
        }
    }
    let caught_up = extract(&mut slow, &world)?;
    assert_eq!(
        (
            caught_up.scene.stats.entities_visited,
            caught_up.scene.stats.instances_updated
        ),
        (2, 1)
    );
    assert!(caught_up.scene.materials()[&MaterialId(material)].as_ref() == &edited);
    world.set_local(id, local)?;
    world.set_material(material, edited)?;
    assert_eq!(extract(&mut fast, &world)?.scene.stats.entities_visited, 0);
    Ok(())
}

#[test]
fn hierarchy_light_and_structural_edits_match_a_cold_extractor() -> Result<()> {
    let mut world = World::demo();
    let parent = world.spawn(Default::default());
    let instance = world
        .entities
        .iter()
        .find(|(_, e)| e.instance.is_some())
        .map(|(&id, _)| id)
        .unwrap();
    let template = world.entities[&instance].instance.clone();
    world.entities.get_mut(&instance).unwrap().parent = Some(parent);
    let lamp = world.spawn(Entity {
        parent: Some(instance),
        light: Some(Light::point([1.; 3], 1., 5.)),
        ..Default::default()
    });
    let mut extractor = SceneExtractor::default();
    extract(&mut extractor, &world)?;
    world.set_local(instance, Mat4::from_translation(Vec3::Z))?;
    world.set_local(parent, Mat4::from_translation(Vec3::X))?;
    let frame = extract(&mut extractor, &world)?;
    assert_eq!(frame.scene.stats.transforms_updated, 3);
    assert_eq!(frame.scene.stats.entities_visited, 3);
    assert_eq!(frame.scene.stats.instances_updated, 1);
    assert_eq!(frame.scene.lights()[0].position, Vec3::X + Vec3::Z);
    let cold = extract(&mut Default::default(), &world)?;
    assert_eq!(frame.scene.lights(), cold.scene.lights());
    for (a, b) in frame.scene.instances().iter().zip(cold.scene.instances()) {
        assert_eq!(a.model, b.model);
    }

    world.entities.get_mut(&instance).unwrap().parent = Some(lamp);
    assert!(extract(&mut extractor, &world).is_err());
    world.entities.get_mut(&instance).unwrap().parent = None;
    assert_eq!(
        extract(&mut extractor, &world)?.scene.lights()[0].position,
        Vec3::Z
    );
    world.entities.remove(&lamp);
    let added = world.spawn(Entity {
        instance: template,
        ..Default::default()
    });
    let next = extract(&mut extractor, &world)?;
    assert_eq!(next.scene.stats.entities_visited, world.entities.len());
    assert!(next.scene.lights().is_empty());
    assert!(next.scene.instances().iter().any(|i| i.id == added));

    let mut fork = world.clone();
    fork.entities.get_mut(&added).unwrap().instance = None;
    let next = extract(&mut extractor, &fork)?;
    assert_eq!(next.scene.stats.entities_visited, fork.entities.len());
    assert!(!next.scene.instances().iter().any(|i| i.id == added));
    assert!(
        extract(&mut extractor, &world)?
            .scene
            .instances()
            .iter()
            .any(|i| i.id == added)
    );
    Ok(())
}

#[test]
fn lagging_extractor_reconciles_evicted_edits() -> Result<()> {
    let mut world = World::demo();
    let id = world
        .entities
        .iter()
        .find(|(_, e)| e.instance.is_some())
        .map(|(&id, _)| id)
        .unwrap();
    let mut extractor = SceneExtractor::default();
    extract(&mut extractor, &world)?;
    for n in 0..10_000 {
        world.set_local(id, Mat4::from_translation(Vec3::Y * n as f32))?;
    }
    let next = extract(&mut extractor, &world)?;
    assert_eq!(next.scene.stats.entities_visited, world.entities.len());
    assert_eq!(next.scene.stats.instances_updated, 1);
    assert_eq!(
        next.scene
            .instances()
            .iter()
            .find(|i| i.id == id)
            .unwrap()
            .model,
        world.entities[&id].local
    );
    assert_eq!(
        extract(&mut extractor, &world)?
            .scene
            .stats
            .entities_visited,
        0
    );
    Ok(())
}
