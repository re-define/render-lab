//! The scene selects its independent light producers through ECS settings.
use super::{ibl, restir};
use crate::scene::{Entity, Shading, World};
use anyhow::Result;
pub fn scene() -> Result<World> {
    let mut world = restir::scene()?;
    for entity in world.entities.values_mut() {
        if let Some(material) = &mut entity.material {
            material.shading = if matches!(entity.name.as_str(), "Blue ceramic" | "Iron oxide") {
                Shading::Toon
            } else {
                Shading::Pbr
            };
        }
    }
    world.spawn(Entity {
        name: "Environment light settings".into(),
        components: [(
            ibl::SETTING.into(),
            serde_json::json!({
                "exposure": -4., "yaw": 0., "sky": false, "view": "combined",
            }),
        )]
        .into(),
        ..Default::default()
    });
    world.validate()?;
    Ok(world)
}
