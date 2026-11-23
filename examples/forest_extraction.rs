//! CPU-only extraction benchmark using the actual vegetation-scale world.
use anyhow::{Context, Result, ensure};
use render_lab::{
    experiments::vegetation,
    render::{Camera, MaterialId, MaterialPrograms, SceneExtractor},
};
use std::{collections::VecDeque, hint::black_box, sync::Arc, time::Instant};

fn main() -> Result<()> {
    let mut world = vegetation::forest()?;
    let tree_rows: Vec<_> = world
        .entities
        .iter()
        .filter(|(_, e)| e.name == "Forest tree")
        .collect();
    let transform = tree_rows.first().context("forest has no trees")?.1.local;
    let parts: Vec<_> = tree_rows
        .iter()
        .filter(|(_, e)| e.local == transform)
        .map(|row| *row.0)
        .collect();
    ensure!(parts.len() == 2, "expected a branch and canopy pair");
    let bark = world.entities[&parts[0]]
        .instance
        .as_ref()
        .and_then(|i| i.material)
        .context("missing bark material")?;
    let material = world.entities[&bark]
        .material
        .clone()
        .context("missing bark")?;
    let users = world
        .entities
        .values()
        .filter(|e| e.instance.as_ref().and_then(|i| i.material) == Some(bark))
        .count();
    eprintln!(
        "entities={} instances={} trees={} grass={} bark_users={users}",
        world.entities.len(),
        world
            .entities
            .values()
            .filter(|e| e.instance.is_some())
            .count(),
        tree_rows.len() / 2,
        world
            .entities
            .values()
            .filter(|e| e.name == "Grass tuft")
            .count()
    );
    println!("run,scenario,sample,visited,updated,extract_ns");
    let programs = Arc::new(MaterialPrograms::default());
    for run in 0..3 {
        for phase in 0..3 {
            let mode = (run + phase) % 3;
            let name = ["camera", "tree", "bark"][mode];
            let mut extractor = SceneExtractor::default();
            let mut camera = Camera::default();
            let mut retained = VecDeque::new();
            for n in 0..=230 {
                let offset = if n % 2 == 1 { 0.25 } else { 0.0 };
                match mode {
                    0 => camera.yaw += 0.001,
                    1 => {
                        for id in &parts {
                            world.entities.get_mut(id).unwrap().local.w_axis.x =
                                transform.w_axis.x + offset;
                        }
                    }
                    _ => {
                        world
                            .entities
                            .get_mut(&bark)
                            .unwrap()
                            .material
                            .as_mut()
                            .unwrap()
                            .roughness = material.roughness - offset
                    }
                }
                let start = Instant::now();
                let snapshot = extractor.snapshot(&world, camera, [800, 500], programs.clone())?;
                let ns = start.elapsed().as_nanos();
                let stats = snapshot.scene.stats;
                if n > 30 {
                    println!(
                        "{run},{name},{},{},{},{ns}",
                        n - 31,
                        stats.entities_visited,
                        stats.instances_updated
                    );
                }
                for id in &parts {
                    let instances = snapshot.scene.instances();
                    let i = instances.binary_search_by_key(id, |item| item.id).unwrap();
                    ensure!(
                        instances[i].model == world.entities[id].local,
                        "stale tree transform"
                    );
                }
                ensure!(
                    snapshot.scene.materials()[&MaterialId(bark)].as_ref()
                        == world.entities[&bark].material.as_ref().unwrap(),
                    "stale shared material"
                );
                black_box(&snapshot);
                retained.push_back(snapshot);
                if retained.len() > 2 {
                    retained.pop_front();
                }
            }
        }
    }
    Ok(())
}
