//! Recover prior transforms for unchanged shapes and reference frames after a binding edit.
use super::{Binding, Id, Mat4, Placement, Query};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub(super) struct History(Option<Samples>);

struct Samples {
    boxes: Arc<Vec<Placement>>,
    nodes: Arc<Vec<Mat4>>,
    remap: Option<Remap>,
}

struct Remap {
    boxes: Vec<Option<usize>>,
    nodes: Vec<Option<usize>>,
    continuous_parents: BTreeSet<Id>,
}

impl Remap {
    fn between(old: &Binding, current: &Binding) -> Self {
        let old_indices: BTreeMap<_, _> = old
            .nodes
            .iter()
            .enumerate()
            .map(|(i, &(id, _))| (id, i))
            .collect();
        let same_entity = |id| old.incarnations.get(&id) == current.incarnations.get(&id);
        // A local sample only has meaning if it is still expressed in the same hierarchy.
        // Parent nodes always precede children in a binding.
        let mut nodes = Vec::with_capacity(current.nodes.len());
        for &(id, parent) in &current.nodes {
            let previous = old_indices.get(&id).copied().filter(|&i| {
                same_entity(id)
                    && match (parent, old.nodes[i].1) {
                        (None, None) => true,
                        (Some(p), Some(old_parent)) => nodes[p] == Some(old_parent),
                        _ => false,
                    }
            });
            nodes.push(previous);
        }
        let boxes = current
            .boxes
            .iter()
            .map(|shape| {
                let node = nodes[shape.node]?;
                let i = old.boxes.binary_search_by_key(&shape.id, |s| s.id).ok()?;
                let previous = &old.boxes[i];
                (previous.node == node && previous.collider == shape.collider).then_some(i)
            })
            .collect();
        let continuous_parents = current
            .parents
            .iter()
            .filter_map(|(&id, parent)| {
                let old_parent = old.parents.get(&id)?;
                let matched = match (parent, old_parent) {
                    (None, None) => true,
                    (Some(i), Some(old)) => nodes[*i] == Some(*old),
                    _ => false,
                };
                (same_entity(id) && matched).then_some(id)
            })
            .collect();
        Self {
            nodes,
            boxes,
            continuous_parents,
        }
    }
}

impl History {
    pub(super) fn new(last: Option<&Query>, binding: &Arc<Binding>) -> Self {
        Self(last.map(|q| Samples {
            boxes: q.boxes.clone(),
            nodes: q.transforms.clone(),
            remap: (!Arc::ptr_eq(&q.binding, binding)).then(|| Remap::between(&q.binding, binding)),
        }))
    }
    pub(super) fn placement(&self, i: usize) -> Option<&Placement> {
        let samples = self.0.as_ref()?;
        let old = match &samples.remap {
            Some(map) => map.boxes[i]?,
            None => i,
        };
        samples.boxes.get(old)
    }
    pub(super) fn parent(&self, actor: Id, i: usize) -> Option<Mat4> {
        let samples = self.0.as_ref()?;
        let old = match &samples.remap {
            Some(map) if map.continuous_parents.contains(&actor) => map.nodes[i]?,
            Some(_) => return None,
            None => i,
        };
        samples.nodes.get(old).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::scene::Entity;
    #[test]
    fn rebinding_keeps_swept_colliders_and_resets_reused_or_reparented_samples() -> Result<()> {
        let mut w = World::default();
        let frame = w.spawn(Entity::default());
        let other_frame = w.spawn(Entity::default());
        let actor = w.spawn(Entity {
            parent: Some(frame),
            ..Default::default()
        });
        w.set_setting(actor, SETTING, serde_json::json!({}));
        let late_shape = w.spawn(Entity {
            local: Mat4::from_translation(Vec3::X * 100.),
            ..Default::default()
        });
        let wall = w.spawn(Entity {
            local: Mat4::from_translation(Vec3::X * (-2.)),
            ..Default::default()
        });
        w.set_box_collider(wall, BoxCollider::default())?;
        let mut schedule = Schedule::default();
        schedule.add_fixed_system(character_clock(), Arc::new(dynamics::Turntables))?;
        register(&w, &mut schedule)?;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
        let mut w = Arc::new(w);
        let tick = |s: &mut Schedule, w: &mut Arc<World>| s.tick(w, 1. / 60., &pool);
        tick(&mut schedule, &mut w)?;
        // A new earlier-ID shape shifts the old wall's index. The wall crosses the stationary actor.
        let edit = Arc::make_mut(&mut w);
        edit.set_box_collider(late_shape, BoxCollider::default())?;
        edit.set_local(wall, Mat4::from_translation(Vec3::X * 2.))?;
        edit.set_local(frame, Mat4::from_translation(Vec3::Z * 0.05))?;
        tick(&mut schedule, &mut w)?;
        let query = w.simulation_resources[QUERY]
            .downcast_ref::<Query>()
            .unwrap();
        let area = Bounds {
            lo: Vec3::splat(-0.2),
            hi: Vec3::splat(0.2),
        };
        let set = query.near(actor, Capsule::default(), area);
        let crossing = set
            .iter()
            .find(|c| c.id == wall)
            .context("wall lost from swept BVH")?;
        let previous = crossing
            .previous
            .context("wall starts cold after unrelated binding edit")?;
        assert!(
            previous
                .w_axis
                .truncate()
                .abs_diff_eq(Vec3::X * (-2.), 1e-6)
        );
        assert!((crossing.transform.w_axis.x - 2.).abs() < 1e-6);
        let collider_index =
            |q: &Query, id| q.binding.boxes.binary_search_by_key(&id, |b| b.id).unwrap();
        assert!(
            query
                .previous
                .placement(collider_index(query, late_shape))
                .is_none()
        );
        // The swept region expires once the wall has spent a full step at rest.
        tick(&mut schedule, &mut w)?;
        let query = w.simulation_resources[QUERY]
            .downcast_ref::<Query>()
            .unwrap();
        assert!(
            query
                .near(actor, Capsule::default(), area)
                .iter()
                .all(|c| c.id != wall)
        );
        let still = w.simulation_resources[QUERY].clone();
        tick(&mut schedule, &mut w)?;
        assert!(Arc::ptr_eq(&still, &w.simulation_resources[QUERY]));
        let edit = Arc::make_mut(&mut w);
        edit.entities.get_mut(&actor).unwrap().parent = Some(other_frame);
        edit.entities.remove(&wall);
        let new_wall = edit.spawn(Entity::default());
        assert_eq!(new_wall, wall);
        edit.set_box_collider(new_wall, BoxCollider::default())?;
        tick(&mut schedule, &mut w)?;
        let query = w.simulation_resources[QUERY]
            .downcast_ref::<Query>()
            .unwrap();
        assert!(
            query
                .previous
                .placement(collider_index(query, new_wall))
                .is_none()
        );
        assert!(
            query
                .previous
                .parent(actor, query.binding.parents[&actor].unwrap())
                .is_none()
        );
        assert!(
            query
                .previous
                .placement(collider_index(query, late_shape))
                .is_some()
        );
        // Nothing samples geometry while every character is detached.
        Arc::make_mut(&mut w).remove_typed_component(actor, SETTING);
        for x in [1., 2., 3.] {
            Arc::make_mut(&mut w).set_local(new_wall, Mat4::from_translation(Vec3::X * x))?;
            tick(&mut schedule, &mut w)?;
        }
        Arc::make_mut(&mut w).set_setting(actor, SETTING, serde_json::json!({}));
        tick(&mut schedule, &mut w)?;
        let query = w.simulation_resources[QUERY]
            .downcast_ref::<Query>()
            .unwrap();
        assert!(
            query
                .previous
                .placement(collider_index(query, new_wall))
                .is_none()
        );
        Ok(())
    }
}
