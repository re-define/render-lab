use super::*;
use crate::experiments::ibl;
use glam::Mat4;
use std::collections::BTreeMap;

// The two procedural scenery generators produce flat static asset references. Keep character
// skeletons, animation and typed player state in their original world and ID space.
fn append_scenery(world: &mut World, source: World, layer: &str, offset: Vec3) -> Result<()> {
    let ids: BTreeMap<_, _> = source
        .entities
        .keys()
        .map(|id| (*id, world.spawn(Entity::default())))
        .collect();
    for (&id, old) in &source.entities {
        let mut e = old.clone();
        ensure!(
            e.skin.is_none()
                && e.skin_binding.is_none()
                && e.animation.is_none()
                && e.parent.is_none(),
            "expected flat scenery"
        );
        if let Some(i) = &mut e.instance {
            i.mesh = ids[&i.mesh];
            i.material = i.material.map(|m| ids[&m]);
            e.local = Mat4::from_translation(offset) * e.local;
            if layer != "water" || e.name == "Cascade" {
                e.components.insert(LAYER.into(), layer.into());
            }
        }
        if let Some(m) = &mut e.material {
            m.texture = m.texture.map(|t| ids[&t]);
            m.gi_hit.remap_textures(|id| ids[&id]);
        }
        *world.entities.get_mut(&ids[&id]).unwrap() = e;
    }
    Ok(())
}
pub fn scene() -> Result<World> {
    let mut world = motion::silver::scene()?;
    for e in world.entities.values_mut() {
        if let Some(m) = &mut e.material {
            m.shading = Shading::Toon;
        }
        if e.instance.is_some() {
            e.components.insert(LAYER.into(), "actor".into());
        }
        e.components.remove(cartoon::SETTING);
    }
    let actor = world.first_raw_component(motion::silver::PLAYER).unwrap().0;
    world.set_local(
        actor,
        Mat4::from_translation(Vec3::new(1.0, 0.08, 2.4)) * Mat4::from_scale(Vec3::splat(1.35)),
    )?;
    let mut grove = vegetation::scene()?;
    grove.entities.retain(|_, e| {
        let p = e.local.w_axis;
        e.instance.is_none()
            || (e.name == "Forest tree" && p.x.abs() < 16. && p.z > -23.)
            || (e.name == "Grass tuft"
                && p.x.abs() < 10.
                && p.z.abs() < 10.
                && (p.x > 3.2 || p.z < -5.))
    });
    append_scenery(&mut world, grove, "forest", Vec3::ZERO)?;
    append_scenery(
        &mut world,
        effects::water()?,
        "water",
        Vec3::new(-2.1, 0., -1.4),
    )?;
    let mut lights = serde_json::json!({});
    for layer in ["forest", "water", "actor", "ibl", "restir"] {
        lights[layer] = true.into();
    }
    world.spawn(Entity {
        name: "Confluence features".into(),
        components: [(SETTING.into(), lights)].into(),
        ..Default::default()
    });
    let mut env = serde_json::to_value(ibl::Settings::default())?;
    env["exposure"] = (-3.5).into();
    for (key, v) in [
        (
            restir::SETTING,
            serde_json::to_value(restir::Settings::default())?,
        ),
        (ibl::SETTING, env),
    ] {
        world.spawn(Entity {
            name: key.into(),
            components: [(key.into(), v)].into(),
            ..Default::default()
        });
    }
    world.validate()?;
    Ok(world)
}
