mod assembly;
pub mod cartoon;
pub mod clusters;
pub mod composition;
pub mod effects;
pub mod gaussian;
pub mod hair;
pub mod ibl;
mod light_stack;
use assembly::SceneConfiguration;
pub mod lighting;
pub mod mixed;
pub mod motion;
pub mod movement_lab;
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

/// Render-time deformations are extracted with the scene, then registered before surface consumers.
struct Deformer {
    component: SnapshotComponent,
    prepare: fn(&mut FrameGraph, &Snapshot) -> Result<()>,
}
const DEFORMERS: &[Deformer] = &[Deformer {
    component: hair::SNAPSHOT,
    prepare: hair::prepare,
}];

pub(crate) fn has_deformation(scene: &crate::render::RenderScene) -> bool {
    DEFORMERS.iter().any(|d| scene.has_derived(d.component.key))
}
pub(crate) fn prepare_deformations(g: &mut FrameGraph, s: &Snapshot) -> Result<()> {
    for d in DEFORMERS
        .iter()
        .filter(|d| s.scene.has_derived(d.component.key))
    {
        (d.prepare)(g, s)?;
    }
    Ok(())
}

/// Overlays for the packed color buffer at display resolution after temporal resolve and tone mapping.
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
        SnapshotComponent::setting(composition::SETTING),
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
    .chain(DEFORMERS.iter().map(|d| d.component))
    .chain(VISIBILITY_OVERLAYS.iter().map(|overlay| overlay.component))
    .chain(
        light_stack::REGISTRY
            .iter()
            .map(|r| SnapshotComponent::setting(r.key)),
    )
}

/// Saved experiment settings restore all scene-bound systems and render programs.
pub fn configure(engine: &mut Engine) -> Result<()> {
    let mut candidate = (*engine.world).clone();
    candidate.clocks = engine.schedule.checkpoints(&candidate);
    candidate.install_motion_providers(&assembly::ROOT_PROVIDERS)?;
    let prepared = SceneConfiguration::prepare(&candidate, &engine.tasks, engine.ray_caps)?;
    engine.world = Arc::new(candidate);
    prepared.apply(engine);
    Ok(())
}

/// Validate and configure the candidate before replacing any live scene state.
pub fn replace(engine: &mut Engine, mut world: World) -> Result<()> {
    world.validate()?;
    SceneConfiguration::restore(&mut world)?;
    let prepared = SceneConfiguration::prepare(&world, &engine.tasks, engine.ray_caps)?;
    engine.audio.stop_all();
    engine.world = Arc::new(world);
    prepared.apply(engine);
    Ok(())
}

pub fn open(mut world: World) -> Result<Engine> {
    SceneConfiguration::restore(&mut world)?;
    let mut engine = Engine::start_workers(world, Arc::new(Forward::default()))?;
    SceneConfiguration::prepare(&engine.world, &engine.tasks, engine.ray_caps)?.apply(&mut engine);
    Ok(engine)
}
