pub mod cartoon;
pub mod clusters;
pub mod composition;
pub mod effects;
pub mod gaussian;
pub mod ibl;
mod light_stack;
pub mod lighting;
pub mod mixed;
pub mod motion;
pub mod restir;
pub mod texture;
pub mod vegetation;
use crate::{
    engine::{Engine, Schedule},
    rdg::Resource,
    render::{
        Camera, Forward, FrameGraph, MaterialPrograms, Snapshot, SnapshotComponent, Technique,
    },
    scene::World,
};
use anyhow::Result;
use std::sync::Arc;

/// Overlays for the packed color buffer after visibility shading and before image outlines.
pub(super) struct VisibilityOverlay {
    pub component: SnapshotComponent,
    pub draw: fn(&mut FrameGraph, &Snapshot, Resource, Resource) -> Result<()>,
}
const VISIBILITY_OVERLAYS: &[VisibilityOverlay] = &[motion::silver::OVERLAY];

pub(super) fn draw_visibility_overlays(
    graph: &mut FrameGraph,
    snapshot: &Snapshot,
    visibility: Resource,
    color: Resource,
) -> Result<()> {
    for overlay in VISIBILITY_OVERLAYS {
        if snapshot.scene.has_derived(overlay.component.key) {
            (overlay.draw)(graph, snapshot, visibility, color)?;
        }
    }
    Ok(())
}

/// Register scene-wide render settings and experiment-owned derived data.
pub(crate) fn snapshot_components() -> impl Iterator<Item = SnapshotComponent> {
    [
        SnapshotComponent::setting(crate::render::rays::VIEW),
        cartoon::SNAPSHOT,
        SnapshotComponent::setting(effects::SETTING),
        // The reference matcher's render input is the joint pose; its controller state stays on the CPU.
        SnapshotComponent {
            copy_setting: false,
            ..SnapshotComponent::setting(motion::PLAYER)
        },
        clusters::SNAPSHOT,
        SnapshotComponent::setting(lighting::SETTING),
        SnapshotComponent::setting(vegetation::SETTING),
    ]
    .into_iter()
    .chain(VISIBILITY_OVERLAYS.iter().map(|overlay| overlay.component))
    .chain(
        light_stack::REGISTRY
            .iter()
            .map(|r| SnapshotComponent::setting(r.key)),
    )
}

struct SceneConfiguration {
    camera: Camera,
    materials: Arc<MaterialPrograms>,
    schedule: Schedule,
    technique: Arc<dyn Technique>,
}

impl SceneConfiguration {
    fn prepare(world: &World, tasks: &rayon::ThreadPool) -> Result<Self> {
        let has = |key| world.first_raw_component(key).is_some();
        let mut materials = MaterialPrograms::default();
        let mut schedule = Schedule::default();
        let (technique, camera): (Arc<dyn Technique>, Camera) = if let Some(modules) =
            composition::open(world, tasks, &mut materials, &mut schedule)?
        {
            modules
        } else if has(lighting::SETTING) {
            let camera = lighting::configure(world, &mut materials)?;
            (Arc::new(lighting::Lighting::default()), camera)
        } else if has(gaussian::SETTING) {
            gaussian::register(&mut materials)?;
            (
                Arc::new(gaussian::Gaussian::new(world)?),
                gaussian::camera(world)?,
            )
        } else if has(texture::SETTING) {
            texture::register(&mut materials)?;
            (
                Arc::new(texture::TextureCompression::new(world)?),
                texture::camera(world)?,
            )
        } else {
            (Arc::new(Forward::default()), Camera::fit(world)?)
        };
        if has(vegetation::SETTING) && composition::enabled(world, "forest") {
            schedule.add_system(Arc::new(vegetation::Wind));
        }
        if has(motion::PLAYER) {
            schedule.add_system(Arc::new(motion::Matcher::default()));
        }
        if has(motion::silver::PLAYER) && composition::enabled(world, "actor") {
            schedule.add_system(Arc::new(motion::silver::SilverMatcher::load()?));
        }
        Ok(Self {
            camera,
            materials: Arc::new(materials),
            schedule,
            technique,
        })
    }

    fn apply(self, engine: &mut Engine) {
        engine.camera = self.camera;
        engine.materials = self.materials;
        engine.technique = self.technique;
        engine.schedule = self.schedule;
    }
}

/// Saved experiment settings restore all scene-bound systems and render programs.
pub fn configure(engine: &mut Engine) -> Result<()> {
    SceneConfiguration::prepare(&engine.world, &engine.tasks)?.apply(engine);
    Ok(())
}

/// Validate and configure the candidate before replacing any live scene state.
pub fn replace(engine: &mut Engine, mut world: World) -> Result<()> {
    world.validate()?;
    motion::silver::hydrate(&mut world)?;
    let prepared = SceneConfiguration::prepare(&world, &engine.tasks)?;
    engine.world = Arc::new(world);
    prepared.apply(engine);
    Ok(())
}

pub fn open(mut world: World) -> Result<Engine> {
    motion::silver::hydrate(&mut world)?;
    let mut engine = Engine::start_workers(world, Arc::new(Forward::default()))?;
    configure(&mut engine)?;
    Ok(engine)
}
