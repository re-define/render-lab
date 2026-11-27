//! One dispatch poses many instances of a mesh. Each output retains its own RDG identity
//! so rasterization, ray builds and temporal consumers keep their ordinary dependencies.
use super::*;
use crate::rdg::Graph;
use anyhow::Context;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    Page(usize),
    Gpu(Resource),
}

#[derive(Default)]
pub(in crate::render) struct SkinBatches {
    active: BTreeMap<(Resource, bool, Source), usize>,
    // Hold each palette until the graph is finalized so its Arc address cannot be recycled.
    palettes: BTreeMap<usize, (Arc<[Mat4]>, usize, u32)>,
    pages: Vec<Vec<[f32; 16]>>,
    batches: Vec<Batch>,
}

struct Batch {
    pass: usize,
    page: Source,
    jobs: Vec<SkinJob>,
    outputs: Vec<Resource>,
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct SkinJob {
    model: [f32; 16],
    root: [f32; 16],
    // The GPU reads the first word as the matrix offset. Pad the std430 array stride.
    offset: [u32; 4],
}

impl SkinBatches {
    fn palette_slot(&mut self, palette: &Arc<[Mat4]>) -> Result<(usize, u32)> {
        let key = palette.as_ptr() as usize;
        if let Some((_, page, offset)) = self.palettes.get(&key) {
            return Ok((*page, *offset));
        }
        const MAX_MATRICES: usize = (16 << 20) / size_of::<Mat4>();
        if self
            .pages
            .last()
            .is_none_or(|p| p.len() + palette.len() > MAX_MATRICES)
        {
            self.pages.push(Vec::new());
        }
        let page = self.pages.len() - 1;
        let offset = self.pages[page].len().try_into()?;
        self.pages[page].extend(palette.iter().map(Mat4::to_cols_array));
        self.palettes.insert(key, (palette.clone(), page, offset));
        Ok((page, offset))
    }
    pub(super) fn add(
        &mut self,
        g: &mut Graph,
        input: MeshInputs,
        vertices: u32,
        [model, root]: [Mat4; 2],
        palette: &Arc<[Mat4]>,
        previous: bool,
    ) -> Result<Resource> {
        let (page, offset) = self.palette_slot(palette)?;
        self.add_source(
            g,
            input,
            vertices,
            [model, root],
            (Source::Page(page), offset),
            previous,
        )
    }

    pub(super) fn add_gpu(
        &mut self,
        g: &mut Graph,
        input: MeshInputs,
        vertices: u32,
        transforms: [Mat4; 2],
        pose: Resource,
        previous: bool,
    ) -> Result<Resource> {
        self.add_source(
            g,
            input,
            vertices,
            transforms,
            (Source::Gpu(pose), 0),
            previous,
        )
    }

    fn add_source(
        &mut self,
        g: &mut Graph,
        input: MeshInputs,
        vertices: u32,
        [model, root]: [Mat4; 2],
        (page, offset): (Source, u32),
        previous: bool,
    ) -> Result<Resource> {
        let influences = input
            .influences
            .context("skinned mesh lacks joint influences")?;
        let key = (input.vertices, previous, page);
        let max_instances = 65_535;
        let index = match self.active.get(&key).copied() {
            Some(i) if self.batches[i].outputs.len() < max_instances => i,
            _ => {
                // Reserve the producer at the first use, before any consumer is registered.
                let groups = vertices.div_ceil(64);
                let pass = g.pass_in(
                    PassPhase::Animation,
                    if previous {
                        "GPU skin previous positions"
                    } else {
                        "GPU skin mesh"
                    },
                    Op::Compute {
                        program: Dispatch {
                            shader: Shader::Compute(if previous {
                                include_bytes!(concat!(env!("OUT_DIR"), "/skin_previous.comp.spv"))
                            } else {
                                include_bytes!(concat!(env!("OUT_DIR"), "/skin.comp.spv"))
                            }),
                            bindings: vec![
                                (input.vertices, Access::ComputeRead),
                                (influences, Access::ComputeRead),
                            ],
                            push: bytemuck::bytes_of(&vertices).to_vec(),
                        },
                        groups: [groups.min(32768), groups.div_ceil(32768), 0],
                    },
                );
                let i = self.batches.len();
                self.batches.push(Batch {
                    pass,
                    page,
                    jobs: Vec::new(),
                    outputs: Vec::new(),
                });
                self.active.insert(key, i);
                i
            }
        };
        let out = g.local_buffer(
            if previous {
                "Previous GPU-skinned positions"
            } else {
                "GPU-skinned vertices"
            },
            (vertices as usize * if previous { 16 } else { 32 }).max(16),
        );
        let batch = &mut self.batches[index];
        batch.jobs.push(SkinJob {
            model: model.to_cols_array(),
            root: root.to_cols_array(),
            offset: [offset, 0, 0, 0],
        });
        batch.outputs.push(out);
        Ok(out)
    }

    pub(in crate::render) fn finish(self, g: &mut Graph) -> Vec<Resource> {
        let pages: Vec<_> = self
            .pages
            .into_iter()
            .map(|records| g.table("Skin pose matrices", &crate::rdg::rows::Rows::new(&records)))
            .collect();
        for batch in self.batches {
            let instances = batch.outputs.len() as u32;
            let jobs = g.input(
                "Skin part transforms and palette offsets",
                bytemuck::cast_slice(&batch.jobs).to_vec(),
            );
            let outputs = g.compute_output_table(batch.outputs);
            let Op::Compute { program, groups } = &mut g.passes[batch.pass].op else {
                unreachable!()
            };
            groups[2] = instances;
            program.bindings.extend([
                (
                    match batch.page {
                        Source::Page(i) => pages[i],
                        Source::Gpu(r) => r,
                    },
                    Access::ComputeRead,
                ),
                (outputs, Access::ComputeRead),
                (jobs, Access::ComputeRead),
            ]);
        }
        pages
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two vertices per part exercise both the weighted joint transform and zero-weight
    // fallback. Reuse the asset for two actors with entirely different poses and history.
    fn two_actors(root_shift: f32) -> Result<(FramePlan, Vec<f32>)> {
        let mut g = FrameGraph::new([48, 1]);
        let vertices: [[f32; 8]; 2] = [[1., 0., 0., 0.25, 0., 1., 0., 0.75]; 2];
        let joints = [[0, 0, 0, 0, 1_f32.to_bits(), 0, 0, 0], [0; 8]];
        let mesh = MeshInputs {
            vertices: g.input("mesh", bytemuck::cast_slice(&vertices).to_vec()),
            faces: g.input("faces", vec![0; 16]),
            influences: Some(g.input("influences", bytemuck::cast_slice(&joints).to_vec())),
            last_joint: Some(0),
        };
        let readback = g.input("result", vec![0; 48 * 4]);
        let mut expected = Vec::new();
        for actor in [0., 100.] {
            for previous in [false, true] {
                let dx = root_shift + actor + if previous { 10. } else { 20. };
                let translate = |x| Mat4::from_translation(Vec3::new(x, 0., 0.));
                let out = g.skins.add(
                    &mut g.graph,
                    mesh,
                    2,
                    [translate(dx + 2.), translate(dx - 3.)],
                    &Arc::from([translate(3.)]),
                    previous,
                )?;
                let offset = expected.len() as u32;
                for position in [dx + 1., dx + 3.] {
                    if previous {
                        expected.extend([position, 0., 0., 1.]);
                    } else {
                        expected.extend([position, 0., 0., 0.25, 0., 1., 0., 0.75]);
                    }
                }
                g.pass(
                    "Copy posed vertices",
                    Op::Compute {
                        program: Dispatch {
                            shader: Shader::Compute(include_bytes!(concat!(
                                env!("OUT_DIR"),
                                "/surface_tail.comp.spv"
                            ))),
                            bindings: vec![
                                (out, Access::ComputeRead),
                                (readback, Access::ComputeReadWrite),
                            ],
                            push: bytemuck::bytes_of(&[offset, expected.len() as u32 - offset])
                                .to_vec(),
                        },
                        groups: [1; 3],
                    },
                );
            }
        }
        g.set_output(readback);
        Ok((g.compile()?, expected))
    }

    #[test]
    fn shared_skin_with_different_part_transforms_uploads_one_palette() -> Result<()> {
        let mut skins = SkinBatches::default();
        let p: Arc<[Mat4]> = Arc::from([Mat4::IDENTITY, Mat4::from_scale(Vec3::splat(2.))]);
        let mut g = Graph::new([1, 1]);
        for mesh in 0..2 {
            let inputs = MeshInputs {
                vertices: g.input("vertices", vec![0; 32]),
                faces: g.input("faces", vec![0; 16]),
                influences: Some(g.input("weights", vec![0; 32])),
                last_joint: Some(1),
            };
            skins.add(
                &mut g,
                inputs,
                1,
                [
                    Mat4::from_translation(Vec3::X * mesh as f32),
                    Mat4::IDENTITY,
                ],
                &p,
                false,
            )?;
        }
        assert_eq!(skins.pages.len(), 1);
        assert_eq!(skins.pages[0].len(), 2);
        assert_eq!(skins.batches.len(), 2);
        assert_eq!(
            skins.batches[0].jobs[0].offset,
            skins.batches[1].jobs[0].offset
        );
        assert_ne!(
            skins.batches[0].jobs[0].model,
            skins.batches[1].jobs[0].model
        );
        Ok(())
    }
    #[test]
    fn two_actors_share_dispatches_and_keep_output_dependencies() -> Result<()> {
        let (plan, _) = two_actors(0.)?;
        let producers: Vec<_> = plan
            .graph
            .passes
            .iter()
            .enumerate()
            .filter(|(_, p)| p.phase == PassPhase::Animation)
            .collect();
        assert_eq!(producers.len(), 2);
        for (producer, pass) in producers {
            assert!(matches!(
                pass.op,
                Op::Compute {
                    groups: [1, 1, 2],
                    ..
                }
            ));
            let writes: Vec<_> = plan.pass_uses[producer]
                .iter()
                .filter(|(_, a)| *a == Access::ComputeWrite)
                .collect();
            assert_eq!(writes.len(), 2);
            for &(out, _) in writes {
                let consumer = plan
                    .steps
                    .iter()
                    .find(|step| plan.pass_uses[step.index].contains(&(out, Access::ComputeRead)))
                    .unwrap();
                assert!(consumer.parents.contains(&producer));
            }
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires Vulkan 1.4 and descriptor heaps"]
    fn batched_gpu_skin_matches_both_actors_and_their_previous_poses() -> Result<()> {
        let mut backend = crate::rhi::Backend::open()?;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        for tick in 0..4 {
            let (plan, expected) = two_actors(tick as f32 * 3.)?;
            let frame = backend.execute(plan, &pool)?;
            let actual: Vec<f32> = frame
                .pixels
                .chunks_exact(4)
                .map(|word| f32::from_ne_bytes(word.try_into().unwrap()))
                .collect();
            assert_eq!(actual, expected);
            if tick == 0 {
                assert_eq!(frame.profile.skin_pose_upload_bytes, 4 * 64);
            }
            if tick >= 2 {
                assert_eq!(frame.profile.skin_pose_upload_bytes, 0);
            }
        }
        Ok(())
    }
}
