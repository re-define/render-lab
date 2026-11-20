use std::{env, path::PathBuf, process::Command};
fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rerun-if-changed=windows.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:windows.manifest");
    }
    println!("cargo:rerun-if-env-changed=GLSLC");
    println!("cargo:rerun-if-changed=shaders/materials");
    println!("cargo:rerun-if-changed=shaders/resolve.glsl");
    println!("cargo:rerun-if-changed=shaders/outline.glsl");
    println!("cargo:rerun-if-changed=shaders/cluster.glsl");
    println!("cargo:rerun-if-changed=shaders/cluster_scene.glsl");
    for file in [
        "plain.mesh",
        "visibility.frag",
        "classify.comp",
        "scatter.comp",
        "allocate.comp",
        "pbr.comp",
        "texture_grid.comp",
        "gaussian_tile.comp",
        "gaussian_scan.comp",
        "gaussian_bvh.comp",
        "gaussian_compact_tile.comp",
        "gaussian_compact_bvh.comp",
        "gaussian_probe_tile.comp",
        "gaussian_probe_fp32.comp",
        "gaussian_probe_bvh.comp",
        "character.comp",
        "outline_hull.mesh",
        "outline_quad.mesh",
        "outline_image.comp",
        "toon.comp",
        "instances.comp",
        "bvh.comp",
        "bvh_last.comp",
        "cluster_cull.comp",
        "occlusion.comp",
        "occlusion_post.comp",
        "hiz.comp",
        "cluster.mesh",
    ] {
        let source = PathBuf::from("shaders").join(match file {
            "scatter.comp" => "classify.comp",
            "gaussian_probe_tile.comp" | "gaussian_probe_bvh.comp" | "gaussian_probe_fp32.comp" => {
                "gaussian_probe.comp"
            }
            "texture_grid.comp"
            | "gaussian_tile.comp"
            | "gaussian_scan.comp"
            | "gaussian_bvh.comp"
            | "gaussian_compact_tile.comp"
            | "gaussian_compact_bvh.comp" => "pbr.comp",
            "bvh_last.comp" => "bvh.comp",
            "occlusion_post.comp" => "occlusion.comp",
            _ => file,
        });
        println!("cargo:rerun-if-changed={}", source.display());
        let out = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join(format!("{file}.spv"));
        let status = Command::new(env::var_os("GLSLC").unwrap_or("glslc".into()))
            .args(match file {
                "scatter.comp" => vec!["-DSCATTER"],
                "gaussian_probe_tile.comp" => vec!["-DGAUSSIAN_COMPACT"],
                "gaussian_probe_bvh.comp" => vec!["-DGAUSSIAN_COMPACT", "-DGAUSSIAN_BVH"],
                "texture_grid.comp" => vec!["-DNEURAL_BASE_COLOR"],
                "gaussian_tile.comp" => vec!["-DGAUSSIAN_BASE_COLOR"],
                "gaussian_scan.comp" => vec!["-DGAUSSIAN_BASE_COLOR", "-DGAUSSIAN_SCAN"],
                "gaussian_bvh.comp" => vec!["-DGAUSSIAN_BASE_COLOR", "-DGAUSSIAN_BVH"],
                "gaussian_compact_tile.comp" => vec!["-DGAUSSIAN_BASE_COLOR", "-DGAUSSIAN_COMPACT"],
                "gaussian_compact_bvh.comp" => vec![
                    "-DGAUSSIAN_BASE_COLOR",
                    "-DGAUSSIAN_COMPACT",
                    "-DGAUSSIAN_BVH",
                ],
                "bvh_last.comp" => vec!["-DLAST_LEVEL"],
                "occlusion_post.comp" => vec!["-DPOST"],
                _ => vec![],
            })
            .arg("--target-env=vulkan1.2")
            .arg("-O")
            .arg(&source)
            .arg("-o")
            .arg(out)
            .status()
            .expect("install Vulkan SDK glslc or set GLSLC");
        assert!(status.success(), "shader compilation failed for {file}");
    }
}
