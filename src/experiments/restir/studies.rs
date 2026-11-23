use super::{DiffuseGi, RayBackend, SETTING, Settings};
use crate::{
    experiments::{cartoon, lighting::scenes::cube, motion, vegetation},
    render::Camera,
    scene::{Entity, GiTexture, Light, Shading, World},
};
use anyhow::Result;
use glam::{Mat4, Vec3};

const SILVER_CAMERA: &str = "lab.gi12.camera";
pub(in crate::experiments) fn camera(world: &World) -> Result<Option<Camera>> {
    let Some((_, value)) = world.first_raw_component(SILVER_CAMERA) else {
        return Ok(None);
    };
    let (target, distance): (Vec3, f32) = serde_json::from_value(value.clone())?;
    Ok(Some(Camera {
        target,
        distance,
        yaw: 0.10,
        pitch: 0.07,
    }))
}
fn enable(world: &mut World, rays: RayBackend) -> Result<()> {
    world.spawn(Entity {
        name: "Diffuse transport comparison".into(),
        components: [(
            SETTING.into(),
            serde_json::to_value(Settings {
                gi: DiffuseGi::Multi,
                rays,
                ..Default::default()
            })?,
        )]
        .into(),
        ..Default::default()
    });
    world.validate()
}
pub fn forest_study() -> Result<World> {
    shade_forest(vegetation::scene()?, RayBackend::Auto)
}

pub fn karst_study() -> Result<World> {
    shade_forest(vegetation::karst_forest()?, RayBackend::Hardware)
}

/// A river reach with the original placements and terrain, sized for per-frame CLAS builds.
pub fn river_shadow_study() -> Result<World> {
    let mut world = vegetation::river_forest()?;
    world.entities.retain(|_, e| {
        let p = e.local.w_axis;
        let bank = crate::experiments::effects::river::shore(p.x, p.z);
        // A trunk and its crown share a location and must survive thinning together.
        let hash = (p.x.to_bits().rotate_left(13) ^ p.z.to_bits()).wrapping_mul(0x85eb_ca6b);
        let distant_sample = (hash ^ (hash >> 16)).is_multiple_of(3);
        match e.name.as_str() {
            "Forest tree" => {
                (-145.0..=55.0).contains(&p.z)
                    && bank < 37.0
                    && ((-69.0..=6.0).contains(&p.z) && bank < 15.0 || distant_sample)
            }
            "Grass tuft" => {
                (-77.0..=0.0).contains(&p.z) && bank < 8.0 && (p.z > -54.0 || distant_sample)
            }
            _ => true,
        }
    });
    shade_forest(world, RayBackend::Auto)
}

fn shade_forest(mut world: World, rays: RayBackend) -> Result<World> {
    for e in world.entities.values_mut() {
        if let Some(m) = &mut e.material
            && let Shading::Custom(program) = &mut m.shading
        {
            match program.as_str() {
                "Vegetation pixels" => *program = "GI foliage".into(),
                "Tree bark" => *program = "GI bark".into(),
                "Riverbank soil" => *program = "GI river soil".into(),
                "Meadow ground" => {
                    *program = "GI soil".into();
                    m.gi_hit.reflectance = Some([0.19, 0.23, 0.085]);
                    m.gi_hit.texture = GiTexture::Constant;
                }
                _ => {}
            }
        }
    }
    enable(&mut world, rays)?;
    Ok(world)
}
pub fn silver_study() -> Result<World> {
    let mut world = motion::silver::scene()?;
    let camera = cartoon::camera(&world, "full")?;
    for e in world.entities.values_mut() {
        e.components.remove(cartoon::SETTING);
        if let Some(m) = &mut e.material {
            m.shading = Shading::Toon;
            // Painted character colors carry diffuse transport even on parts authored as metallic.
            m.gi_hit.reflectance = Some(m.base_color[..3].try_into().unwrap());
            if e.name == "material_5" {
                use crate::scene::{GiHitModel, GiNode::*};
                m.gi_hit.model = GiHitModel::Program {
                    nodes: vec![
                        Texture(GiTexture::Inherit),
                        Luminance(0),
                        Constant([0.14, 0.055, 0.045]),
                        Constant([0.95, 0.75, 0.66]),
                        Mix([2, 3, 1]),
                        Constant([0.; 3]),
                    ],
                    diffuse: 4,
                    emission: 5,
                };
            }
        }
    }
    world.spawn(Entity {
        name: "Character framing".into(),
        components: [(
            SILVER_CAMERA.into(),
            serde_json::to_value((camera.target, camera.distance * 1.25))?,
        )]
        .into(),
        ..Default::default()
    });
    let mesh = super::scene::mesh(&mut world, "Studio panels", cube());
    for (name, position, scale, color) in [
        (
            "Matte floor",
            [0., -0.075, 0.],
            [6., 0.15, 7.],
            [0.59, 0.57, 0.52],
        ),
        (
            "Studio backdrop",
            [0., 1.7, -1.45],
            [5., 3.5, 0.15],
            [0.46, 0.51, 0.56],
        ),
        (
            "Warm bounce card",
            [-1.23, 1.5, 0.],
            [0.12, 3., 3.],
            [0.79, 0.25, 0.095],
        ),
        (
            "Cool bounce card",
            [1.48, 1.5, -0.2],
            [0.12, 3., 2.6],
            [0.13, 0.46, 0.74],
        ),
    ] {
        let mat = super::scene::material(&mut world, name, color);
        world
            .entities
            .get_mut(&mat)
            .unwrap()
            .material
            .as_mut()
            .unwrap()
            .shading = Shading::Pbr;
        super::scene::object(&mut world, mesh, mat, name, position, scale);
    }
    world.spawn(Entity {
        name: "Studio key".into(),
        local: Mat4::from_translation(Vec3::new(-0.78, 2.55, 1.1)),
        light: Some(Light::point([1., 0.92, 0.82], 4.5, 8.)),
        ..Default::default()
    });
    enable(&mut world, RayBackend::Auto)?;
    Ok(world)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{console::Console, engine::Engine, render::SceneExtractor, rhi::Backend};

    #[test]
    fn karst_defaults_and_saved_ray_settings() -> Result<()> {
        let (_, defaults) = super::super::read(&karst_study()?)?;
        assert_eq!(
            (defaults.gi, defaults.rays),
            (DiffuseGi::Multi, RayBackend::Hardware)
        );
        let mut raw = serde_json::to_value(defaults)?;
        raw.as_object_mut().unwrap().remove("rays");
        for (old, expected) in [(true, RayBackend::Hardware), (false, RayBackend::Auto)] {
            raw["hardware_proxy_rays"] = old.into();
            assert_eq!(super::super::decode(&raw)?.rays, expected);
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires VK_KHR_ray_tracing_pipeline and VK_KHR_acceleration_structure"]
    fn hardware_proxies_match_software_visibility_and_update_in_wind() -> Result<()> {
        let mut engine = crate::experiments::open(forest_study()?)?;
        let mut console = Console::default();
        for cmd in [
            "vegetation shadows adaptive",
            "restir gi direct",
            "restir reference",
            "restir denoise off",
            "post off",
        ] {
            console.execute(&mut engine, &[], cmd)?;
        }
        let mut gpu = Backend::open()?;
        let mut extractor = SceneExtractor::default();
        let mut capture = |engine: &Engine| {
            let mut view = extractor.snapshot(
                &engine.world,
                engine.camera,
                [256, 160],
                engine.materials.clone(),
            )?;
            view.buffer_atomic64 = gpu.buffer_atomic64();
            gpu.execute(engine.technique.build(&view)?.compile()?, &engine.tasks)
        };
        let has = |f: &crate::rhi::Frame, name: &str| f.passes.iter().any(|p| p.name == name);
        let software = capture(&engine)?;
        assert!(has(&software, "Refit ray BVH level"));
        console.execute(&mut engine, &[], "restir rays hardware")?;
        let hardware = capture(&engine)?;
        assert!(has(&hardware, "Trace hardware proxy shadow rays"));
        assert!(has(&hardware, "Build triangle BLAS") && has(&hardware, "Build scene TLAS"));
        assert!(
            !has(&hardware, "Refit ray BVH level")
                && !has(&hardware, "Gather shared ray triangles")
        );
        for counter in ["Detailed shadow triangles", "Coarse shadow triangles"] {
            assert!(software.counters[counter] > 0);
            assert_eq!(software.counters[counter], hardware.counters[counter]);
        }
        let different = |a: &[u8], b: &[u8]| {
            a.chunks_exact(4)
                .zip(b.chunks_exact(4))
                .filter(|(p, q)| (0..3).any(|i| p[i].abs_diff(q[i]) > 3))
                .count()
        };
        let n = 256 * 160;
        assert_eq!(software.pixels.len(), n * 4);
        assert_eq!(hardware.pixels.len(), n * 4);
        let mismatch = different(&software.pixels, &hardware.pixels);
        assert!(
            mismatch < n / 100,
            "different RGB pixels at fixed wind phase: {mismatch}/{n}"
        );
        console.execute(&mut engine, &[], "vegetation time 1.7")?;
        let gust = capture(&engine)?;
        assert!(gust.profile.acceleration_updates > 0);
        assert!(different(&hardware.pixels, &gust.pixels) > 0);
        console.execute(&mut engine, &[], "restir gi multi")?;
        let gi = capture(&engine)?;
        assert!(
            has(&gi, "GI-1.2 trace hardware probe paths")
                && has(&gi, "Trace hardware proxy shadow rays")
        );
        assert!(!has(&gi, "Refit ray BVH level") && !has(&gi, "Gather shared ray triangles"));
        assert_eq!(
            gi.passes
                .iter()
                .filter(|p| p.name == "Build scene TLAS")
                .count(),
            1
        );
        assert_eq!(
            console.execute(&mut engine, &[], "restir rays toggle")?,
            "restir rays software"
        );
        let fallback = capture(&engine)?;
        assert!(
            has(&fallback, "GI-1.2 trace diffuse probe paths")
                && has(&fallback, "Refit ray BVH level")
        );
        assert!(!has(&fallback, "Build scene TLAS"));
        assert_eq!(
            console.execute(&mut engine, &[], "restir rays toggle")?,
            "restir rays hardware"
        );
        assert!(has(&capture(&engine)?, "GI-1.2 trace hardware probe paths"));
        Ok(())
    }

    #[test]
    #[ignore = "requires Vulkan 1.4"]
    fn forest_shadow_lod_partitions_all_casters_and_sun_extent_changes_visibility() -> Result<()> {
        let mut engine = crate::experiments::open(forest_study()?)?;
        let mut console = Console::default();
        let sun = *engine
            .world
            .entities
            .iter()
            .find(|(_, e)| e.name == "Late afternoon sun")
            .unwrap()
            .0;
        for command in [
            "restir gi direct",
            "restir reference",
            "restir denoise off",
            "vegetation shadows coarse",
        ] {
            console.execute(&mut engine, &[], command)?;
        }
        console.execute(&mut engine, &[], &format!("light angular-radius {sun} 0"))?;
        let mut gpu = Backend::open()?;
        let mut extractor = SceneExtractor::default();
        let mut capture = |engine: &Engine| {
            let view = extractor.snapshot(
                &engine.world,
                engine.camera,
                [256, 160],
                engine.materials.clone(),
            )?;
            gpu.execute(engine.technique.build(&view)?.compile()?, &engine.tasks)
        };
        let coarse = capture(&engine)?;
        let all = coarse.counters["Coarse shadow instances"];
        assert_eq!(coarse.counters["Detailed shadow instances"], 0);
        console.execute(&mut engine, &[], "vegetation shadows adaptive")?;
        let near = capture(&engine)?;
        let fine = near.counters["Detailed shadow instances"];
        assert!(fine > 0 && fine < all);
        assert_eq!(fine + near.counters["Coarse shadow instances"], all);
        assert!(
            near.counters["Detailed shadow triangles"] + near.counters["Coarse shadow triangles"]
                > coarse.counters["Coarse shadow triangles"]
        );
        let differing = |a: &[u8], b: &[u8]| {
            a.chunks_exact(4)
                .zip(b.chunks_exact(4))
                .filter(|(a, b)| (0..3).any(|i| a[i].abs_diff(b[i]) > 3))
                .count()
        };
        assert!(differing(&near.pixels, &coarse.pixels) > 100);
        console.execute(&mut engine, &[], &format!("light angular-radius {sun} 2"))?;
        let soft = capture(&engine)?;
        assert_eq!(
            soft.counters["Detailed shadow triangles"],
            near.counters["Detailed shadow triangles"]
        );
        assert!(differing(&near.pixels, &soft.pixels) > 100);
        let camera = engine.camera;
        engine.camera.target.x += 2000.;
        let far = capture(&engine)?;
        assert_eq!(far.counters["Detailed shadow instances"], 0);
        assert_eq!(far.counters["Coarse shadow instances"], all);
        engine.camera = camera;
        assert_eq!(
            capture(&engine)?.counters["Detailed shadow instances"],
            fine
        );
        Ok(())
    }
}

#[cfg(test)]
mod mega_tests;
