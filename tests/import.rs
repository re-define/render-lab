use anyhow::Result;
use render_lab::{import, scene::*};
use serde_json::json;
use std::{fs, path::Path};

fn fixture(path: &Path) -> Result<()> {
    // Two translated instances share a mesh. One node has an animated translation.
    let data: [f32; 17] = [
        -1., 0., 0., 1., 0., 0., 0., 1., 0., 0., 1., 0., 0., 0., 1., 0., 0.,
    ];
    fs::write(path.join("mesh.bin"), bytemuck::cast_slice(&data))?;
    let document = json!({"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0,1]}],
        "nodes":[{"mesh":0},{"mesh":0,"translation":[2,0,0]}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
        "buffers":[{"byteLength":68,"uri":"mesh.bin"}],
        "bufferViews":[{"buffer":0,"byteLength":36},{"buffer":0,"byteOffset":36,"byteLength":8},{"buffer":0,"byteOffset":44,"byteLength":24}],
        "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[-1,0,0],"max":[1,1,0]},
        {"bufferView":1,"componentType":5126,"count":2,"type":"SCALAR","min":[0],"max":[1]},
        {"bufferView":2,"componentType":5126,"count":2,"type":"VEC3"}],
        "animations":[{"channels":[{"sampler":0,"target":{"node":0,"path":"translation"}}],"samplers":[{"input":1,"output":2,"interpolation":"LINEAR"}]}]});
    fs::write(path.join("scene.gltf"), serde_json::to_vec(&document)?)?;
    Ok(())
}
#[test]
fn gltf_assets_are_shared_and_survive_removal_of_source() -> Result<()> {
    let root = tempfile::tempdir()?;
    let data = root.path().join("source");
    fs::create_dir(&data)?;
    fixture(&data)?;
    let mut w = World::default();
    let roots = import::import(&mut w, &data.join("scene.gltf"))?;
    let instances: Vec<_> = w
        .entities
        .values()
        .filter_map(|e| e.instance.as_ref())
        .collect();
    assert_eq!(instances.len(), 2);
    assert_eq!(instances[0].mesh, instances[1].mesh);
    let track = &w
        .entities
        .values()
        .find_map(|e| e.animation.as_ref())
        .unwrap()[0];
    assert_eq!(track.target, roots[0]);
    assert_eq!(track.times, [0., 1.]);
    let save = root.path().join("saved.json");
    w.save(&save)?;
    fs::remove_file(data.join("mesh.bin"))?;
    assert!(import::import(&mut w, &data.join("scene.gltf")).is_err());
    assert_eq!(
        serde_json::to_value(&w)?,
        serde_json::to_value(World::load(&save)?)?
    );
    Ok(())
}
