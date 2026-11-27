//! Validate lighting dependencies before emitting geometry, allocation or dispatch work.
use super::{Backdrop, FrameGraph, LightingInput, MaterialPrograms, ResolveInputs, Resource};
use crate::{
    rdg::{Access, Dispatch, Op, PassPhase, Shader},
    render::rays::RayScene,
};
use anyhow::{Context, Result, bail, ensure};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::experiments) enum Signal {
    Surfaces,
    RawDirect,
    Direct,
    IndirectDiffuse,
    Environment,
}

impl From<LightingInput> for Signal {
    fn from(input: LightingInput) -> Self {
        match input {
            LightingInput::Environment | LightingInput::EnvironmentControls => Self::Environment,
            LightingInput::Direct | LightingInput::DirectControls => Self::Direct,
            LightingInput::IndirectDiffuse => Self::IndirectDiffuse,
        }
    }
}

/// Shared per-pixel receiver data and geometry for shadow and material-hit queries.
pub(in crate::experiments) struct SurfaceLighting {
    pub current: Resource,
    pub config: Resource,
    pub lights: Resource,
    pub history: Resource,
    pub indices: Resource,
    pub motion: Resource,
    pub depth: Resource,
    pub raster: [Resource; 3],
    pub occluders: RayScene,
    pub material_hits: Option<RayScene>,
    pub history_key: u64,
}
/// Radiance and initial candidate reservoirs produced by the selected direct estimator.
#[derive(Clone, Copy)]
pub(in crate::experiments) struct Samples {
    pub raw: Resource,
    pub initial: Resource,
}

pub(in crate::experiments) enum Contribution {
    Surfaces(Box<SurfaceLighting>),
    RawDirect(Samples),
    Direct(Resource, Resource),
    IndirectDiffuse(Resource),
    Environment((Resource, Resource), Option<Backdrop>),
}

type Build<'a> =
    Box<dyn FnOnce(&mut FrameGraph, ResolveInputs, Reads<'_>) -> Result<Contribution> + 'a>;
pub(in crate::experiments) struct AddLight<'a> {
    name: &'static str,
    inputs: &'static [Signal],
    output: Signal,
    build: Build<'a>,
}
impl<'a> AddLight<'a> {
    pub(in crate::experiments) fn new(
        name: &'static str,
        inputs: &'static [Signal],
        output: Signal,
        build: impl FnOnce(&mut FrameGraph, ResolveInputs, Reads<'_>) -> Result<Contribution> + 'a,
    ) -> Self {
        Self {
            name,
            inputs,
            output,
            build: Box::new(build),
        }
    }
}

pub(super) fn order<'a>(
    mut stages: Vec<AddLight<'a>>,
    materials: &MaterialPrograms,
) -> Result<Vec<AddLight<'a>>> {
    for (i, stage) in stages.iter().enumerate() {
        if let Some(other) = stages[..i]
            .iter()
            .find(|other| other.output == stage.output)
        {
            bail!(
                "lighting stages '{}' and '{}' both produce {:?}",
                other.name,
                stage.name,
                stage.output
            );
        }
        for input in stage.inputs {
            ensure!(
                stages.iter().any(|s| s.output == *input),
                "{} requires a producer for {input:?}",
                stage.name
            );
        }
    }
    for (_, name, inputs, optional) in materials.lighting() {
        if !optional {
            for input in inputs {
                ensure!(
                    stages.iter().any(|s| s.output == (*input).into()),
                    "{name} requires a producer for {input:?}"
                );
            }
        }
    }
    let mut sorted: Vec<AddLight<'a>> = Vec::with_capacity(stages.len());
    while !stages.is_empty() {
        let ready = stages.iter().position(|s| {
            s.inputs
                .iter()
                .all(|input| sorted.iter().any(|done| done.output == *input))
        });
        let Some(ready) = ready else {
            bail!(
                "cyclic lighting stages: {}",
                stages.iter().map(|s| s.name).collect::<Vec<_>>().join(", ")
            );
        };
        sorted.push(stages.remove(ready));
    }
    Ok(sorted)
}

#[derive(Default)]
pub(in crate::experiments) struct Products {
    surfaces: Option<Box<SurfaceLighting>>,
    raw: Option<Samples>,
    direct: Option<(Resource, Resource)>,
    indirect: Option<Resource>,
    pub(super) environment: Option<((Resource, Resource), Option<Backdrop>)>,
}
pub(in crate::experiments) struct Reads<'a> {
    products: &'a Products,
    declared: &'static [Signal],
}
impl Reads<'_> {
    pub(in crate::experiments) fn samples(&self) -> Result<&Samples> {
        ensure!(
            self.declared.contains(&Signal::RawDirect),
            "undeclared lighting input RawDirect"
        );
        self.products
            .raw
            .as_ref()
            .context("missing raw direct-light samples")
    }
    pub(in crate::experiments) fn surfaces(&self) -> Result<&SurfaceLighting> {
        ensure!(
            self.declared.contains(&Signal::Surfaces),
            "undeclared lighting input Surfaces"
        );
        self.products
            .surfaces
            .as_deref()
            .context("missing reconstructed lighting surfaces")
    }
}
impl Products {
    pub(super) fn material_input(&self, input: LightingInput) -> Option<Resource> {
        match input {
            LightingInput::Environment => self.environment.map(|((data, _), _)| data),
            LightingInput::EnvironmentControls => {
                self.environment.map(|((_, controls), _)| controls)
            }
            LightingInput::Direct => self.direct.map(|(data, _)| data),
            LightingInput::DirectControls => self.direct.map(|(_, controls)| controls),
            LightingInput::IndirectDiffuse => self.indirect,
        }
    }
    pub(super) fn run(
        &mut self,
        stage: AddLight<'_>,
        graph: &mut FrameGraph,
        inputs: ResolveInputs,
    ) -> Result<()> {
        let output = (stage.build)(
            graph,
            inputs,
            Reads {
                products: self,
                declared: stage.inputs,
            },
        )?;
        match (stage.output, output) {
            (Signal::Surfaces, Contribution::Surfaces(scene)) => self.surfaces = Some(scene),
            (Signal::RawDirect, Contribution::RawDirect(samples)) => self.raw = Some(samples),
            (Signal::Direct, Contribution::Direct(r, c)) => self.direct = Some((r, c)),
            (Signal::IndirectDiffuse, Contribution::IndirectDiffuse(r)) => self.indirect = Some(r),
            (Signal::Environment, Contribution::Environment(r, b)) => {
                self.environment = Some((r, b))
            }
            _ => bail!(
                "{} did not publish its declared {:?} output",
                stage.name,
                stage.output
            ),
        }
        Ok(())
    }
}

pub(in crate::experiments) fn pixel_buffer(
    g: &mut FrameGraph,
    name: &str,
    stride: usize,
) -> Resource {
    let [w, h] = g.extent;
    g.local_buffer(name, w as usize * h as usize * stride)
}

pub(in crate::experiments) fn pass(
    g: &mut FrameGraph,
    phase: PassPhase,
    name: &str,
    shader: &'static [u8],
    bindings: Vec<(Resource, Access)>,
    push: Vec<u8>,
) {
    let [w, h] = g.extent;
    g.pass_in(
        phase,
        name,
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(shader),
                bindings,
                push,
            },
            groups: [w.div_ceil(8), h.div_ceil(8), 1],
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stage(name: &'static str, inputs: &'static [Signal], out: Signal) -> AddLight<'static> {
        AddLight::new(name, inputs, out, |_, _, _| {
            unreachable!("validation executes no passes")
        })
    }
    #[test]
    fn resolves_registered_stages_and_rejects_invalid_graphs() {
        use Signal::*;
        let order = |s| super::order(s, &MaterialPrograms::default());
        let error = |s| order(s).err().expect("must fail").to_string();
        assert!(
            error(vec![stage("a", &[], Direct), stage("b", &[], Direct)])
                .contains("both produce Direct")
        );
        assert!(
            error(vec![stage("a", &[RawDirect], Direct)])
                .contains("requires a producer for RawDirect")
        );
        assert!(
            error(vec![
                stage("a", &[RawDirect], Direct),
                stage("b", &[Direct], RawDirect)
            ])
            .contains("cyclic")
        );
        let sorted = order(vec![
            stage("filter", &[RawDirect], Direct),
            stage("probes", &[], IndirectDiffuse),
            stage("sample", &[], RawDirect),
        ])
        .unwrap();
        assert_eq!(
            sorted.iter().map(|s| s.name).collect::<Vec<_>>(),
            ["probes", "sample", "filter"]
        );
    }
}
