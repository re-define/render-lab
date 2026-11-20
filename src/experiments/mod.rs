pub mod cartoon;
pub mod clusters;
pub mod gaussian;
pub mod motion;
pub mod texture;
use crate::{engine::Engine, render::Forward, scene::World};
use anyhow::Result;
use std::sync::Arc;

/// Saved experiment settings are entities, so opening a saved scene restores its systems.
pub fn configure(engine: &mut Engine) -> Result<()> {
    engine.schedule = Default::default();
    engine.technique = if engine
        .world
        .entities
        .values()
        .any(|e| e.components.contains_key(gaussian::SETTING))
    {
        gaussian::register(Arc::make_mut(&mut engine.materials))?;
        engine.camera = gaussian::camera(&engine.world)?;
        Arc::new(gaussian::Gaussian::new(&engine.world)?)
    } else if engine
        .world
        .entities
        .values()
        .any(|e| e.components.contains_key(texture::SETTING))
    {
        texture::register(Arc::make_mut(&mut engine.materials))?;
        engine.camera = texture::camera(&engine.world)?;
        Arc::new(texture::TextureCompression::new(&engine.world)?)
    } else if engine
        .world
        .entities
        .values()
        .any(|e| e.components.contains_key(cartoon::SETTING))
    {
        cartoon::Cartoon::register(Arc::make_mut(&mut engine.materials))?;
        let motion = engine
            .world
            .entities
            .values()
            .any(|e| e.components.contains_key(motion::silver::PLAYER));
        engine.camera = cartoon::camera(&engine.world, if motion { "full" } else { "face" })?;
        if motion {
            engine.camera.distance *= 1.30;
            engine.camera.yaw = 0.62;
            engine.camera.pitch = 0.06;
        }
        Arc::new(cartoon::Cartoon::default())
    } else if engine
        .world
        .entities
        .values()
        .any(|e| e.components.contains_key(clusters::SETTING))
    {
        Arc::new(clusters::ClusterLod::default())
    } else {
        Arc::new(Forward::default())
    };
    if engine
        .world
        .entities
        .values()
        .any(|e| e.components.contains_key(motion::PLAYER))
    {
        engine
            .schedule
            .stage(vec![Arc::new(motion::Matcher::default())]);
    }
    if engine
        .world
        .entities
        .values()
        .any(|e| e.components.contains_key(motion::silver::PLAYER))
    {
        engine
            .schedule
            .stage(vec![Arc::new(motion::silver::SilverMatcher::load()?)]);
    }
    Ok(())
}
pub fn open(world: World) -> Result<Engine> {
    let mut e = Engine::new(world, Arc::new(Forward::default()))?;
    configure(&mut e)?;
    Ok(e)
}
