use anyhow::Result;
use render_lab::{
    render::{Camera, SceneExtractor},
    scene::{Entity, GeometryAssets, Instance, LocalBounds, World},
};
use std::sync::Arc;

#[test]
fn shared_products_round_trip_and_update_without_rebuilding_instances() -> Result<()> {
    let mut world = World::demo();
    let (&first, _) = world
        .entities
        .iter()
        .find(|(_, e)| e.instance.is_some())
        .unwrap();
    let mesh = world.entities[&first].instance.as_ref().unwrap().mesh;
    let bounds = LocalBounds::from_mesh(world.entities[&mesh].mesh.as_ref().unwrap())?;
    let asset = GeometryAssets {
        source_mesh: mesh,
        local_bounds: Some(bounds),
        shadow_mesh: Some(mesh),
    };
    let assets = world.spawn(Entity {
        geometry_assets: Some(Arc::new(asset.clone())),
        ..Default::default()
    });
    let second = world.spawn(Entity {
        instance: Some(Instance::new(mesh, None)),
        ..Default::default()
    });
    for id in [first, second] {
        world.bind_geometry_assets(id, Some(assets))?;
    }
    let sample = |x: &mut SceneExtractor, w: &World| {
        x.snapshot(w, Camera::default(), [160, 100], Default::default())
    };
    let mut reader = SceneExtractor::default();
    let old = sample(&mut reader, &world)?.scene;
    let mut larger = asset.clone();
    larger.local_bounds = Some(LocalBounds {
        min: [-2.; 3],
        max: [2.; 3],
    });
    world.set_geometry_assets(assets, larger.clone())?;
    let new = sample(&mut reader, &world)?.scene;
    assert!(!new.stats.full_rebuild);
    assert_eq!(
        (new.stats.entities_visited, new.stats.instances_updated),
        (1, 0)
    );
    for id in [first, second] {
        let i = old.instances().iter().find(|i| i.id == id).unwrap();
        let j = new.instances().iter().find(|i| i.id == id).unwrap();
        assert!(Arc::ptr_eq(i, j));
        assert_eq!(old.products_for(i), Some(&asset));
        assert_eq!(new.products_for(j), Some(&larger));
    }
    let mut invalid = larger.clone();
    invalid.local_bounds = Some(LocalBounds {
        min: [-0.01; 3],
        max: [0.01; 3],
    });
    assert!(world.set_geometry_assets(assets, invalid).is_err());
    assert!(world.bind_geometry_assets(second, Some(mesh)).is_err());
    world.bind_geometry_assets(first, None)?;
    let rebound = sample(&mut reader, &world)?.scene;
    assert_eq!(
        (
            rebound.stats.entities_visited,
            rebound.stats.instances_updated
        ),
        (1, 1)
    );
    assert!(!rebound.stats.full_rebuild);
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("scene.json");
    world.save(&path)?;
    let world = World::load(&path)?;
    let restored = sample(&mut SceneExtractor::default(), &world)?.scene;
    assert_eq!(
        restored.products_for(restored.instances().iter().find(|i| i.id == first).unwrap()),
        None
    );
    assert_eq!(
        restored.products_for(
            restored
                .instances()
                .iter()
                .find(|i| i.id == second)
                .unwrap()
        ),
        Some(&larger)
    );
    Ok(())
}
