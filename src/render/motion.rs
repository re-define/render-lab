use super::*;
use crate::rdg::PassPhase;
use anyhow::Context;

pub(crate) struct SurfaceHistory<'a> {
    pub previous: Option<&'a RenderInstance>,
    // Scene slots in each of the two snapshots, with room for a surface type.
    tokens: [u32; 2],
}

impl Snapshot {
    pub(crate) fn surface_history(
        &self,
        instance: &RenderInstance,
    ) -> Result<Option<SurfaceHistory<'_>>> {
        let Some(frame) = &self.temporal else {
            return Ok(None);
        };
        let token = |slot: usize| -> Result<u32> {
            let id = u32::try_from(slot + 1)?;
            ensure!(id <= u32::MAX >> 2, "too many temporal instances");
            Ok(id << 2)
        };
        let slot = self
            .scene
            .instances
            .binary_search_by_key(&instance.id, |i| i.id)
            .ok()
            .context("surface missing from the render snapshot")?;
        let prior = frame.previous_instance(&self.scene, instance);
        Ok(Some(SurfaceHistory {
            previous: prior.map(|(_, i)| i),
            tokens: [
                token(slot)?,
                prior.map(|(slot, _)| token(slot)).transpose()?.unwrap_or(0),
            ],
        }))
    }
}

pub(crate) enum MotionSource {
    Camera,
    Rigid(Mat4),
    Skinned(Resource),
    Wind {
        previous_model: Mat4,
        palette: Resource,
    },
    Unavailable,
}

impl MotionSource {
    pub(crate) fn rigid(model: Mat4, previous: Mat4) -> Self {
        if model == previous {
            return Self::Camera;
        }
        let old_from_current = previous * model.inverse();
        if old_from_current.is_finite() {
            Self::Rigid(old_from_current)
        } else {
            Self::Unavailable
        }
    }
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct Object {
    transform: [f32; 16],
    info: [u32; 4],
}

#[derive(Default)]
pub(super) struct Table {
    objects: Vec<Object>,
    // Previous geometry is only a dependency of the motion pass.
    addresses: Vec<Resource>,
}

impl Table {
    pub(super) fn address(&mut self, resource: Resource) -> Result<u32> {
        if let Some(i) = self.addresses.iter().position(|&r| r == resource) {
            return Ok(i.try_into()?);
        }
        let i = self.addresses.len().try_into()?;
        self.addresses.push(resource);
        Ok(i)
    }
    pub(super) fn register(
        &mut self,
        history: SurfaceHistory<'_>,
        source: MotionSource,
    ) -> Result<()> {
        let (kind, index, matrix) = match source {
            MotionSource::Camera => (0, 0, Mat4::IDENTITY),
            MotionSource::Rigid(transform) => (1, 0, transform),
            MotionSource::Skinned(vertices) => (2, self.address(vertices)?, Mat4::IDENTITY),
            MotionSource::Wind {
                previous_model,
                palette,
            } => (3, self.address(palette)?, previous_model),
            MotionSource::Unavailable => (4, 0, Mat4::IDENTITY),
        };
        self.objects.push(Object {
            transform: matrix.to_cols_array(),
            info: [history.tokens[0], history.tokens[1], kind, index],
        });
        Ok(())
    }

    pub(super) fn resolve(
        self,
        graph: &mut FrameGraph,
        snapshot: &Snapshot,
        frame: &post::TemporalFrame,
        inputs: ResolveInputs,
        prefix: Option<(Resource, u32)>,
    ) -> Resource {
        #[derive(Clone, Copy, Pod, Zeroable)]
        #[repr(C)]
        struct View {
            inverse_vp: [f32; 16],
            previous_vp: [f32; 16],
            jitter: [f32; 2],
            has_previous_view: u32,
            same_projection: u32,
        }
        let previous_vp = frame
            .previous_camera
            .view_projection(snapshot.output_extent());
        let view = View {
            inverse_vp: snapshot.view_projection().inverse().to_cols_array(),
            previous_vp: previous_vp.to_cols_array(),
            jitter: frame.jitter,
            has_previous_view: u32::from(frame.previous_scene.is_some()),
            same_projection: u32::from(
                previous_vp == snapshot.camera.view_projection(snapshot.output_extent()),
            ),
        };
        let view = graph.input(
            "Motion reprojection cameras",
            bytemuck::bytes_of(&view).to_vec(),
        );
        let objects = super::visibility::surface_table(
            graph,
            "Previous surface transforms and geometry",
            &self.objects,
            prefix,
        );
        let previous_addresses = graph.address_table(self.addresses);
        let [width, height] = snapshot.extent;
        let samples = graph.local_buffer(
            "Visibility motion and history correspondence",
            width as usize * height as usize * 24,
        );
        use Access::{ComputeRead as R, ComputeSample as S, ComputeWrite as W};
        graph.pass_in(
            PassPhase::Motion,
            "Resolve visibility motion",
            Op::Compute {
                program: Dispatch {
                    shader: Shader::Compute(include_bytes!(concat!(
                        env!("OUT_DIR"),
                        "/surface_motion.comp.spv"
                    ))),
                    bindings: vec![
                        (inputs.ids, S),
                        (inputs.objects, R),
                        (inputs.depth, S),
                        (inputs.addresses, R),
                        (inputs.camera, R),
                        (samples, W),
                        (view, R),
                        (objects, R),
                        (previous_addresses, R),
                    ],
                    push: vec![],
                },
                groups: [width.div_ceil(8), height.div_ceil(8), 1],
            },
        );
        samples
    }
}
