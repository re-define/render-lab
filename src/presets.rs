//! Named experiments and file-backed scenes accepted by the executable.
use anyhow::{Result, bail, ensure};
use render_lab::{
    experiments::{
        cartoon, clusters, composition, effects, gaussian, ibl, lighting, mixed, motion, restir,
        texture, vegetation,
    },
    import,
    scene::World,
};
use std::path::Path;

type SceneBuilder = fn() -> Result<World>;

pub(super) const PRESETS: &[(&str, SceneBuilder)] = &[
    ("clusters", clusters::scene),
    ("vegetation", vegetation::scene),
    ("vegetation-scale", vegetation::forest),
    ("river-forest", vegetation::river_forest),
    ("forest-karst", restir::karst_study),
    ("confluence", composition::scene),
    ("water", effects::water),
    ("destruction", effects::destruction),
    ("motion", motion::silver::scene),
    ("silver-contact", motion::silver::contact::scene),
    ("silver-contact-duo", motion::silver::contact::duo_scene),
    ("toon", cartoon::scene),
    ("texture", texture::scene),
    ("gaussian", || gaussian::scene(None)),
    ("gi", || lighting::scene(lighting::Kind::Gi)),
    ("shadow", || lighting::scene(lighting::Kind::Shadow)),
    ("rt", || lighting::scene(lighting::Kind::Rt)),
    ("restir", restir::scene),
    ("gi12-forest", restir::forest_study),
    ("gi12-silver", restir::silver_study),
    ("ibl", ibl::scene),
    ("mixed", mixed::scene),
];

pub(super) fn load(name: &str, path: Option<&Path>) -> Result<World> {
    let Some(path) = path else {
        if name.is_empty() {
            return Ok(World::demo());
        }
        if let Some((_, build)) = PRESETS.iter().find(|(key, _)| *key == name) {
            return build();
        }
        bail!("unknown experiment {name:?}; use --list-experiments");
    };
    let saved = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"));
    match name {
        "gaussian" => gaussian::scene(Some(path)),
        "toon" if saved => {
            let world = World::load(path)?;
            ensure!(
                world.first_raw_component(cartoon::SETTING).is_some(),
                "scene has no cartoon settings"
            );
            Ok(world)
        }
        "toon" => cartoon::from_gltf(path),
        "" | "clusters" => {
            let mut world = if saved {
                World::load(path)?
            } else {
                let mut world = World::default();
                import::import(&mut world, path)?;
                world
            };
            if name == "clusters" {
                clusters::prepare(&mut world)?;
            }
            Ok(world)
        }
        _ => bail!("--scene supports the default, clusters, toon and gaussian loaders"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn published_experiments_are_unique_and_documented() {
        let catalog = include_str!("../docs/experiment-catalog.md");
        let mut names = BTreeSet::new();
        for (name, _) in PRESETS {
            assert!(names.insert(*name), "duplicate experiment {name}");
            assert!(
                catalog.contains(&format!("`{name}`")),
                "missing experiment {name}"
            );
        }
        assert!(load("forest-mega", None).is_err());
    }
}
