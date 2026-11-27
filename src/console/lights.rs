use super::*;
use crate::scene::{Entity, Id, Light, LightKind};
use glam::{Mat4, Vec3};

fn vector(values: &[&str]) -> Result<Vec3> {
    let [x, y, z] = values else {
        bail!("expected three numbers")
    };
    let v = Vec3::new(x.parse()?, y.parse()?, z.parse()?);
    ensure!(v.is_finite(), "expected finite coordinates");
    Ok(v)
}

fn color(values: &[&str]) -> Result<[f32; 3]> {
    if values.is_empty() {
        Ok([1.; 3])
    } else {
        Ok(vector(values)?.to_array())
    }
}

pub(super) fn command(engine: &mut Engine, args: &str) -> Result<String> {
    let words: Vec<_> = args.split_whitespace().collect();
    match words.as_slice() {
        ["list"] => {
            let mut descriptions = engine.world.entities.iter().filter_map(|(id, e)| {
                let light = e.light?;
                let kind = match light.kind {
                    LightKind::Directional => format!("sun ({:.2}°)", light.angular_radius_degrees),
                    LightKind::Point { .. } => "point".into(),
                };
                Some(format!(
                    "#{id} {kind}{} {:.2}",
                    if light.enabled { "" } else { " off" },
                    light.intensity
                ))
            });
            let preview = descriptions.by_ref().take(12).collect::<Vec<_>>();
            let remaining = descriptions.count();
            return Ok(if preview.is_empty() {
                "No scene light instances".into()
            } else if remaining == 0 {
                preview.join(" · ")
            } else {
                format!("{} · +{remaining} more", preview.join(" · "))
            });
        }
        ["point", x, y, z, intensity, range, rgb @ ..] => {
            let local = Mat4::from_translation(vector(&[*x, *y, *z])?);
            let light = Light::point(color(rgb)?, intensity.parse()?, range.parse()?);
            return spawn(engine, "Point light", light, local);
        }
        ["sun", x, y, z, intensity, rgb @ ..] => {
            let local = Light::aiming(vector(&[*x, *y, *z])?)?;
            let light = Light::directional(color(rgb)?, intensity.parse()?);
            return spawn(engine, "Directional light", light, local);
        }
        [
            action @ ("move" | "aim" | "power" | "color" | "enable" | "angular-radius"),
            id,
            values @ ..,
        ] => {
            let id: Id = id.parse()?;
            let e = engine
                .world
                .entities
                .get(&id)
                .context("unknown light entity")?;
            let mut light = e.light.context("entity has no light")?;
            let mut local = e.local;
            match (*action, values) {
                ("move", [_, _, _]) => local.w_axis = vector(values)?.extend(1.),
                ("aim", [_, _, _]) => {
                    ensure!(
                        light.kind == LightKind::Directional,
                        "aim requires a directional light"
                    );
                    let orientation = Light::aiming(vector(values)?)?;
                    let (scale, _, position) = local.to_scale_rotation_translation();
                    let (_, rotation, _) = orientation.to_scale_rotation_translation();
                    local = Mat4::from_scale_rotation_translation(scale, rotation, position);
                }
                ("angular-radius", [value]) => {
                    ensure!(
                        light.kind == LightKind::Directional,
                        "angular radius requires a directional light"
                    );
                    light.angular_radius_degrees = value.parse()?;
                }
                ("power", [value]) => light.intensity = value.parse()?,
                ("color", [_, _, _]) => light.color = color(values)?,
                ("enable", ["on"]) => light.enabled = true,
                ("enable", ["off"]) => light.enabled = false,
                _ => bail!(
                    "light angular-radius ID DEGREES · move/aim ID X Y Z · power ID I · color ID R G B · enable ID on/off"
                ),
            }
            Arc::make_mut(&mut engine.world).set_light_instance(id, light, local)?;
            return Ok(format!("Light #{id} updated"));
        }
        _ => (),
    }
    bail!(
        "light list · light point X Y Z I RANGE [R G B] · light sun DX DY DZ I [R G B] · move/aim/power/color/enable/angular-radius ID"
    )
}

fn spawn(engine: &mut Engine, name: &str, light: Light, local: Mat4) -> Result<String> {
    light.validate(local)?;
    let id = Arc::make_mut(&mut engine.world).spawn(Entity {
        name: name.into(),
        local,
        light: Some(light),
        ..Default::default()
    });
    Ok(format!("Created {name} #{id}"))
}
