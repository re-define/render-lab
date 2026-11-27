use super::*;
use crate::{render::MaterialPrograms, rhi::Backend};

#[test]
#[ignore = "requires Vulkan 1.4 descriptor heaps, KHR pipeline and inline ray queries"]
fn rigid_derived_assets_change_shadows_but_preserve_visual_and_gi_geometry() -> Result<()> {
    use crate::{
        rdg::{AccelerationShape, Op, Storage},
        render::SceneExtractor,
        scene::GeometryAssets,
    };
    let mut world = scene()?;
    let (&aid, row) = world
        .entities
        .iter()
        .find(|(_, e)| e.geometry_assets.is_some())
        .unwrap();
    let products = row.geometry_assets.as_ref().unwrap().as_ref().clone();
    let proxy_id = products.shadow_mesh.unwrap();
    let original = world.entities[&proxy_id].mesh.clone().unwrap();
    // An exaggerated hull makes the change in shadowing visible to every traversal.
    let mut enlarged = original.as_ref().clone();
    for p in &mut enlarged.positions {
        for x in p {
            *x *= 2.;
        }
    }
    world.entities.get_mut(&proxy_id).unwrap().mesh = Some(Arc::new(enlarged));
    let mut materials = MaterialPrograms::default();
    materials.register_lighting(PROGRAM, REGISTRATION.material.display, REGISTRATION.inputs)?;
    let materials = Arc::new(materials);
    let tasks = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut gpu = Backend::open()?;
    let caps = gpu.ray_capabilities();
    ensure!(
        caps.pipeline && caps.inline,
        "requires native RT and inline query"
    );
    let camera = camera("overview")?;
    let mut reader = SceneExtractor::default();
    for rays in [
        RayBackend::Software,
        RayBackend::Hardware,
        RayBackend::Inline,
    ] {
        let mut colors = Vec::new();
        let mut ids = Vec::new();
        let mut visual_software_payloads = Vec::new();
        for proxy in [None, Some(proxy_id)] {
            world.set_geometry_assets(
                aid,
                GeometryAssets {
                    shadow_mesh: proxy,
                    ..products.clone()
                },
            )?;
            let mut view = reader.snapshot(&world, camera, [160, 100], materials.clone())?;
            view.ray_caps = caps;
            let technique = Restir::new();
            let cfg = Settings {
                rays,
                sampling: Sampling::Reference,
                denoise: Denoise::Off,
                lights: 64,
                reference_samples: 16,
                ..Default::default()
            };
            let g = graph::build(&view, &technique, cfg)?;
            colors.push(gpu.execute(g.compile()?, &tasks)?.pixels);
            let g = graph::build(
                &view,
                &technique,
                Settings {
                    gi: DiffuseGi::Single,
                    ..cfg
                },
            )?;
            let bound: Vec<_> = g
                .products
                .ray_surfaces
                .iter()
                .filter(|s| s.shadow_proxy.is_some())
                .collect();
            assert_eq!(bound.len(), if proxy.is_some() { 4 } else { 0 });
            for s in bound {
                assert_eq!(s.geometry.count, 360);
                assert_eq!(s.occluder().geometry.count, 48);
            }
            if rays != RayBackend::Software {
                let tls = g
                    .passes
                    .iter()
                    .filter(|p| matches!(p.op, Op::BuildTlas { .. }))
                    .count();
                assert_eq!(tls, if proxy.is_some() { 2 } else { 1 });
                let counts: Vec<_> = g
                    .resources
                    .iter()
                    .filter_map(|(_, r)| match r {
                        Storage::Acceleration(AccelerationShape::Triangles { count, .. }) => {
                            Some(*count)
                        }
                        _ => None,
                    })
                    .collect();
                assert!(counts.contains(&360));
                assert_eq!(counts.contains(&48), proxy.is_some());
                for pass in &g.passes {
                    if let Op::TraceRays { blases, .. } | Op::RayQuery { blases, .. } = &pass.op {
                        let direct = pass.name.contains("shadow rays");
                        let expected = if direct && proxy.is_some() { 48 } else { 360 };
                        assert!(blases.iter().any(|r|matches!(g.resources[r.0].1,Storage::Acceleration(AccelerationShape::Triangles{count,..}) if count==expected)));
                    }
                }
            } else {
                let name = if proxy.is_some() {
                    "GI visual triangles"
                } else {
                    "Shadow caster triangles"
                };
                let (
                    _,
                    Storage::Buffer {
                        initial: Some(data),
                        ..
                    },
                ) = g.resources.iter().find(|(n, _)| n == name).unwrap()
                else {
                    unreachable!()
                };
                visual_software_payloads.push(data.clone());
            }
            let _ = gpu.execute(g.compile()?, &tasks)?;
            let mut g = graph::build(&view, &technique, cfg)?;
            let image = g.products.visibility.unwrap();
            let output = g.input("raw visibility ID", vec![0; 160 * 100 * 8]);
            g.pass(
                "Read visibility",
                Op::ReadPixels {
                    image,
                    buffer: output,
                },
            );
            g.display_extent = Some([320, 100]); // both 32-bit ID words per pixel
            g.set_output(output);
            ids.push(gpu.execute(g.compile()?, &tasks)?.pixels);
        }
        if rays == RayBackend::Software {
            assert_eq!(visual_software_payloads[0], visual_software_payloads[1]);
        }
        assert_eq!(ids[0], ids[1], "visible IDs changed with {rays:?}");
        let changed = colors[0]
            .chunks_exact(4)
            .zip(colors[1].chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        ensure!(
            changed > 10,
            "shadow proxy did not affect output ({rays:?}, {changed} pixels)"
        );
        println!("{rays:?}: {changed} changed shadowed/color pixels; visibility unchanged");
    }
    Ok(())
}

#[test]
fn replacing_proxy_mesh_contents_invalidates_only_the_shadow_payload() -> Result<()> {
    use crate::{rdg::Storage, render::SceneExtractor};
    let mut world = scene()?;
    let (&asset, row) = world
        .entities
        .iter()
        .find(|(_, e)| e.geometry_assets.is_some())
        .unwrap();
    let products = row.geometry_assets.as_ref().unwrap().as_ref().clone();
    let proxy = products.shadow_mesh.unwrap();
    let mut materials = MaterialPrograms::default();
    materials.register_lighting(PROGRAM, REGISTRATION.material.display, REGISTRATION.inputs)?;
    let materials = Arc::new(materials);
    let technique = Restir::new();
    let mut extract = SceneExtractor::default();
    let mut capture = |w: &World| -> Result<_> {
        let snapshot = extract.snapshot(w, camera("overview")?, [32, 32], materials.clone())?;
        let graph = graph::build(
            &snapshot,
            &technique,
            Settings {
                rays: RayBackend::Software,
                gi: DiffuseGi::Single,
                ..Default::default()
            },
        )?;
        let payload = |r: &Storage| match r {
            Storage::Buffer {
                initial: Some(data),
                ..
            } => data.clone(),
            _ => panic!("expected an uploaded payload"),
        };
        let named = |n: &str| {
            graph
                .resources
                .iter()
                .find(|(name, _)| name == n)
                .map(|(_, r)| payload(r))
        };
        let shadow = named("Shadow caster triangles").unwrap();
        let visual = named("GI visual triangles").unwrap_or_else(|| shadow.clone());
        let proxy_vertices = graph
            .products
            .ray_surfaces
            .iter()
            .find_map(|s| s.shadow_proxy)
            .map(|surface| payload(&graph.resources[surface.vertices.0].1));
        Ok((snapshot, shadow, visual, proxy_vertices))
    };
    let (before, shadow, visual, old_gpu) = capture(&world)?;
    // Keep all entity and binding IDs, the triangle count and the software technique constant.
    let old_mesh = world.entities[&proxy].mesh.as_ref().unwrap().clone();
    let mut replacement = old_mesh.as_ref().clone();
    for p in &mut replacement.positions {
        p[0] += 0.2;
    }
    world.entities.get_mut(&proxy).unwrap().mesh = Some(Arc::new(replacement));
    let (after, new_shadow, new_visual, new_gpu) = capture(&world)?;
    assert!(Arc::ptr_eq(&visual, &new_visual));
    assert_ne!(shadow, new_shadow);
    assert_ne!(old_gpu.unwrap(), new_gpu.unwrap());
    assert_ne!(
        before.scene.geometry_revision,
        after.scene.geometry_revision
    );
    assert!(Arc::ptr_eq(
        &before.scene.meshes[&crate::render::MeshId(proxy)].mesh,
        &old_mesh
    ));
    // Explicitly selecting the visual mesh is also the shared one-BVH configuration.
    world.set_geometry_assets(
        asset,
        crate::scene::GeometryAssets {
            shadow_mesh: Some(products.source_mesh),
            ..products
        },
    )?;
    let (_, shared_shadow, shared_visual, no_override) = capture(&world)?;
    assert!(Arc::ptr_eq(&shared_shadow, &shared_visual) && no_override.is_none());
    Ok(())
}
