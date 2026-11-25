//! Exercise the real forest material bindings, incremental extraction, texture payload and Vulkan slot.
use super::*;
use crate::{
    experiments::{self, clusters::ClusterLod, vegetation},
    rhi::Backend,
};
use std::time::Instant;

fn solid(width: u32, height: u32, color: [u8; 4]) -> Arc<Texture> {
    Arc::new(Texture {
        width,
        height,
        rgba: color.repeat((width * height) as usize),
    })
}

fn bind_bark(world: &mut World, material_id: Id, texture: Arc<Texture>) -> Result<Id> {
    let id = world.spawn(Entity {
        name: "Atlas test texture".into(),
        texture: Some(texture),
        ..Default::default()
    });
    let mut material = world.entities[&material_id].material.clone().unwrap();
    material.texture = Some(id);
    world.set_material(material_id, material)?;
    Ok(id)
}

fn hit_region(view: &Snapshot, material: Id) -> Result<(u64, [u32; 3])> {
    let mut graph = FrameGraph::new([1, 1]);
    let (resource, revision) = Atlas::gi_hit_table(&view.scene, &mut graph)?;
    let header = 16 * view.scene.routes.material_slots[&MaterialId(material)] + 8;
    let rows = &graph.tables[&resource];
    Ok((
        revision,
        std::array::from_fn(|i| u32::from_ne_bytes(rows.record(header + i).try_into().unwrap())),
    ))
}

fn p95(samples: &mut [f64]) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[(95 * samples.len()).div_ceil(100) - 1]
}

#[test]
#[ignore = "full procedural forest and a Vulkan 1.4 device"]
fn forest_bark_replacement_keeps_other_texels_and_measures_uploads() -> Result<()> {
    let mut world = vegetation::forest()?;
    let count = world.entities.len();
    let barks: Vec<_> = world
        .entities
        .iter()
        .filter(|(_, e)| e.name == "Bark")
        .map(|(&id, _)| id)
        .collect();
    let ground = world
        .entities
        .iter()
        .find(|(_, e)| e.texture.as_ref().is_some_and(|t| t.width == 1024))
        .map(|(&id, _)| id)
        .context("forest terrain texture")?;
    let trees = world.entities.values().filter(|e| {
        e.name == "Forest tree" && e.instance.as_ref().unwrap().material == Some(barks[0])
    });
    let bark_users = trees.clone().count();
    assert!(bark_users > 200);
    let target = trees
        .map(|e| e.local.transform_point3(Vec3::ZERO))
        .min_by(|a, b| a.length_squared().total_cmp(&b.length_squared()))
        .unwrap();
    let camera = Camera {
        target: target + Vec3::Y * 1.9,
        distance: 5.5,
        yaw: 0.12,
        pitch: 0.04,
    };
    let white = solid(64, 64, [255, 255, 255, 255]);
    let red = solid(64, 64, [230, 25, 40, 255]);
    let large = solid(128, 64, [230, 25, 40, 255]);
    let bark = bind_bark(&mut world, barks[0], white.clone())?;
    let untouched = bind_bark(&mut world, barks[1], solid(16, 8, [200, 180, 150, 255]))?;
    let engine = experiments::open(world)?;
    let config = engine.materials.clone();
    let mut world = engine.world.clone();
    drop(engine);
    let world = Arc::make_mut(&mut world);
    let mut extractor = SceneExtractor::default();
    let snapshot = |extractor: &mut SceneExtractor, world: &World| {
        extractor.snapshot(world, camera, [384, 216], config.clone())
    };
    let cold = snapshot(&mut extractor, world)?;
    let cold_atlas = Atlas::cached(&cold.scene)?;
    let cold_layout = cold_atlas.locations.clone();
    let (original_gi, region) = hit_region(&cold, barks[0])?;
    assert_eq!(region, cold_layout[&bark]);
    let bytes = cold_atlas.pixels.bytes();
    let old_word = cold_atlas
        .pixels
        .record(cold_layout[&bark][0] as usize)
        .to_vec();
    let mut cold_cpu = Vec::new();
    let mut hot_cpu = Vec::new();
    let mut edit_cpu = Vec::new();
    let mut extract_cpu = Vec::new();
    for i in 0..32 {
        let now = Instant::now();
        let rebuilt =
            AtlasCache::default().get(&cold.scene, &BTreeMap::new(), "Base color texels")?;
        cold_cpu.push(now.elapsed().as_secs_f64() * 1000.);
        assert_eq!(rebuilt.locations, cold_layout);
        let texture = if i % 2 == 0 { &red } else { &white };
        world.set_texture(bark, texture.clone())?;
        let now = Instant::now();
        let changed = snapshot(&mut extractor, world)?;
        extract_cpu.push(now.elapsed().as_secs_f64() * 1000.);
        assert!(!changed.scene.stats.full_rebuild);
        assert_eq!(changed.scene.stats.entities_visited, 1);
        assert_eq!(changed.scene.stats.instances_updated, 0);
        let now = Instant::now();
        let a = Atlas::cached(&changed.scene)?;
        edit_cpu.push(now.elapsed().as_secs_f64() * 1000.);
        let now = Instant::now();
        let cached = Atlas::cached(&changed.scene)?;
        hot_cpu.push(now.elapsed().as_secs_f64() * 1000.);
        assert!(Arc::ptr_eq(&cached, &a));
        assert_eq!(a.locations, cold_layout);
        assert_eq!(
            a.pixels.record(a.locations[&bark][0] as usize),
            &texture.rgba[..4]
        );
    }
    assert_eq!(
        cold_atlas.pixels.record(cold_layout[&bark][0] as usize),
        old_word
    );
    let gpu = &mut Backend::open()?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(8).build()?;
    let tech = ClusterLod::default();
    let mut render = |mut view: Snapshot| -> Result<_> {
        view.buffer_atomic64 = gpu.buffer_atomic64();
        view.ray_caps = gpu.ray_capabilities();
        gpu.execute(tech.build(&view)?.compile()?, &pool)
    };
    let first = render(snapshot(&mut extractor, world)?)?;
    let hot = render(snapshot(&mut extractor, world)?)?;
    assert_eq!(first.profile.atlas_upload_bytes, bytes as u64);
    assert_eq!(hot.profile.atlas_upload_bytes, 0);
    world.set_texture(bark, red.clone())?;
    let edited_view = snapshot(&mut extractor, world)?;
    let (edited_gi, region) = hit_region(&edited_view, barks[0])?;
    assert_ne!(edited_gi, original_gi);
    assert_eq!(region, cold_layout[&bark]);
    let edited = render(edited_view)?;
    assert_eq!(edited.profile.atlas_upload_bytes, red.rgba.len() as u64);
    let color_delta = |a: &[u8], b: &[u8]| {
        a.chunks_exact(4)
            .zip(b.chunks_exact(4))
            .filter(|(a, b)| a.iter().zip(*b).take(3).any(|(&a, &b)| a.abs_diff(b) > 8))
            .count()
    };
    let noise = color_delta(&first.pixels, &hot.pixels);
    let changed = color_delta(&hot.pixels, &edited.pixels);
    assert!(
        changed > noise + 8,
        "visible trunk pixels changed {changed}, same-pose baseline {noise}"
    );
    world.set_texture(bark, large)?;
    let grown = snapshot(&mut extractor, world)?;
    let new = Atlas::cached(&grown.scene)?;
    assert_eq!(new.locations[&ground], cold_layout[&ground]);
    assert_eq!(new.locations[&untouched], cold_layout[&untouched]);
    assert_ne!(new.locations[&bark][0], cold_layout[&bark][0]);
    assert_eq!(&new.locations[&bark][1..], &[128, 64]);
    let (larger_gi, region) = hit_region(&grown, barks[0])?;
    assert_ne!(larger_gi, edited_gi);
    assert_eq!(region, new.locations[&bark]);
    for id in [ground, untouched] {
        let base = new.locations[&id][0] as usize;
        let words = grown.scene.textures[&id].rgba.len() / 4;
        for offset in (0..words).step_by(1024).chain([words - 1]) {
            assert_eq!(
                new.pixels.record(base + offset),
                cold_atlas.pixels.record(base + offset)
            );
        }
    }
    let resized = render(grown)?;
    assert_eq!(resized.profile.atlas_upload_bytes, 128 * 64 * 4);
    assert_eq!(
        first.memory.atlas.as_ref().unwrap().allocated_bytes,
        resized.memory.atlas.as_ref().unwrap().allocated_bytes
    );
    world.set_texture(bark, solid(256, 128, [230, 25, 40, 255]))?;
    let beyond = snapshot(&mut extractor, world)?;
    let beyond_bytes = Atlas::cached(&beyond.scene)?.pixels.bytes() as u64;
    let expanded = render(beyond)?;
    assert_eq!(expanded.profile.atlas_upload_bytes, beyond_bytes);
    assert!(
        expanded.memory.atlas.as_ref().unwrap().allocated_bytes
            > resized.memory.atlas.as_ref().unwrap().allocated_bytes
    );
    world.set_texture(bark, white)?;
    let restored = render(snapshot(&mut extractor, world)?)?;
    let restored_delta = color_delta(&hot.pixels, &restored.pixels);
    assert!(
        restored_delta <= noise + hot.pixels.len() / (4 * 2048),
        "restoring the bark changed {restored_delta} pixels; baseline {noise}"
    );
    println!(
        "forest atlas: entities={count}, shared bark instances={bark_users}, extent=384x216, samples=32, p95_ms (cold pack/hot cache/edit cache/edit extraction)={:.5}/{:.5}/{:.5}/{:.5}, upload_B (cold/hot/edit/resize/beyond)={}/{}/{}/{}/{}, device_binding_B (cold/resize/beyond)={}/{}/{}, same-camera changed pixels (any RGB channel >8, repeat/edit/restore)={noise}/{changed}/{restored_delta}",
        p95(&mut cold_cpu),
        p95(&mut hot_cpu),
        p95(&mut edit_cpu),
        p95(&mut extract_cpu),
        first.profile.atlas_upload_bytes,
        hot.profile.atlas_upload_bytes,
        edited.profile.atlas_upload_bytes,
        resized.profile.atlas_upload_bytes,
        expanded.profile.atlas_upload_bytes,
        first.memory.atlas.unwrap().allocated_bytes,
        resized.memory.atlas.unwrap().allocated_bytes,
        expanded.memory.atlas.unwrap().allocated_bytes
    );
    Ok(())
}
