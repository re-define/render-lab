use super::*;

const BUCKETS: u32 = 4;
const METRICS: &[&str] = &[
    "Brick candidate pixels",
    "Brick LOD rejects",
    "Brick early depth rejects",
    "Brick DDA calls",
    "Brick DDA hits",
    "Brick atomic wins",
    "Mesh brick changed pixels",
    "Compute brick changed pixels",
    "Brick coverage differences",
    "Brick ID differences",
    "Brick depth differences over 1e-5",
    "Brick maximum depth difference bits",
    "Brick ID differences within one depth ULP",
    "Recovered brick candidate pixels",
    "Recovered brick DDA calls",
];

fn local(g: &mut FrameGraph, name: &str, bytes: usize) -> Resource {
    let r = g.resource(
        name,
        Storage::Buffer {
            bytes,
            initial: None,
        },
    );
    g.prefer_local_buffer(r);
    r
}

/// One atomic visibility layer spans the main and recovered lists. Current
/// Hi-Z reads it directly; final visibility and depth are exported once.
pub(super) struct SoftwareRaster {
    pub winners: Resource,
    ranges: Resource,
    metrics: Option<Resource>,
}

impl SoftwareRaster {
    pub fn new(g: &mut FrameGraph, t: &Targets, ranges: Resource, compare: bool) -> Self {
        let [w, h] = g.extent;
        let winners = local(
            g,
            "Voxel depth and hit handles",
            w as usize * h as usize * 8,
        );
        g.pass_in(
            PassPhase::Raster,
            "Main voxel brick visibility seed",
            Op::Compute {
                program: compute(
                    spirv!("brick_seed.comp"),
                    &[(t.depth, Access::ComputeSample), (winners, W)],
                    vec![],
                ),
                groups: [w.div_ceil(8), h.div_ceil(8), 1],
            },
        );
        Self {
            winners,
            ranges,
            metrics: compare
                .then(|| g.input("Voxel software raster counters", vec![0; 4 * METRICS.len()])),
        }
    }

    pub fn trace(
        &self,
        g: &mut FrameGraph,
        scene: &ClusterScene,
        bins: &BrickBins,
        post_depth: Option<Resource>,
        name: &str,
    ) {
        let palette = scene.palette.expect("brick assets carry a plant skeleton");
        let mut bindings = vec![
            (scene.assets, R),
            (scene.instances, R),
            (scene.view, R),
            (bins.jobs.items, R),
            (palette, R),
            (self.ranges, R),
            (self.winners, RW),
        ];
        if let Some(depth) = post_depth {
            bindings.push((depth, Access::ComputeSample));
        }
        if let Some(metrics) = self.metrics {
            bindings.push((metrics, RW));
        }
        let shader: &'static [u8] = match (post_depth.is_some(), self.metrics.is_some()) {
            (false, false) => spirv!("brick_raster.comp"),
            (true, false) => spirv!("brick_raster_post.comp"),
            (false, true) => spirv!("brick_raster_measure.comp"),
            (true, true) => spirv!("brick_raster_post_measure.comp"),
        };
        g.pass_in(
            PassPhase::Raster,
            name,
            Op::ComputeIndirect {
                program: compute(shader, &bindings, vec![]),
                args: bins.jobs.args,
                offset: 16,
            },
        );
    }

    pub fn finish(
        &self,
        g: &mut FrameGraph,
        t: &Targets,
        scene: &ClusterScene,
        main: &BrickBins,
        post: &BrickBins,
    ) {
        let [w, h] = g.extent;
        let pixels = w as usize * h as usize;
        let grid = [w.div_ceil(8), h.div_ceil(8), 1];
        let ids = local(g, "Resolved voxel visibility", pixels * 8);
        let depth = local(g, "Resolved voxel depth", pixels * 4);
        let sample = Access::ComputeSample;
        g.pass_in(
            PassPhase::Raster,
            "Final voxel brick visibility resolve",
            Op::Compute {
                program: compute(
                    spirv!("brick_resolve.comp"),
                    &[
                        (t.visibility, sample),
                        (self.winners, R),
                        (self.ranges, R),
                        (scene.instances, R),
                        (ids, W),
                        (depth, W),
                        (t.depth, sample),
                    ],
                    vec![],
                ),
                groups: grid,
            },
        );
        if let Some(metrics) = self.metrics {
            // Both rasterizers see the same two append lists and the same final
            // opaque triangles. Reference draws are exclusive to comparisons.
            let before = local(g, "Pre-brick visibility IDs", pixels * 8);
            g.pass(
                "Save opaque IDs for raster comparison",
                Op::ReadPixels {
                    image: t.visibility,
                    buffer: before,
                },
            );
            main.draw_mesh(g, t, scene, "Main voxel brick visibility");
            post.draw_mesh(g, t, scene, "Post voxel brick visibility");
            g.pass_in(
                PassPhase::Counters,
                "Compare same-list voxel IDs and depth",
                Op::Compute {
                    program: compute(
                        spirv!("brick_compare.comp"),
                        &[
                            (t.visibility, sample),
                            (t.depth, sample),
                            (before, R),
                            (ids, R),
                            (depth, R),
                            (metrics, RW),
                        ],
                        vec![],
                    ),
                    groups: grid,
                },
            );
            g.pass(
                "Read voxel raster comparison",
                Op::Counters {
                    buffer: metrics,
                    first: 0,
                    labels: METRICS.to_vec(),
                },
            );
        }
        for (name, buffer, image) in [
            (
                "Final voxel brick visibility publish IDs",
                ids,
                t.visibility,
            ),
            ("Final voxel brick visibility publish depth", depth, t.depth),
        ] {
            g.pass_in(PassPhase::Raster, name, Op::WritePixels { buffer, image });
        }
    }
}

pub(super) struct BrickBins {
    jobs: Work,
    pub counts: Option<Resource>,
    capacity: u32,
}

impl BrickBins {
    pub(super) fn new(g: &mut FrameGraph, name: &str, capacity: u32, mesh: bool) -> Option<Self> {
        if capacity == 0 {
            return None;
        }
        let items = g.append_list(name, 16 * capacity as usize);
        let args = arguments(g, &format!("{name} dispatch"), 2);
        let counts = mesh.then(|| {
            let r = g.input(&format!("{name} counts"), vec![0; BUCKETS as usize * 4]);
            g.prefer_local_buffer(r);
            r
        });
        g.prefer_local_buffer(items);
        Some(Self {
            jobs: Work { items, args },
            counts,
            capacity,
        })
    }

    pub(super) fn outputs(&self) -> impl Iterator<Item = (Resource, Access)> {
        [self.jobs.items, self.jobs.args]
            .into_iter()
            .chain(self.counts)
            .map(|r| (r, RW))
    }

    pub(super) fn draw_mesh(
        &self,
        g: &mut FrameGraph,
        t: &Targets,
        scene: &ClusterScene,
        name: &str,
    ) {
        let counts = self.counts.expect("mesh raster needs depth bins");
        let pages = self.capacity.div_ceil(DRAW_PAGE_SIZE);
        let args = arguments(g, &format!("{name} indirect draws"), pages * BUCKETS);
        let sorted = g.resource(
            &format!("{name} sorted jobs"),
            Storage::Buffer {
                bytes: 16 * self.capacity as usize,
                initial: None,
            },
        );
        g.prefer_local_buffer(sorted);
        g.pass_in(
            PassPhase::Culling,
            &format!("{name} depth bins"),
            Op::ComputeIndirect {
                program: compute(
                    spirv!("brick_bins.comp"),
                    &[(self.jobs.items, R), (counts, R), (sorted, W), (args, RW)],
                    bytemuck::bytes_of(&[pages, DRAW_PAGE_SIZE]).to_vec(),
                ),
                args: self.jobs.args,
                offset: 0,
            },
        );
        let palette = scene.palette.expect("brick assets carry a plant skeleton");
        for bucket in 0..BUCKETS {
            for page in 0..pages {
                g.pass_in(
                    PassPhase::Raster,
                    name,
                    Op::Draw {
                        program: Dispatch {
                            shader: Shader::Mesh(spirv!("brick.mesh"), spirv!("brick.frag")),
                            bindings: vec![
                                (scene.assets, Access::MeshFragment),
                                (scene.instances, M),
                                (scene.view, M),
                                (sorted, M),
                                (palette, M),
                                (counts, M),
                            ],
                            push: bytemuck::bytes_of(&[bucket, page * DRAW_PAGE_SIZE]).to_vec(),
                        },
                        color: t.visibility,
                        depth: t.depth,
                        clear: false,
                        count: DrawCount::MeshIndirect {
                            args,
                            offset: 16 * u64::from(bucket * pages + page),
                        },
                    },
                );
            }
        }
    }
}
