//! The frame builder supplies typed inputs. A backend declares its own RDG passes.
use super::{Camera, RenderInstance, RenderScene, Snapshot, Technique};
use crate::render::FrameGraph;
use crate::{rdg::Resource, scene::Id};
use anyhow::Result;
use std::sync::Arc;
mod ngx;
mod taa;
use ngx::Nvidia;
use taa::Taa;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Off,
    Taa,
    Dlss,
    Dlaa,
}

impl Mode {
    pub fn uses_ngx(self) -> bool {
        matches!(self, Self::Dlss | Self::Dlaa)
    }
}

#[derive(Clone, Copy, Default)]
pub struct Settings {
    mode: Mode,
    view: DebugView,
    revision: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum DebugView {
    #[default]
    Off,
    Motion,
    Reprojection,
}

impl Settings {
    pub fn mode(self) -> Mode {
        self.mode
    }

    pub fn select(&mut self, mode: Mode) {
        if self.mode != mode {
            self.mode = mode;
            self.reset();
        }
    }

    pub fn reset(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn view(self) -> DebugView {
        self.view
    }

    pub fn select_view(&mut self, view: DebugView) {
        if self.view != view {
            self.view = view;
            self.reset();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorEncoding {
    Bgra8,
    LinearRgba32,
}

#[derive(Clone, Copy)]
pub(crate) struct ColorInput {
    pub resource: Resource,
    pub encoding: ColorEncoding,
}

pub(crate) struct Inputs {
    pub color: ColorInput,
    pub depth: Resource,
    /// See motion.glsl. Vectors are current to previous in unjittered render pixels.
    pub motion: Resource,
}

trait Backend {
    fn resolve(
        &self,
        graph: &mut FrameGraph,
        frame: &TemporalFrame,
        inputs: Inputs,
    ) -> Result<ColorInput>;
}

pub(crate) fn resolve(
    graph: &mut FrameGraph,
    frame: &TemporalFrame,
    inputs: Inputs,
) -> Result<ColorInput> {
    if frame.mode.uses_ngx() {
        Nvidia.resolve(graph, frame, inputs)
    } else {
        Taa.resolve(graph, frame, inputs)
    }
}

pub(crate) struct TemporalFrame {
    key: u64,
    color_key: u64,
    pub jitter: [f32; 2],
    pub(crate) jitter_delta: [f32; 2],
    pub(super) output_extent: [u32; 2],
    mode: Mode,
    debug: DebugView,
    reset: bool,
    pub(crate) previous_camera: Camera,
    pub(crate) previous_scene: Option<Arc<RenderScene>>,
    stationary_camera: bool,
    moving: Vec<Id>,
}

impl TemporalFrame {
    pub(crate) fn generation_key(&self) -> u64 {
        self.key
    }
    pub(super) fn has_post(&self) -> bool {
        self.mode != Mode::Off || self.debug != DebugView::Off
    }

    pub fn moved(&self, id: Id) -> bool {
        self.moving.binary_search(&id).is_ok()
    }

    pub(super) fn previous_instance<'a>(
        &'a self,
        current: &RenderScene,
        instance: &RenderInstance,
    ) -> Option<(usize, &'a RenderInstance)> {
        let scene = self.previous_scene.as_ref()?;
        let index = scene
            .instances
            .binary_search_by_key(&instance.id, |i| i.id)
            .ok()?;
        let prior = &scene.instances[index];
        if prior.incarnation != instance.incarnation
            || prior.mesh != instance.mesh
            || !current.meshes[&instance.mesh].same_motion_geometry(&scene.meshes[&prior.mesh])
            || prior.palette.is_some() != instance.palette.is_some()
        {
            return None;
        }
        Some((index, prior))
    }
}

struct Previous {
    jitter: [f32; 2],
    technique: Arc<dyn Technique>,
    scene: Arc<RenderScene>,
    camera: Camera,
    extent: [u32; 2],
    revision: u64,
}

struct PreviousDeformation {
    scene: Arc<RenderScene>,
    technique: Arc<dyn Technique>,
}

#[derive(Default)]
pub(crate) struct Sequence {
    previous: Option<Previous>,
    deformation: Option<PreviousDeformation>,
    generation: u64,
    sample: usize,
}

impl Sequence {
    pub fn assign(
        &mut self,
        settings: Settings,
        technique: &Arc<dyn Technique>,
        view: &mut Snapshot,
    ) {
        self.assign_post(settings, technique, view);
        let prior = self.deformation.take();
        view.previous_deformation = None;
        if crate::experiments::has_deformation(&view.scene) {
            view.previous_deformation = prior
                .filter(|f| Arc::ptr_eq(&f.technique, technique))
                .map(|f| f.scene);
            self.deformation = Some(PreviousDeformation {
                scene: view.scene.clone(),
                technique: technique.clone(),
            });
        }
    }

    fn assign_post(
        &mut self,
        settings: Settings,
        technique: &Arc<dyn Technique>,
        view: &mut Snapshot,
    ) {
        let output_extent = view.output_extent();
        view.temporal = None;
        view.extent = output_extent;
        if (settings.mode == Mode::Off
            && settings.view == DebugView::Off
            && !technique.needs_surface_motion(view))
            || super::rays::view(view).is_some()
            || !technique.accepts_temporal_inputs(view)
        {
            self.previous = None;
            return;
        }
        let mode = if settings.view != DebugView::Off {
            Mode::Off
        } else if settings.mode.uses_ngx() && (output_extent[0] < 96 || output_extent[1] < 64) {
            Mode::Taa
        } else {
            settings.mode
        };
        if mode == Mode::Dlss {
            view.extent = output_extent.map(|dimension| dimension.saturating_mul(2).div_ceil(3));
        }

        let camera = view.camera;
        let direction = |camera: Camera| (camera.target - camera.eye()).normalize_or_zero();
        let previous = self.previous.as_ref().filter(|prior| {
            prior.revision == settings.revision
                && prior.extent == output_extent
                && Arc::ptr_eq(&prior.technique, technique)
                && camera.eye().distance(prior.camera.eye()) < camera.distance.max(1.) * 0.5
                && direction(camera).dot(direction(prior.camera)) > 0.82
                && (0.5..2.).contains(&(camera.distance / prior.camera.distance))
        });
        if previous.is_none() {
            self.generation = self.generation.wrapping_add(1);
            self.sample = 0;
        }
        let stationary_camera = previous.is_some_and(|prior| {
            camera.eye().abs_diff_eq(prior.camera.eye(), 0.00001)
                && camera.target.abs_diff_eq(prior.camera.target, 0.00001)
        });
        let mut moving = Vec::new();
        if let Some(prior) = previous {
            let current = &view.scene.instances;
            for index in current.changed(&prior.scene.instances) {
                let instance = &current[index];
                let previous = if current.same_layout(&prior.scene.instances) {
                    Some(index)
                } else {
                    prior
                        .scene
                        .instances
                        .binary_search_by_key(&instance.id, |i| i.id)
                        .ok()
                };
                if previous.is_none_or(|i| {
                    let last = &prior.scene.instances[i];
                    last.transform_version != instance.transform_version
                        || last.mesh != instance.mesh
                }) {
                    moving.push(instance.id);
                }
            }
        }

        let radical_inverse = |mut index: usize, base| {
            let mut value = 0.;
            let mut place = 1.;
            while index != 0 {
                place /= base as f32;
                value += (index % base) as f32 * place;
                index /= base;
            }
            value
        };
        let index = self.sample + 1;
        let jitter = if mode != Mode::Off && settings.view == DebugView::Off {
            [
                radical_inverse(index, 2) - 0.5,
                radical_inverse(index, 3) - 0.5,
            ]
        } else {
            [0.; 2]
        };
        let key = 0x5441410000000000 | (self.generation & 0x000000ffffffffff);
        let color_revision = technique.color_revision(&view.scene);
        let reset_color =
            previous.is_none_or(|prior| technique.color_revision(&prior.scene) != color_revision);
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&(key, color_revision), &mut hash);
        view.temporal = Some(TemporalFrame {
            key,
            color_key: std::hash::Hasher::finish(&hash),
            jitter,
            jitter_delta: previous.map_or([0.; 2], |old| {
                [old.jitter[0] - jitter[0], old.jitter[1] - jitter[1]]
            }),
            output_extent,
            mode,
            debug: settings.view,
            reset: reset_color,
            previous_camera: previous.map_or(camera, |prior| prior.camera),
            previous_scene: previous.map(|prior| prior.scene.clone()),
            stationary_camera,
            moving,
        });
        self.sample = (self.sample + 1) % if mode == Mode::Dlss { 18 } else { 8 };
        self.previous = Some(Previous {
            jitter,
            technique: technique.clone(),
            scene: view.scene.clone(),
            camera,
            extent: output_extent,
            revision: settings.revision,
        });
    }

    pub fn discard(&mut self) {
        self.previous = None;
        self.deformation = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{rdg::Op, render::Forward, scene::World};

    #[test]
    fn frontend_resets_jitter_on_a_cut_and_exposes_resolved_output() -> Result<()> {
        let world = World::demo();
        let technique: Arc<dyn Technique> = Arc::new(Forward::default());
        let mut context = Sequence::default();
        let mut settings = Settings::default();
        settings.select(Mode::Taa);
        let mut build = |camera, extent| -> Result<_> {
            let mut view = Snapshot::new(&world, camera, extent, Default::default())?;
            context.assign(settings, &technique, &mut view);
            let frame = view.temporal.as_ref().unwrap();
            let mut graph = technique.build(&view)?;
            let output = graph.present()?;
            let plan = graph.compile()?;
            let resolve = plan
                .graph()
                .passes
                .iter()
                .position(|pass| pass.name == "Temporal AA resolve")
                .unwrap();
            let present = plan
                .steps()
                .iter()
                .find(|step| matches!(plan.graph().passes[step.index].op, Op::Present(_)))
                .unwrap();
            assert!(present.parents.contains(&resolve));
            assert_eq!(plan.graph().resources[output.0].0, "Temporal AA color");
            Ok((frame.key, frame.jitter))
        };
        let camera = Camera::default();
        let first = build(camera, [128, 80])?;
        let second = build(camera, [128, 80])?;
        assert_eq!(first.0, second.0);
        assert_ne!(first.1, second.1);
        let mut moved = camera;
        moved.look(2., 0.);
        let cut = build(moved, [128, 80])?;
        assert_ne!(cut.0, second.0);
        assert_eq!(cut.1, first.1);
        let resized = build(moved, [130, 81])?;
        assert_ne!(resized.0, cut.0);
        assert_eq!(resized.1, first.1);
        Ok(())
    }

    #[test]
    #[ignore = "requires Vulkan 1.4 and VK_EXT_descriptor_heap"]
    fn temporal_history_reduces_static_subpixel_flicker() -> Result<()> {
        use crate::rhi::Backend;
        let world = World::demo();
        let technique: Arc<dyn Technique> = Arc::new(Forward::default());
        let mut extractor = crate::render::SceneExtractor::default();
        let mut sequence = Sequence::default();
        let mut settings = Settings::default();
        settings.select(Mode::Taa);
        let mut gpu = Backend::open()?;
        let mut raw_gpu = Backend::open()?;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
        let mut samples: [Vec<Vec<u8>>; 2] = Default::default();
        for i in 0..20 {
            let mut s =
                extractor.snapshot(&world, Camera::default(), [173, 101], Default::default())?;
            sequence.assign(settings, &technique, &mut s);
            let graph = technique.build(&s)?;
            let mut raw = technique.build(&s)?;
            let resolve = raw
                .passes
                .iter()
                .position(|pass| pass.name == "Temporal AA resolve")
                .unwrap();
            let before = raw.passes[resolve].op.program().unwrap().bindings[3].0;
            raw.passes.truncate(resolve);
            raw.set_output(before);
            let filtered = gpu.execute(graph.compile()?, &pool)?.pixels;
            let baseline = raw_gpu.execute(raw.compile()?, &pool)?.pixels;
            if i >= 8 {
                samples[0].push(baseline);
                samples[1].push(filtered);
            }
        }
        let change = |frames: &[Vec<u8>]| -> u64 {
            frames
                .windows(2)
                .map(|pair| {
                    pair[0]
                        .iter()
                        .zip(&pair[1])
                        .map(|(&a, &b)| u64::from(a.abs_diff(b)))
                        .sum::<u64>()
                })
                .sum()
        };
        let raw = change(&samples[0]);
        let filtered = change(&samples[1]);
        println!("Accumulated RGB change: {raw} without history, {filtered} with TAA");
        assert!(filtered < raw * 4 / 5, "filtered={filtered}, raw={raw}");
        Ok(())
    }

    #[test]
    #[ignore = "requires Vulkan 1.4 and VK_EXT_descriptor_heap"]
    fn visibility_motion_tracks_rigid_and_skinned_surfaces_and_rejects_disocclusion() -> Result<()>
    {
        use crate::{
            render::SceneExtractor,
            rhi::Backend,
            scene::{Entity, Instance, Mesh, Skin},
        };
        use glam::{Mat4, Vec3};

        let extent = [160, 100];
        let camera = Camera {
            yaw: 0.,
            pitch: 0.,
            distance: 5.,
            target: Vec3::ZERO,
        };
        let pixels_per_unit = extent[1] as f32 * 0.5 / (25f32.to_radians().tan() * camera.distance);
        let offset = -0.5 * pixels_per_unit;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
        let technique: Arc<dyn Technique> = Arc::new(Forward::default());
        let mut gpu = Backend::open()?;
        let at = |bytes: &[u8], x: usize, y: usize| -> [u8; 4] {
            let start = 4 * (y * extent[0] as usize + x);
            bytes[start..start + 4].try_into().unwrap()
        };
        for skinned in [false, true] {
            let mut world = World::default();
            let mesh = world.spawn(Entity {
                mesh: Some(Arc::new(Mesh {
                    positions: vec![
                        [-0.65, -0.8, 0.],
                        [0.65, -0.8, 0.],
                        [0.65, 0.8, 0.],
                        [-0.65, 0.8, 0.],
                    ],
                    indices: vec![0, 1, 2, 0, 2, 3],
                    normals: vec![[0., 0., 1.]; 4],
                    uv: vec![],
                    joints: if skinned {
                        vec![[0; 4], [1, 0, 0, 0], [1, 0, 0, 0], [0; 4]]
                    } else {
                        vec![]
                    },
                    weights: if skinned {
                        vec![[1., 0., 0., 0.]; 4]
                    } else {
                        vec![]
                    },
                })),
                ..Default::default()
            });
            let joint = world.spawn(Entity::default());
            let right_joint = world.spawn(Entity {
                parent: Some(joint),
                ..Default::default()
            });
            let skin = world.spawn(Entity {
                skin: Some(Skin {
                    joints: vec![joint, right_joint],
                    inverse_bind: vec![Mat4::IDENTITY; 2],
                }),
                ..Default::default()
            });
            let object = world.spawn(Entity {
                instance: Some(Instance::new(mesh, None)),
                skin_binding: skinned.then_some(skin),
                ..Default::default()
            });
            let node = if skinned { joint } else { object };
            let mut extractor = SceneExtractor::default();
            let mut sequence = Sequence::default();
            let mut settings = Settings::default();
            let mut build = |world: &World, camera, settings| -> Result<FrameGraph> {
                let mut s = extractor.snapshot(world, camera, extent, Default::default())?;
                sequence.assign(settings, &technique, &mut s);
                technique.build(&s)
            };
            settings.select_view(DebugView::Motion);
            gpu.execute(build(&world, camera, settings)?.compile()?, &pool)?;
            world.entities.get_mut(&node).unwrap().local = Mat4::from_translation(Vec3::X * 0.5);
            let result = gpu.execute(build(&world, camera, settings)?.compile()?, &pool)?;
            let [_, green, red, _] = at(&result.pixels, 85, 50);
            let measured = (f32::from(red) / 255. - 0.5) * 32.;
            assert!(
                (measured - offset).abs() < 0.14 && green.abs_diff(128) <= 1,
                "wrong vector with skinned={skinned}: {measured}, {green}"
            );
            if skinned {
                assert!(
                    result
                        .passes
                        .iter()
                        .any(|p| p.name == "GPU skin previous positions")
                );
            }

            settings.select_view(DebugView::Reprojection);
            world.entities.get_mut(&node).unwrap().local = Mat4::IDENTITY;
            gpu.execute(build(&world, camera, settings)?.compile()?, &pool)?;
            world.entities.get_mut(&node).unwrap().local = Mat4::from_translation(Vec3::X * 0.5);
            let result = gpu.execute(build(&world, camera, settings)?.compile()?, &pool)?;
            let magenta = [166, 13, 255, 255];
            assert_eq!(at(&result.pixels, 68, 50), magenta);
            assert_ne!(at(&result.pixels, 85, 50), magenta);

            // Execute through the NGX input adapter without invoking the proprietary runtime.
            settings.select_view(DebugView::Off);
            settings.select(Mode::Dlaa);
            let pack = |mut graph: FrameGraph| -> Result<_> {
                let (index, motion, output) = graph
                    .passes
                    .iter()
                    .enumerate()
                    .find_map(|(i, p)| {
                        if let Op::Dlss(eval) = &p.op {
                            Some((i, eval.motion, eval.output))
                        } else {
                            None
                        }
                    })
                    .unwrap();
                graph.passes.truncate(index);
                graph.resources.truncate(output.0);
                graph.local_buffers.retain(|r| r.0 < output.0);
                let readback = graph.resource(
                    "NGX motion image readback",
                    crate::rdg::Storage::Buffer {
                        bytes: (extent[0] * extent[1] * 4) as usize,
                        initial: None,
                    },
                );
                graph.pass(
                    "Copy NGX motion image for inspection",
                    Op::ReadPixels {
                        image: motion,
                        buffer: readback,
                    },
                );
                graph.set_output(readback);
                graph.compile()
            };
            world.entities.get_mut(&node).unwrap().local = Mat4::IDENTITY;
            gpu.execute(pack(build(&world, camera, settings)?)?, &pool)?;
            world.entities.get_mut(&node).unwrap().local = Mat4::from_translation(Vec3::X * 0.5);
            let packed_vector = |pixels: &[u8], x: usize, y: usize| {
                let [xl, xh, yl, yh] = at(pixels, x, y);
                [u16::from_le_bytes([xl, xh]), u16::from_le_bytes([yl, yh])]
                    .map(|v| half::f16::from_bits(v).to_f32())
            };
            let result = gpu.execute(pack(build(&world, camera, settings)?)?, &pool)?;
            let [x, y] = packed_vector(&result.pixels, 85, 50);
            assert!(
                (x - offset).abs() < 0.02 && y.abs() < 0.02,
                "NGX input {x}, {y}"
            );
            if !skinned {
                let still = gpu.execute(pack(build(&world, camera, settings)?)?, &pool)?;
                assert_eq!(
                    packed_vector(&still.pixels, 85, 50),
                    [0.; 2],
                    "jitter leaked into motion"
                );
                let translated = Camera {
                    target: camera.target + Vec3::X * 0.5,
                    ..camera
                };
                let result = gpu.execute(pack(build(&world, translated, settings)?)?, &pool)?;
                let [x, y] = packed_vector(&result.pixels, 80, 50);
                assert!(
                    (x + offset).abs() < 0.03 && y.abs() < 0.02,
                    "camera motion {x}, {y}"
                );
                let sky = packed_vector(&result.pixels, 20, 20);
                assert!(sky.iter().all(|v| v.abs() < 0.02), "sky parallax {sky:?}");
            } else {
                // Move only the right edge. Interior pixels must interpolate the
                // deformation across the quad, even though the instance is static.
                settings.select_view(DebugView::Motion);
                world.entities.get_mut(&joint).unwrap().local = Mat4::IDENTITY;
                gpu.execute(build(&world, camera, settings)?.compile()?, &pool)?;
                world.entities.get_mut(&right_joint).unwrap().local =
                    Mat4::from_translation(Vec3::X * 0.5);
                let result = gpu.execute(build(&world, camera, settings)?.compile()?, &pool)?;
                for x in [74, 86, 95] {
                    let screen_x = x as f32 + 0.5 - extent[0] as f32 * 0.5;
                    let original_x = (screen_x / pixels_per_unit - 0.25) * (1.3 / 1.8);
                    let expected = original_x * pixels_per_unit - screen_x;
                    let red = at(&result.pixels, x, 50)[2];
                    let measured = (f32::from(red) / 255. - 0.5) * 32.;
                    assert!(
                        (measured - expected).abs() < 0.14,
                        "two-bone deformation at {x}: {measured} vs {expected}"
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn dlss_graph_separates_render_and_output_extents() -> Result<()> {
        let technique: Arc<dyn Technique> = Arc::new(Forward::default());
        let mut view = Snapshot::new(
            &World::demo(),
            Camera::default(),
            [800, 500],
            Default::default(),
        )?;
        let mut settings = Settings::default();
        settings.select(Mode::Dlss);
        Sequence::default().assign(settings, &technique, &mut view);
        let mut graph = technique.build(&view)?;
        assert_eq!(graph.extent, [534, 334]);
        assert_eq!(graph.output_extent(), [800, 500]);
        let output = graph.present()?;
        let plan = graph.compile()?;
        let steps = plan.steps();
        let pass = |name| {
            steps
                .iter()
                .find(|step| plan.graph().passes[step.index].name == name)
                .unwrap()
        };
        let eval = pass("NVIDIA DLSS Quality");
        assert!(
            pass("Prepare DLSS color and motion")
                .parents
                .contains(&pass("Resolve visibility motion").index)
        );
        assert!(eval.parents.contains(&pass("Copy DLSS color image").index));
        assert!(eval.parents.contains(&pass("Copy DLSS motion image").index));
        assert!(
            pass("Copy DLSS output to frame color")
                .parents
                .contains(&eval.index)
        );
        assert_eq!(plan.graph().resources[output.0].0, "DLSS resolved color");
        Ok(())
    }

    #[cfg(render_lab_ngx)]
    #[test]
    #[ignore = "requires the NVIDIA NGX runtime and an RTX Vulkan adapter"]
    fn ngx_resolves_nonempty_color_at_native_and_scaled_extents() -> Result<()> {
        use crate::rhi::Backend;
        let mut device = Backend::open()?;
        if let Some(reason) = device.dlss_unavailable() {
            anyhow::bail!("DLSS unavailable: {reason}");
        }
        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
        let technique: Arc<dyn Technique> = Arc::new(Forward::default());
        let world = World::demo();
        let mut context = Sequence::default();
        let mut settings = Settings::default();
        for (mode, extent) in [
            (Mode::Dlss, [800, 500]),
            (Mode::Dlaa, [640, 400]),
            (Mode::Dlss, [641, 401]),
        ] {
            settings.select(mode);
            let reference = Snapshot::new(&world, Camera::default(), extent, Default::default())?;
            let reference = device.execute(technique.build(&reference)?.compile()?, &pool)?;
            let mut frame = None;
            for _ in 0..4 {
                let mut view =
                    Snapshot::new(&world, Camera::default(), extent, Default::default())?;
                context.assign(settings, &technique, &mut view);
                let graph = technique.build(&view)?;
                frame = Some(device.execute(graph.compile()?, &pool)?);
            }
            let frame = frame.unwrap();
            let error: u64 = frame
                .pixels
                .chunks_exact(4)
                .zip(reference.pixels.chunks_exact(4))
                .map(|(a, b)| {
                    (0..3)
                        .map(|channel| u64::from(a[channel].abs_diff(b[channel])))
                        .sum::<u64>()
                })
                .sum();
            let mae = error as f64 / (frame.pixels.len() / 4 * 3) as f64;
            assert!(
                mae < 12.,
                "{mode:?} at {extent:?} disagrees with native scene, RGB MAE {mae:.2}"
            );
            let colors = frame
                .pixels
                .chunks_exact(4)
                .filter(|pixel| pixel[0..3] != frame.pixels[..3])
                .count();
            assert_eq!(frame.extent, extent);
            assert!(
                colors > 1000,
                "{mode:?} at {extent:?}: {colors} visible pixels"
            );
        }
        Ok(())
    }
}
