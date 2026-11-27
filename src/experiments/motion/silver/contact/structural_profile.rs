//! Compares a journaled edit with warm full reconciliation on the same 500-actor world.
use super::*;
use crate::render::{Camera, SceneExtractor};

#[test]
#[ignore = "builds 500 Silver Wolf rigs and measures CPU extraction"]
fn profile_structural_extraction() -> Result<()> {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let mut world = population::populated_world(500, &pool, false)?;
    let actor = *world.entities.component_owners(PLAYER).last().unwrap();
    let mut lane = actor;
    while let Some(parent) = world.entities[&lane].parent {
        lane = parent;
    }
    let mut children = BTreeMap::<Id, Vec<Id>>::new();
    for (&id, e) in &world.entities {
        if let Some(p) = e.parent {
            children.entry(p).or_default().push(id);
        }
    }
    let mut lane_rows = vec![(lane, world.entities[&lane].clone())];
    let mut cursor = 0;
    while cursor < lane_rows.len() {
        if let Some(ids) = children.get(&lane_rows[cursor].0) {
            lane_rows.extend(ids.iter().map(|&id| (id, world.entities[&id].clone())));
        }
        cursor += 1;
    }
    let instance = world
        .entities
        .values()
        .find_map(|e| {
            e.instance
                .as_ref()
                .filter(|i| e.skin_binding.is_none() && world.entities[&i.mesh].hierarchy.is_none())
                .cloned()
        })
        .context("missing plain mesh")?;
    let programs = Arc::default();
    let dimensions = [1920, 1080];
    let view = Camera::default();
    println!(
        "actors=500 entities={} actor_lane_rows={}",
        world.entities.len(),
        lane_rows.len()
    );
    for scenario in ["light", "ordinary", "actor_lane"] {
        for baseline in [true, false] {
            let mut x = SceneExtractor::default();
            let mut previous = x.snapshot(&world, view, dimensions, Arc::clone(&programs))?;
            let mut samples = [Vec::new(), Vec::new()];
            let mut visited = [0; 2];
            let mut updated = [0; 2];
            let mut added = 0;
            for i in 0..44 {
                let phase = i % 2;
                if scenario == "actor_lane" {
                    if phase == 0 {
                        for (id, _) in &lane_rows {
                            world.entities.remove(id);
                        }
                    } else {
                        for (id, e) in &lane_rows {
                            world.entities.insert(*id, e.clone());
                        }
                    }
                } else if phase == 0 {
                    let e = if scenario == "ordinary" {
                        Entity {
                            instance: Some(instance.clone()),
                            ..Default::default()
                        }
                    } else {
                        Entity {
                            light: Some(crate::scene::Light::point([1.; 3], 2., 8.)),
                            ..Default::default()
                        }
                    };
                    added = world.spawn(e);
                } else {
                    world.entities.remove(&added);
                }
                // Reproduce the former invalidation at a structural write, keeping the warm
                // extractor and retained in-flight snapshot so shared asset caches are comparable.
                if baseline {
                    let _ = world.entities.values_mut();
                }
                let start = Instant::now();
                let next = x.snapshot(&world, view, dimensions, Arc::clone(&programs))?;
                let elapsed = start.elapsed().as_secs_f64() * 1000.;
                assert_eq!(
                    next.scene.stats.full_rebuild,
                    baseline || scenario == "actor_lane"
                );
                visited[phase] = next.scene.stats.entities_visited;
                updated[phase] = next.scene.stats.instances_updated;
                if !baseline && scenario != "actor_lane" {
                    assert_eq!(visited[phase], 1);
                }
                if i >= 4 {
                    samples[phase].push(elapsed);
                }
                std::hint::black_box(&previous.scene);
                previous = next;
            }
            for (phase, sample) in samples.iter_mut().enumerate() {
                sample.sort_by(f64::total_cmp);
                let add = (phase == 0) != (scenario == "actor_lane");
                println!(
                    "scenario={scenario}, action={}, source={}, n={}, visited={}, updated={}, P50_ms={:.4}, P95_ms={:.4}",
                    if add { "add" } else { "remove" },
                    if baseline { "full" } else { "journal" },
                    sample.len(),
                    visited[phase],
                    updated[phase],
                    sample[sample.len() / 2 - 1],
                    sample[(sample.len() * 95).div_ceil(100) - 1]
                );
            }
        }
    }
    Ok(())
}
