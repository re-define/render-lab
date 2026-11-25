use std::{env, path::PathBuf, process::Command};
fn main() {
    build_ngx();
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rerun-if-changed=windows.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:windows.manifest");
    }
    println!("cargo:rerun-if-env-changed=GLSLC");
    println!("cargo:rerun-if-changed=shaders");
    let shaders = [
        "plain.mesh",
        "skin.comp",
        "outline_edges.comp",
        "visibility.frag",
        "classify.comp",
        "tone_map.comp",
        "taa.comp",
        "allocate.comp",
        "pbr.comp",
        "ibl_material.comp",
        "ibl_sky.comp",
        "mixed_material.comp",
        "restir_reproject.comp",
        "gi12_trace.comp",
        "gi12_cache.comp",
        "gi12_probe.comp",
        "gi12_resolve.comp",
        "gi12_store.comp",
        "restir_surface.comp",
        "restir_initial.comp",
        "restir_spatial.comp",
        "restir_light.comp",
        "restir_temporal_filter.comp",
        "restir_atrous.comp",
        "restir_commit.comp",
        "restir_material.comp",
        "lighting_direct.comp",
        "lighting_gi.comp",
        "lighting_filter.comp",
        "lighting_probe_bake.comp",
        "lighting_material.comp",
        "lighting_shadow.mesh",
        "mega_clusters.comp",
        "mega_shadow.rgen",
        "mega_shadow.rahit",
        "mega_probe.rgen",
        "gi12_mega.rgen",
        "gi12_hit.rmiss",
        "gi12_hit.rchit",
        "mega_probe.rahit",
        "mega_shadow.rmiss",
        "opaque_shadow.rchit",
        "ray_preview.rgen",
        "ray_preview.rmiss",
        "ray_preview.rchit",
        "character.comp",
        "outline_hull.mesh",
        "outline_quad.mesh",
        "outline_image.comp",
        "motion_trajectory.comp",
        "toon.comp",
        "instances.comp",
        "cluster_tables.comp",
        "surface_tail.comp",
        "ordinary_slots.comp",
        "ray_gather.comp",
        "ray_indices.comp",
        "ray_inspect.comp",
        "ray_refit.comp",
        "plant_proxy.comp",
        "bvh.comp",
        "cluster_cull.comp",
        "occlusion.comp",
        "hiz.comp",
        "cluster.mesh",
        "brick.mesh",
        "brick.frag",
        "brick_seed.comp",
        "brick_raster.comp",
        "brick_resolve.comp",
        "brick_compare.comp",
        "brick_clas.comp",
        "overdraw_visible.comp",
        "plants.comp",
        "surface_motion.comp",
        "motion_view.comp",
        "vegetation_material.comp",
        "vegetation_sky.comp",
        "water_integrate.comp",
        "water_surface.comp",
        "river_surface.comp",
        "river_material.comp",
        "river_reflections.comp",
        "dissolve.frag",
    ];
    let variants: &[(&str, &str, &[&str])] = &[
        (
            "plant_proxy_indexed.comp",
            "plant_proxy.comp",
            &["-DINDEXED"],
        ),
        (
            "plant_shadow_detail.comp",
            "plant_proxy.comp",
            &["-DINDEXED", "-DDETAIL"],
        ),
        ("gi12_clear.comp", "gi12_cache.comp", &["-DCLEAR_BATCH"]),
        (
            "gi_woodland_soil.comp",
            "vegetation_material.comp",
            &["-DGI_STUDY", "-DGROUND", "-DKARST_SOIL"],
        ),
        (
            "gi_limestone.comp",
            "vegetation_material.comp",
            &["-DGI_STUDY", "-DGROUND", "-DLIMESTONE"],
        ),
        (
            "gi_foliage.comp",
            "vegetation_material.comp",
            &["-DGI_STUDY"],
        ),
        (
            "gi_bark.comp",
            "vegetation_material.comp",
            &["-DGI_STUDY", "-DBARK"],
        ),
        (
            "gi_soil.comp",
            "vegetation_material.comp",
            &["-DGI_STUDY", "-DGROUND"],
        ),
        (
            "gi_river_soil.comp",
            "vegetation_material.comp",
            &["-DGI_STUDY", "-DGROUND", "-DRIVER_GROUND"],
        ),
        ("dissolve.mesh", "plain.mesh", &["-DDISSOLVE"]),
        ("river_previous.comp", "river_surface.comp", &["-DPREVIOUS"]),
        (
            "river_ground.comp",
            "vegetation_material.comp",
            &["-DRIVER_GROUND", "-DGROUND"],
        ),
        (
            "river_ground_scene.comp",
            "vegetation_material.comp",
            &["-DRIVER_GROUND", "-DGROUND", "-DSCENE_LIGHTING"],
        ),
        ("water_material.comp", "effect_material.comp", &["-DWATER"]),
        ("ore_material.comp", "effect_material.comp", &[]),
        ("stone_material.comp", "effect_material.comp", &["-DSTONE"]),
        (
            "dissolve_material.comp",
            "effect_material.comp",
            &["-DDISSOLVE"],
        ),
        ("skin_previous.comp", "skin.comp", &["-DPOSITION_ONLY"]),
        ("visibility_plant.frag", "visibility.frag", &["-DPLANTS"]),
        (
            "coverage_ordinary.frag",
            "visibility.frag",
            &["-DCOVERAGE_PROBE"],
        ),
        (
            "coverage_plant.frag",
            "visibility.frag",
            &["-DPLANTS", "-DCOVERAGE_PROBE"],
        ),
        ("coverage_brick.frag", "brick.frag", &["-DCOVERAGE_PROBE"]),
        (
            "brick_raster_measure.comp",
            "brick_raster.comp",
            &["-DMEASURE"],
        ),
        (
            "coverage_rect.frag",
            "brick.frag",
            &["-DCOVERAGE_PROBE", "-DRECTANGLE_PROBE"],
        ),
        ("classify_hdr.comp", "classify.comp", &["-DHDR"]),
        ("taa_hdr.comp", "taa.comp", &["-DHDR"]),
        ("taa_debug_hdr.comp", "taa.comp", &["-DHDR", "-DVISUALIZE"]),
        ("taa_store.comp", "taa.comp", &["-DSTORE"]),
        ("taa_store_hdr.comp", "taa.comp", &["-DSTORE", "-DHDR"]),
        ("dlss_input.comp", "dlss.comp", &[]),
        ("dlss_input_hdr.comp", "dlss.comp", &["-DHDR"]),
        ("dlss_output.comp", "dlss.comp", &["-DOUTPUT"]),
        ("scatter.comp", "classify.comp", &["-DSCATTER"]),
        ("pbr_scene.comp", "pbr.comp", &["-DSCENE_LIGHTING"]),
        ("ibl_sky_hdr.comp", "ibl_sky.comp", &["-DHDR"]),
        ("vegetation_sky_hdr.comp", "vegetation_sky.comp", &["-DHDR"]),
        (
            "river_reflections_hdr.comp",
            "river_reflections.comp",
            &["-DHDR"],
        ),
        (
            "restir_material_hdr.comp",
            "restir_material.comp",
            &["-DHDR_OUTPUT"],
        ),
        (
            "ibl_material_hdr.comp",
            "ibl_material.comp",
            &["-DHDR_OUTPUT"],
        ),
        (
            "restir_reproject_stats.comp",
            "restir_reproject.comp",
            &["-DREPROJECTION_STATS"],
        ),
        (
            "mixed_material_ldr.comp",
            "mixed_material.comp",
            &["-DLDR_OUTPUT"],
        ),
        (
            "mixed_toon_ldr.comp",
            "mixed_material.comp",
            &["-DCEL", "-DLDR_OUTPUT"],
        ),
        ("mixed_toon.comp", "mixed_material.comp", &["-DCEL"]),
        ("texture_grid.comp", "pbr.comp", &["-DNEURAL_BASE_COLOR"]),
        ("gaussian_tile.comp", "pbr.comp", &["-DGAUSSIAN_BASE_COLOR"]),
        (
            "gaussian_scan.comp",
            "pbr.comp",
            &["-DGAUSSIAN_BASE_COLOR", "-DGAUSSIAN_SCAN"],
        ),
        (
            "gaussian_bvh.comp",
            "pbr.comp",
            &["-DGAUSSIAN_BASE_COLOR", "-DGAUSSIAN_BVH"],
        ),
        (
            "gaussian_compact_tile.comp",
            "pbr.comp",
            &["-DGAUSSIAN_BASE_COLOR", "-DGAUSSIAN_COMPACT"],
        ),
        (
            "gaussian_compact_bvh.comp",
            "pbr.comp",
            &[
                "-DGAUSSIAN_BASE_COLOR",
                "-DGAUSSIAN_COMPACT",
                "-DGAUSSIAN_BVH",
            ],
        ),
        (
            "gaussian_probe_tile.comp",
            "gaussian_probe.comp",
            &["-DGAUSSIAN_COMPACT"],
        ),
        ("gaussian_probe_fp32.comp", "gaussian_probe.comp", &[]),
        (
            "gaussian_probe_bvh.comp",
            "gaussian_probe.comp",
            &["-DGAUSSIAN_COMPACT", "-DGAUSSIAN_BVH"],
        ),
        ("toon_scene.comp", "toon.comp", &["-DSCENE_LIGHTING"]),
        ("mega_finalize.comp", "mega_clusters.comp", &["-DFINALIZE"]),
        (
            "mega_barycentric.comp",
            "restir_surface.comp",
            &["-DPROBE_BARYCENTRIC"],
        ),
        ("mega_gi.rahit", "mega_shadow.rahit", &["-DGI_SHADOW"]),
        (
            "proxy_shadow.rgen",
            "mega_shadow.rgen",
            &["-DPROXY_SHADOWS"],
        ),
        (
            "inline_shadow.comp",
            "mega_shadow.rgen",
            &[
                "-fshader-stage=compute",
                "-DPROXY_SHADOWS",
                "-DSCENE_RAY_INLINE",
            ],
        ),
        (
            "gi12_inline.comp",
            "gi12_trace.comp",
            &["-DPROXY_SHADOWS", "-DSCENE_RAY_INLINE"],
        ),
        ("gi12_proxy.rgen", "gi12_mega.rgen", &["-DPROXY_SHADOWS"]),
        (
            "gi12_cluster.rchit",
            "gi12_hit.rchit",
            &["-DGI_CLUSTER_HIT"],
        ),
        ("bvh_last.comp", "bvh.comp", &["-DLAST_LEVEL"]),
        ("occlusion_post.comp", "occlusion.comp", &["-DPOST"]),
        ("hiz_depth.comp", "hiz.comp", &["-DSOURCE_DEPTH"]),
        ("cluster_plant.mesh", "cluster.mesh", &["-DPLANTS"]),
        ("occlusion_bricks.comp", "occlusion.comp", &["-DBRICKS"]),
        (
            "occlusion_bricks_post.comp",
            "occlusion.comp",
            &["-DBRICKS", "-DPOST"],
        ),
        ("plants_previous.comp", "plants.comp", &["-DPREVIOUS"]),
        (
            "vegetation_scene.comp",
            "vegetation_material.comp",
            &["-DSCENE_LIGHTING"],
        ),
        (
            "vegetation_bark.comp",
            "vegetation_material.comp",
            &["-DBARK"],
        ),
        (
            "vegetation_bark_scene.comp",
            "vegetation_material.comp",
            &["-DBARK", "-DSCENE_LIGHTING"],
        ),
        (
            "vegetation_ground.comp",
            "vegetation_material.comp",
            &["-DGROUND"],
        ),
        (
            "vegetation_ground_scene.comp",
            "vegetation_material.comp",
            &["-DGROUND", "-DSCENE_LIGHTING"],
        ),
    ];
    let compiler = env::var_os("GLSLC").unwrap_or_else(|| "glslc".into());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for (file, source, defines) in shaders
        .into_iter()
        .map(|name| (name, name, &[][..]))
        .chain(variants.iter().copied())
    {
        // Compile both frame color ABIs for geometry materials that participate in composition.
        let dual_output = matches!(
            source,
            "character.comp"
                | "vegetation_material.comp"
                | "effect_material.comp"
                | "river_material.comp"
        ) || matches!(
            file,
            "pbr.comp" | "pbr_scene.comp" | "toon.comp" | "toon_scene.comp"
        );
        for linear in [false, true]
            .into_iter()
            .take(if dual_output { 2 } else { 1 })
        {
            let suffix = if linear { ".linear.spv" } else { ".spv" };
            let status = Command::new(&compiler)
                .args(linear.then_some("-DHDR_OUTPUT"))
                .args(defines)
                .arg("--target-env=vulkan1.2")
                .arg("-O")
                .arg(PathBuf::from("shaders").join(source))
                .arg("-o")
                .arg(out_dir.join(format!("{file}{suffix}")))
                .status()
                .expect("install Vulkan SDK glslc or set GLSLC");
            assert!(
                status.success(),
                "shader compilation failed for {file}{suffix}"
            );
        }
    }
}

fn build_ngx() {
    println!("cargo:rustc-check-cfg=cfg(render_lab_ngx)");
    println!("cargo:rerun-if-env-changed=RENDER_LAB_NGX_SDK");
    let Some(sdk) = env::var_os("RENDER_LAB_NGX_SDK").map(PathBuf::from) else {
        return;
    };
    assert_eq!(
        env::var("TARGET").as_deref(),
        Ok("x86_64-pc-windows-msvc"),
        "the NGX SDK bridge is built for x64 MSVC"
    );
    println!("cargo:rerun-if-env-changed=RENDER_LAB_VULKAN_INCLUDE");
    println!("cargo:rerun-if-env-changed=VULKAN_SDK");
    let vulkan_headers = env::var_os("RENDER_LAB_VULKAN_INCLUDE")
        .map(PathBuf::from)
        .or_else(|| env::var_os("VULKAN_SDK").map(|p| PathBuf::from(p).join("Include")))
        .expect("set VULKAN_SDK or RENDER_LAB_VULKAN_INCLUDE for the NGX bridge");
    let include = sdk.join("include");
    let libraries = sdk.join("lib/Windows_x86_64/x64");
    let bridge = "src/rhi/ngx_bridge.cpp";
    println!("cargo:rerun-if-changed={bridge}");
    println!("cargo:rerun-if-changed={}", include.display());
    println!(
        "cargo:rerun-if-changed={}",
        libraries.join("nvsdk_ngx_d.lib").display()
    );
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .include(include)
        .include(vulkan_headers)
        .file(bridge)
        .compile("render_lab_ngx");
    println!("cargo:rustc-link-search=native={}", libraries.display());
    println!("cargo:rustc-link-lib=static=nvsdk_ngx_d");
    println!("cargo:rustc-link-lib=advapi32");
    println!("cargo:rustc-link-lib=user32");
    println!("cargo:rustc-cfg=render_lab_ngx");
}
