use super::*;

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

/// Seed once from complete opaque triangle depth, then resolve the combined brick list.
pub(super) struct SoftwareRaster {
    winners: Resource,
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
            "Final voxel brick visibility seed",
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

    pub fn trace(&self, g: &mut FrameGraph, scene: &ClusterScene, jobs: &BrickJobs) {
        let palette = scene.palette.expect("brick assets carry a plant skeleton");
        let mut bindings = vec![
            (scene.assets, R),
            (scene.instances, R),
            (scene.view, R),
            (jobs.items, R),
            (palette, R),
            (self.ranges, R),
            (self.winners, RW),
        ];
        if let Some(metrics) = self.metrics {
            bindings.push((metrics, RW));
        }
        let shader: &'static [u8] = if self.metrics.is_some() {
            spirv!("brick_raster_measure.comp")
        } else {
            spirv!("brick_raster.comp")
        };
        for (push, offset) in jobs.dispatches() {
            g.pass_in(
                PassPhase::Raster,
                "Final voxel brick visibility trace",
                Op::ComputeIndirect {
                    program: compute(shader, &bindings, push),
                    args: jobs.args,
                    offset,
                },
            );
        }
    }

    pub fn finish(&self, g: &mut FrameGraph, t: &Targets, scene: &ClusterScene, jobs: &BrickJobs) {
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
            // Replay the identical combined list against the same opaque triangles.
            let before = local(g, "Pre-brick visibility IDs", pixels * 8);
            g.pass(
                "Save opaque IDs for raster comparison",
                Op::ReadPixels {
                    image: t.visibility,
                    buffer: before,
                },
            );
            jobs.draw_mesh(g, t, scene);
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

pub(super) struct BrickJobs {
    items: Resource,
    args: Resource,
    pages: u32,
}

impl BrickJobs {
    pub(super) fn new(g: &mut FrameGraph, capacity: u32) -> Option<Self> {
        if capacity == 0 {
            return None;
        }
        let items = g.append_list("Visible voxel brick jobs", 16 * capacity as usize);
        g.prefer_local_buffer(items);
        let pages = capacity.div_ceil(DRAW_PAGE_SIZE);
        let args = arguments(g, "Voxel brick indirect arguments", pages);
        Some(Self { items, args, pages })
    }

    pub(super) fn outputs(&self) -> [(Resource, Access); 2] {
        [(self.items, RW), (self.args, RW)]
    }

    fn dispatches(&self) -> impl Iterator<Item = (Vec<u8>, u64)> {
        (0..self.pages).map(|page| {
            (
                bytemuck::bytes_of(&(page * DRAW_PAGE_SIZE)).to_vec(),
                16 * u64::from(page),
            )
        })
    }

    pub(super) fn draw_mesh(&self, g: &mut FrameGraph, t: &Targets, scene: &ClusterScene) {
        let palette = scene.palette.expect("brick assets carry a plant skeleton");
        for (push, offset) in self.dispatches() {
            g.pass_in(
                PassPhase::Raster,
                "Final voxel brick visibility",
                Op::Draw {
                    program: Dispatch {
                        shader: Shader::Mesh(spirv!("brick.mesh"), spirv!("brick.frag")),
                        bindings: vec![
                            (scene.assets, Access::MeshFragment),
                            (scene.instances, M),
                            (scene.view, M),
                            (self.items, M),
                            (palette, M),
                        ],
                        push,
                    },
                    color: t.visibility,
                    depth: t.depth,
                    clear: false,
                    count: DrawCount::MeshIndirect {
                        args: self.args,
                        offset,
                    },
                },
            );
        }
    }
}
