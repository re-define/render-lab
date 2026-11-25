use super::*;
use crate::rdg::PassPhase;
use crate::render::FrameGraph;
use crate::{
    rdg::*,
    render::{Snapshot, SnapshotComponent, SnapshotContext, SnapshotValue},
};

struct Trajectory {
    model: Mat4,
    points: [Vec2; 10],
}

pub(in crate::experiments) const OVERLAY: crate::experiments::VisibilityOverlay =
    crate::experiments::VisibilityOverlay {
        component: SnapshotComponent {
            key: PLAYER,
            copy_setting: false,
            related_settings: &[],
            extract: Some(extract_trajectory),
        },
        draw: draw_trajectory,
    };

fn extract_trajectory(context: &mut SnapshotContext<'_>) -> Result<Option<SnapshotValue>> {
    if !crate::experiments::composition::snapshot_layer_enabled(context, "actor") {
        return Ok(None);
    }
    let model = context
        .transform(context.owner)
        .context("missing player transform")?;
    let state = context.typed::<State>()?;
    let mut points = [Vec2::ZERO; 10];
    let horizons = [0., HORIZONS[0], HORIZONS[1], HORIZONS[2]];
    for (index, point) in points.iter_mut().enumerate().skip(1) {
        let interval = (index - 1) / 3;
        let t = ((index - 1) % 3 + 1) as f32 / 3.;
        let time = horizons[interval] + (horizons[interval + 1] - horizons[interval]) * t;
        *point = state.predict(time).0;
    }
    Ok(Some(Arc::new(Trajectory { model, points })))
}

fn draw_trajectory(
    graph: &mut FrameGraph,
    snapshot: &Snapshot,
    visibility: Resource,
    color: Resource,
) -> Result<()> {
    let trajectory = snapshot
        .scene
        .derived::<Trajectory>(PLAYER)
        .context("invalid trajectory output")?;
    let display_extent = graph.output_extent();
    let extent = Vec2::from_array(display_extent.map(|v| v as f32));
    let view = snapshot.view_projection() * trajectory.model;
    let project = |p: Vec2| {
        let clip = view * Vec3::new(p.x, 0.02, p.y).extend(1.);
        (clip.is_finite() && clip.w > 0. && clip.z >= 0.)
            .then(|| (clip.truncate().truncate() / clip.w * 0.5 + 0.5) * extent)
    };
    let Some(origin) = project(Vec2::ZERO) else {
        return Ok(());
    };
    let mut points = [origin; 10];
    for i in 1..points.len() {
        points[i] = project(trajectory.points[i]).unwrap_or(points[i - 1]);
    }

    // Limit the dispatch to the trajectory's screen rectangle, including the markers.
    let padding = Vec2::splat(9.);
    let min = (points.iter().copied().fold(origin, Vec2::min) - padding)
        .floor()
        .clamp(Vec2::ZERO, extent)
        .as_uvec2();
    let max = (points.iter().copied().fold(origin, Vec2::max) + padding)
        .ceil()
        .clamp(Vec2::ZERO, extent)
        .as_uvec2();
    let area = max - min;
    if area.min_element() == 0 {
        return Ok(());
    }
    let mut push = Vec::with_capacity(26);
    push.extend(display_extent);
    push.extend(min.to_array());
    push.extend(area.to_array());
    push.extend(
        points
            .into_iter()
            .flat_map(|p| [p.x.to_bits(), p.y.to_bits()]),
    );
    graph.pass_in(
        PassPhase::Overlay,
        "Motion trajectory",
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/motion_trajectory.comp.spv"
                ))),
                bindings: vec![
                    (visibility, Access::ComputeSample),
                    (color, Access::ComputeReadWrite),
                ],
                push: bytemuck::cast_slice(&push).to_vec(),
            },
            groups: [area.x.div_ceil(8), area.y.div_ceil(8), 1],
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{Camera, SceneExtractor};

    #[test]
    fn typed_player_edits_reach_the_registered_overlay() -> Result<()> {
        let mut world = scene()?;
        let id = actor(&world).unwrap();
        let mut extractor = SceneExtractor::default();
        let mut snapshot = |world: &World| {
            extractor.snapshot(world, Camera::default(), [128, 80], Default::default())
        };
        let still = snapshot(&world)?;
        assert!(
            still
                .scene
                .derived::<Trajectory>(PLAYER)
                .unwrap()
                .points
                .iter()
                .all(|p| *p == Vec2::ZERO)
        );
        set_keyboard_goal(&mut world, 1, 1)?;
        let moving = snapshot(&world)?;
        assert_eq!(moving.scene.stats.entities_visited, 1);
        assert!(moving.scene.setting(PLAYER).is_none());
        let trajectory = moving.scene.derived::<Trajectory>(PLAYER).unwrap();
        assert_eq!(
            trajectory.points[9],
            player(&world, id)?.predict(HORIZONS[2]).0
        );
        assert_ne!(trajectory.points[9], Vec2::ZERO);
        let mut graph = FrameGraph::new(moving.extent);
        let ids = graph.resource("Test visibility", Storage::Color);
        let color = graph.input("Test color", vec![0; 128 * 80 * 4]);
        crate::experiments::draw_visibility_overlays(&mut graph, &moving, ids, color)?;
        assert!(
            graph
                .passes
                .iter()
                .any(|p| p.name == "Motion trajectory" && p.phase == PassPhase::Overlay)
        );
        Ok(())
    }
}
