//! An indexed stream package with independent texture, cluster and joint-detail chains.
use super::assembly::{Primary, Registration};
use crate::{
    rdg::{Access, Dispatch, DrawCount, Op, Shader, Storage},
    render::{Camera, FrameGraph, Snapshot, Technique},
    scene::{Entity, World},
    streaming::{Budget, Package, Streamer},
};
use anyhow::Result;
use std::sync::{Arc, Mutex};
const KEY: &str = "lab.streaming";
pub(super) const MODULE: Registration = Registration {
    key: KEY,
    root_writers: &[],
    primary: Primary::Exclusive,
    restore: None,
    install: |_, a| {
        a.camera = Some(Camera {
            yaw: 0.,
            pitch: 0.,
            distance: 3.,
            ..Default::default()
        });
        a.technique = Some(Arc::new(Surface(Mutex::new(Streamer::new(
            package()?,
            Budget::default(),
        )?))));
        Ok(())
    },
};
pub fn scene() -> Result<World> {
    let mut w = World::default();
    let mut e = Entity::default();
    e.components.insert(KEY.into(), true.into());
    w.spawn(e);
    Ok(w)
}
fn package() -> Result<Package> {
    let path = crate::assets::cache_dir().join("woven-surface-v1.pages");
    Package::open(&path).or_else(|_| {
        let textures = [8, 64, 256].map(|side| {
            let mut v = Vec::new();
            for y in 0..side {
                for x in 0..side {
                    let fx = x as f32 / side as f32;
                    let fy = y as f32 / side as f32;
                    let weave = 0.5 + 0.5 * (fx * 110.).sin() * (fy * 98.).cos();
                    v.extend([
                        (45. + 170. * fx) as u8,
                        (50. + 120. * weave) as u8,
                        (100. + 110. * fy) as u8,
                        255,
                    ]);
                }
            }
            v
        });
        let geometry = [2, 8, 24].map(|side| {
            let mut v = vec![];
            for y in 0..side {
                for x in 0..side {
                    for (dx, dy) in [(0, 0), (1, 0), (1, 1), (0, 0), (1, 1), (0, 1)] {
                        let u = (x + dx) as f32 / side as f32;
                        let t = (y + dy) as f32 / side as f32;
                        let uv =
                            ((u * 65535.).round() as u32) | (((t * 65535.).round() as u32) << 16);
                        v.extend(uv.to_le_bytes());
                        v.extend((0.1 * (u * 13.).sin() * t).to_le_bytes());
                    }
                }
            }
            v
        });
        let joints = [1, 4, 16].map(|n| {
            (0..n)
                .flat_map(|j| (0.16 * (6. * j as f32 / n as f32).sin()).to_le_bytes())
                .collect()
        });
        Package::cook(&path, vec![textures.into(), geometry.into(), joints.into()])
    })
}
pub struct Surface(Mutex<Streamer>);
impl Technique for Surface {
    fn accepts_temporal_inputs(&self, _: &Snapshot) -> bool {
        false
    }
    fn build(&self, s: &Snapshot) -> Result<FrameGraph> {
        let mut g = FrameGraph::new(s.extent);
        let mut streamer = self.0.lock().unwrap();
        let v = streamer.bind_view(&mut g, s.stream_adapter, !s.secondary_view)?;
        let [Some(tex), Some(geo), Some(skin)] = v.parts.as_slice() else {
            anyhow::bail!("missing surface stream part")
        };
        let offsets = [tex.first_page, geo.first_page, skin.first_page, 0];
        let levels = [tex.level, geo.level, skin.level];
        let sizes = [
            [8, 64, 256][levels[0]],
            [2, 8, 24][levels[1]],
            [1, 4, 16][levels[2]],
            0u32,
        ];
        let mut params = bytemuck::cast_slice(&s.view_projection().to_cols_array()).to_vec();
        params.extend(bytemuck::bytes_of(&offsets));
        params.extend(bytemuck::bytes_of(&sizes));
        let uniform = g.input("Surface parameters", params);
        let vis = g.resource("Stream surface visibility", Storage::Color);
        let depth = g.resource("Stream surface depth", Storage::Depth);
        g.pass(
            "Raster resident geometry and pose",
            Op::Draw {
                program: Dispatch {
                    shader: Shader::Mesh(
                        include_bytes!(concat!(env!("OUT_DIR"), "/stream.mesh.spv")),
                        include_bytes!(concat!(env!("OUT_DIR"), "/stream.frag.spv")),
                    ),
                    bindings: vec![(v.table, Access::Mesh), (uniform, Access::Mesh)],
                    push: vec![],
                },
                color: vis,
                depth,
                clear: true,
                count: DrawCount::Mesh([(2 * sizes[1] * sizes[1]).div_ceil(32), 1, 1]),
            },
        );
        let feedback = g.input("Visible page demand", vec![0; 16]);
        let [w, h] = s.extent;
        let out = g.resource(
            "Streamed material color",
            Storage::Buffer {
                bytes: w as usize * h as usize * 4,
                initial: None,
            },
        );
        g.pass(
            "Resolve streamed texture and request detail",
            Op::Compute {
                program: Dispatch {
                    shader: Shader::Compute(include_bytes!(concat!(
                        env!("OUT_DIR"),
                        "/stream_resolve.comp.spv"
                    ))),
                    bindings: vec![
                        (vis, Access::ComputeSample),
                        (v.table, Access::ComputeRead),
                        (uniform, Access::ComputeRead),
                        (out, Access::ComputeWrite),
                        (feedback, Access::ComputeReadWrite),
                    ],
                    push: vec![],
                },
                groups: [w.div_ceil(8), h.div_ceil(8), 1],
            },
        );
        let progress = g.input(
            "Stream residency",
            bytemuck::bytes_of(&[
                levels[0] as u32,
                levels[1] as u32,
                levels[2] as u32,
                v.uploads as u32,
            ])
            .to_vec(),
        );
        g.pass(
            "Resident detail levels",
            Op::Counters {
                buffer: progress,
                first: 0,
                labels: vec![
                    "texture level",
                    "geometry level",
                    "joint level",
                    "page uploads",
                ],
            },
        );
        if !s.secondary_view {
            streamer.observe_demand(&mut g, feedback);
        }
        g.set_output(out);
        Ok(g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{render::Snapshot, rhi::Backend};
    #[test]
    #[ignore = "requires Vulkan 1.4 descriptor heaps and mesh shaders"]
    fn streamed_pixels_refine_then_release() -> Result<()> {
        let mut gpu = Backend::open()?;
        let tasks = rayon::ThreadPoolBuilder::new().num_threads(3).build()?;
        let mut s = Snapshot::new(
            &scene()?,
            Camera {
                yaw: 0.,
                pitch: 0.,
                distance: 2.5,
                ..Default::default()
            },
            [320, 180],
            Default::default(),
        )?;
        s.stream_adapter = gpu.stream_adapter();
        let t = Surface(Mutex::new(Streamer::new(
            package()?,
            Budget {
                requests: 2,
                upload_pages: 1,
                in_flight: 3,
                pool_pages: 16,
            },
        )?));
        let mut first = vec![];
        let mut fine = None;
        let mut peak = 0;
        let mut blocks = 0;
        let mut copies = 0;
        let mut direct = 0;
        let start = std::time::Instant::now();
        for i in 0.. {
            if start.elapsed().as_secs() > 15 {
                break;
            }
            let f = gpu.execute(t.build(&s)?.compile()?, &tasks)?;
            let levels = ["texture level", "geometry level", "joint level"].map(|k| f.counters[k]);
            if i == 0 {
                assert_eq!(levels, [0; 3]);
                first = f.pixels.clone();
            }
            let n = f.counters["page uploads"];
            assert!(n <= 1);
            copies += n;
            direct += f
                .counters
                .get("stream DirectStorage uploads")
                .copied()
                .unwrap_or(0);
            peak = peak.max(f.memory.managed.device_local.stream_page);
            blocks = blocks.max(f.memory.managed.stream_pool_bytes);
            if levels == [2; 3] {
                fine = Some(f.pixels);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let fine = fine.ok_or_else(|| {
            anyhow::anyhow!(
                "fine page timeout, last I/O error {:?}",
                t.0.lock().unwrap().last_error
            )
        })?;
        use sha2::Digest;
        let digest = sha2::Sha256::digest(&fine);
        let changed = first
            .chunks_exact(4)
            .zip(fine.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 1000 && peak >= 65536);
        let held = t.build(&s)?.compile()?;
        s.camera.distance = 200.;
        for _ in 0..4 {
            let f = gpu.execute(t.build(&s)?.compile()?, &tasks)?;
            assert!(f.memory.managed.device_local.stream_page > 0);
        }
        let coarse = t.build(&s)?.compile()?;
        assert!(gpu.enqueue(held, &tasks, None)?.is_none());
        assert!(gpu.enqueue(coarse, &tasks, None)?.is_none());
        let old = gpu.retire_next().unwrap()?;
        let coarse = gpu.retire_next().unwrap()?;
        assert_eq!(coarse.profile.frames_in_flight, 2);
        assert_eq!(coarse.counters["texture level"], 0);
        assert_eq!(
            old.pixels, fine,
            "queued old address table changed after eviction"
        );
        let mut released = false;
        for _ in 0..8 {
            let f = gpu.execute(t.build(&s)?.compile()?, &tasks)?;
            if f.counters["texture level"] == 0
                && f.memory.managed.device_local.stream_page == 0
                && f.memory.managed.stream_pool_bytes == 0
            {
                released = true;
                break;
            }
        }
        assert!(released, "physical page allocation did not retire");
        drop(t);
        let _ = gpu.retire_ready();
        assert!(
            !gpu.has_stream_allocations(),
            "idle unload retained GPU owners"
        );
        if std::env::var_os("RENDER_LAB_REQUIRE_DIRECTSTORAGE").is_some() {
            assert_eq!(direct, copies);
        }
        eprintln!(
            "stream verification: direct uploads={direct}, changed={changed}, copies={copies}, peak live page bytes={peak}, peak committed block={blocks}, final=0, fine sha256={digest:x}"
        );
        Ok(())
    }
}
