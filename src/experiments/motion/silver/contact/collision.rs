//! Character-clock collision snapshot. The immutable BVH and sampled world transforms
//! are shared by all character solves on this substep.
mod history;
use super::*;
use crate::{
    engine::{
        BoundSystems,
        SimulationData::{Component, LocalTransformsById, Resource as SimResource},
    },
    physics::{
        BoxCollider, BoxSurface, COLLIDER, Capsule,
        collision::{Bounds, BoxIndex},
    },
};
use std::collections::BTreeSet;

pub(super) const QUERY: &str = "engine.physics.character_query";
#[derive(Clone, Copy)]
struct Shape {
    id: Id,
    node: usize,
    collider: BoxCollider,
}
struct Binding {
    nodes: Vec<(Id, Option<usize>)>,
    boxes: Vec<Shape>,
    incarnations: BTreeMap<Id, u64>,
    parents: BTreeMap<Id, Option<usize>>,
}
impl Binding {
    fn new(w: &World) -> Result<Self> {
        fn collect(
            id: Id,
            w: &World,
            items: &mut Vec<(Id, Option<usize>)>,
            lookup: &mut BTreeMap<Id, usize>,
            mut path: BTreeSet<Id>,
        ) -> Result<usize> {
            if let Some(&i) = lookup.get(&id) {
                return Ok(i);
            }
            ensure!(path.insert(id), "collider hierarchy cycle");
            let e = w.entities.get(&id).context("missing collider ancestor")?;
            let parent = e
                .parent
                .map(|p| collect(p, w, items, lookup, path))
                .transpose()?;
            let i = items.len();
            items.push((id, parent));
            lookup.insert(id, i);
            Ok(i)
        }
        let mut nodes = Vec::new();
        let mut lookup = BTreeMap::new();
        let mut boxes = Vec::new();
        let mut parents = BTreeMap::new();
        for &id in w.entities.component_owners(COLLIDER) {
            let shape = *w
                .typed_component::<BoxCollider>(id, COLLIDER)?
                .context("missing box collider")?;
            shape.validate()?;
            boxes.push(Shape {
                id,
                node: collect(id, w, &mut nodes, &mut lookup, BTreeSet::new())?,
                collider: shape,
            });
        }
        for &id in w.entities.component_owners(SETTING) {
            let index = w.entities[&id]
                .parent
                .map(|p| collect(p, w, &mut nodes, &mut lookup, BTreeSet::new()))
                .transpose()?;
            parents.insert(id, index);
        }
        let incarnations = nodes
            .iter()
            .map(|n| n.0)
            .chain(parents.keys().copied())
            .map(|id| (id, w.entities.incarnation(id)))
            .filter(|(_, v)| *v != 0)
            .collect();
        Ok(Self {
            nodes,
            boxes,
            incarnations,
            parents,
        })
    }
}
#[derive(Clone, Copy)]
struct Placement {
    world: Mat4,
    local: Mat4,
    active: bool,
    angular_drive: Option<(f32, Vec3)>,
    local_face: Option<crate::physics::SupportFace>,
}
pub(super) struct Query {
    binding: Arc<Binding>,
    boxes: Arc<Vec<Placement>>,
    previous: history::History,
    transforms: Arc<Vec<Mat4>>,
    locals: Vec<Mat4>,
    settings_version: [u64; 2],
    swept: bool,
    pub(super) restore_history: bool,
    hierarchy: Arc<BoxIndex>,
}
pub(super) struct Candidate {
    pub id: Id,
    pub surface: BoxSurface,
    pub transform: Mat4,
    pub previous: Option<Mat4>,
    pub drive: Option<(f32, Vec3, Option<Vec3>)>,
    pub face: crate::physics::SupportFace,
    pub prior_face: Option<(Vec3, Quat)>,
}
impl Query {
    pub fn walkable_shape(&self, id: Id) -> Option<BoxCollider> {
        let i = self
            .binding
            .boxes
            .binary_search_by_key(&id, |row| row.id)
            .ok()?;
        let shape = self.binding.boxes[i].collider;
        (self.boxes[i].active && shape.surface != BoxSurface::Anchor).then_some(shape)
    }
    pub fn permits_grip(&self, id: Id, mask: u32) -> bool {
        self.binding
            .boxes
            .binary_search_by_key(&id, |row| row.id)
            .is_ok_and(|i| {
                self.boxes[i].active && self.binding.boxes[i].collider.layers & mask != 0
            })
    }
    pub fn near(&self, actor: Id, shape: Capsule, area: Bounds) -> Vec<Candidate> {
        let parent = self.binding.parents[&actor];
        let frame = parent.map_or(Mat4::IDENTITY, |i| self.transforms[i]);
        let inverse = frame.inverse();
        let previous_frame = parent
            .and_then(|i| self.previous.parent(actor, i))
            .unwrap_or(frame);
        let old = previous_frame.inverse();
        let swept_area = area
            .transformed(frame)
            .union(area.transformed(previous_frame));
        let mut found = Vec::new();
        self.hierarchy.query(swept_area, |i| {
            let Shape { id, node, collider } = self.binding.boxes[i];
            let p = self.boxes[i];
            if !p.active
                || collider.layers & shape.mask == 0
                || collider.surface == BoxSurface::Anchor
                || id == actor
            {
                return;
            }
            let sibling = self.binding.nodes[node].1 == parent;
            let transform = if sibling { p.local } else { inverse * p.world };
            let Some(face) = (if sibling {
                p.local_face
            } else {
                crate::physics::SupportFace::from_box(id, transform, 0.65)
            }) else {
                return;
            };
            let previous = self.previous.placement(i).filter(|p| p.active);
            let old_transform = previous.map(|p| if sibling { p.local } else { old * p.world });
            let prior_face = previous.and_then(|p| {
                if sibling {
                    p.local_face.map(|f| (f.center, f.rotation))
                } else {
                    old_transform
                        .and_then(|m| crate::physics::SupportFace::from_box(id, m, 0.65))
                        .map(|f| (f.center, f.rotation))
                }
            });
            found.push(Candidate {
                id,
                surface: collider.surface,
                transform,
                previous: old_transform,
                face,
                prior_face,
                drive: p.angular_drive.filter(|_| sibling).map(|(rate, pivot)| {
                    (
                        rate,
                        pivot,
                        previous.and_then(|p| p.angular_drive.map(|drive| drive.1)),
                    )
                }),
            });
        });
        found.sort_unstable_by_key(|c| c.id);
        found
    }
}
struct Publish(Arc<Binding>);
impl SimulationSystem for Publish {
    fn order(&self) -> SystemOrder {
        SystemOrder {
            key: Some(QUERY),
            after: &[dynamics::TURNTABLE],
        }
    }
    fn access(&self) -> SystemAccess {
        SystemAccess {
            name: "Shared character collision BVH",
            reads: &[
                LocalTransformsById,
                Component(MOTION_POLICY),
                Component(dynamics::TURNTABLE),
                SimResource(QUERY),
            ],
            writes: &[SimResource(QUERY)],
        }
    }
    fn bound_transforms(&self) -> Option<TransformTargets> {
        Some(TransformTargets {
            reads: self.0.nodes.iter().map(|n| n.0).collect(),
            writes: Vec::new(),
        })
    }
    fn run(&self, cx: &mut SystemContext<'_>, _: f32) -> Result<()> {
        let snapshot = cx.resource::<Query>(QUERY)?;
        if self.0.parents.is_empty() {
            if snapshot.is_some_and(|q| !q.binding.parents.is_empty()) {
                cx.set_resource(
                    QUERY,
                    Query {
                        binding: self.0.clone(),
                        boxes: Arc::default(),
                        previous: history::History::new(None, &self.0),
                        transforms: Arc::default(),
                        locals: Vec::new(),
                        settings_version: [0; 2],
                        swept: false,
                        restore_history: false,
                        hierarchy: Arc::default(),
                    },
                )?;
            }
            return Ok(());
        }
        let restore_history = snapshot.is_none();
        let snapshot = snapshot.filter(|q| !q.binding.parents.is_empty());
        let history = history::History::new(snapshot, &self.0);
        let last = snapshot.filter(|q| Arc::ptr_eq(&self.0, &q.binding));
        let settings_version = [
            cx.component_binding_version(MOTION_POLICY)?,
            cx.component_binding_version(dynamics::TURNTABLE)?,
        ];
        let cached = last.filter(|q| q.settings_version == settings_version);
        let locals = self
            .0
            .nodes
            .iter()
            .map(|(id, _)| cx.local(*id)?.context("missing collider node"))
            .collect::<Result<Vec<_>>>()?;
        if let Some(q) = cached
            && q.locals == locals
            && !q.swept
        {
            return Ok(());
        }
        let mut transforms: Vec<Mat4> = Vec::with_capacity(locals.len());
        for (&local, &(_, parent)) in locals.iter().zip(&self.0.nodes) {
            transforms.push(parent.map_or(local, |p| transforms[p] * local));
        }
        let mut boxes = Vec::with_capacity(self.0.boxes.len());
        let mut bounds = Vec::with_capacity(self.0.boxes.len());
        let moved = cached.is_none_or(|q| q.locals != locals);
        for (i, definition) in self.0.boxes.iter().enumerate() {
            let Shape {
                id,
                node,
                collider: shape,
            } = *definition;
            let prior = history.placement(i);
            // Read entity transforms even for fixed geometry, so editor moves and moved ancestors take effect.
            // Reuse the complete validated local geometry when the entity and its parent stayed still.
            if let Some(q) = cached
                && transforms[node] == q.transforms[node]
                && locals[node] == q.locals[node]
            {
                let p = q.boxes[i];
                bounds.push(Bounds::for_box(p.world));
                boxes.push(p);
                continue;
            }
            let (active, rate) = if let Some(q) = cached {
                let p = q.boxes[i];
                (p.active, p.angular_drive.map(|d| d.0))
            } else {
                let policy = cx.motion_policy(id, MotionPolicy::FIXED_COLLIDER)?;
                let rate = cx.component::<f32>(id, dynamics::TURNTABLE)?.map(|v| {
                    if policy.mobility == Mobility::Static {
                        0.
                    } else {
                        *v
                    }
                });
                (policy.physics != PhysicsParticipation::Off, rate)
            };
            let angular_drive = rate.map(|r| (r, locals[node].w_axis.truncate()));
            let world = shape.box_transform(transforms[node]);
            ensure!(
                world.is_finite() && (!active || world.determinant().abs() > 1e-12),
                "degenerate collider {id}"
            );
            let bound = Bounds::for_box(world);
            bounds.push(prior.map_or(bound, |p| bound.union(Bounds::for_box(p.world))));
            let local = shape.box_transform(locals[node]);
            let local_face = if active && shape.surface != BoxSurface::Anchor {
                if let Some(p) = prior.filter(|p| p.active && p.local == local) {
                    p.local_face
                } else {
                    crate::physics::SupportFace::from_box(id, local, 0.65)
                }
            } else {
                None
            };
            boxes.push(Placement {
                world,
                local,
                local_face,
                active,
                angular_drive,
            });
        }
        for i in self.0.parents.values().flatten() {
            ensure!(
                transforms[*i].determinant().abs() > 1e-12,
                "singular character parent"
            );
        }
        let hierarchy = match last {
            None => Arc::new(BoxIndex::build(&bounds)),
            Some(q) if !moved && !q.swept => q.hierarchy.clone(),
            Some(q) => {
                let mut tree = (*q.hierarchy).clone();
                tree.refit(&bounds);
                Arc::new(tree)
            }
        };
        cx.set_resource(
            QUERY,
            Query {
                binding: self.0.clone(),
                boxes: Arc::new(boxes),
                previous: history,
                transforms: Arc::new(transforms),
                locals,
                settings_version,
                swept: moved,
                restore_history,
                hierarchy,
            },
        )
    }
}
pub(super) fn register(w: &World, schedule: &mut Schedule) -> Result<()> {
    let mut systems = BoundSystems::<Arc<Binding>>::default();
    systems.register(|b| Publish(b.clone()));
    systems.install(schedule, character_clock(), w, COLLIDER, |w| {
        let binding = Arc::new(Binding::new(w)?);
        let owners: Vec<_> = binding.parents.keys().copied().collect();
        let shape_version = w.binding_version(COLLIDER);
        Ok((
            binding,
            Box::new(move |world| {
                Ok(world.entities.component_owners(SETTING) == owners
                    && world.binding_version(COLLIDER) == shape_version)
            }),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::TickRate,
        movement::character::{RESOLVED, ResolvedMotion},
    };
    #[test]
    #[ignore = "dynamic 3,000-collider scene, 500 capsule casts per snapshot"]
    fn profile_dynamic_collision_snapshot() -> Result<()> {
        let mut w = World::default();
        let mut movers = Vec::new();
        for i in 0..3000 {
            let center = Vec3::new((i % 60) as f32 * 2.5, 1., (i / 60) as f32 * 4.);
            let id = w.spawn(Entity {
                local: Mat4::from_translation(center),
                ..Default::default()
            });
            w.set_box_collider(
                id,
                BoxCollider {
                    half_extents: Vec3::new(0.25, 1., 0.5),
                    ..Default::default()
                },
            )?;
            if i % 10 == 0 {
                movers.push((id, center));
            }
        }
        let actors: Vec<_> = (0..500)
            .map(|i| {
                let at = Vec3::new((i % 50) as f32 * 2.5 + 0.6, 0., (i / 50) as f32 * 4.);
                let id = w.spawn(Entity {
                    local: Mat4::from_translation(at),
                    ..Default::default()
                });
                w.set_setting(id, SETTING, serde_json::json!({}));
                (id, at)
            })
            .collect();
        let mut schedule = Schedule::default();
        schedule.add_fixed_system(character_clock(), Arc::new(dynamics::Turntables))?;
        register(&w, &mut schedule)?;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        let mut w = Arc::new(w);
        let mut stats: [Vec<f64>; 3] = Default::default();
        let mut total = 0;
        for frame in 0..100 {
            for &(id, center) in &movers {
                Arc::make_mut(&mut w).set_local(
                    id,
                    Mat4::from_translation(center + Vec3::X * ((frame % 8) as f32 * 0.01)),
                )?;
            }
            let now = std::time::Instant::now();
            schedule.tick(&mut w, 1. / 60., &pool)?;
            let prepare = now.elapsed();
            let query = w.simulation_resources[QUERY]
                .downcast_ref::<Query>()
                .unwrap();
            let c = Capsule::default();
            let d = Vec3::X * 2.;
            let now = std::time::Instant::now();
            let narrow: Vec<_> = actors
                .iter()
                .map(|&(actor, from)| {
                    let r = Vec3::splat(c.radius);
                    query.near(
                        actor,
                        c,
                        Bounds {
                            lo: from - r,
                            hi: from + d + Vec3::Y * c.height + r,
                        },
                    )
                })
                .collect();
            let lookup = now.elapsed();
            let now = std::time::Instant::now();
            let mut hits = 0;
            for (&(_, from), set) in actors.iter().zip(&narrow) {
                total += set.len();
                for hit in set {
                    let displacement = hit
                        .previous
                        .map_or(Vec3::ZERO, |p| (hit.transform.w_axis - p.w_axis).truncate());
                    hits += usize::from(
                        crate::physics::sweep_capsule(
                            c,
                            from,
                            from + d,
                            hit.transform,
                            displacement,
                        )
                        .is_some(),
                    );
                }
            }
            assert!(hits >= actors.len());
            if frame >= 4 {
                for (t, time) in stats.iter_mut().zip([prepare, lookup, now.elapsed()]) {
                    t.push(time.as_secs_f64() * 1000.);
                }
            }
        }
        for (t, name) in stats.iter_mut().zip([
            "snapshot fixed substep",
            "500 BVH queries and support assembly",
            "capsule narrow phase",
        ]) {
            t.sort_by(f64::total_cmp);
            eprintln!("{name}: P50/P95 {:.4}/{:.4} ms", t[47], t[91]);
        }
        eprintln!(
            "dynamic scene mean candidates per capsule {:.2}",
            total as f64 / 50000.
        );
        Ok(())
    }

    #[test]
    fn spring_stands_on_authored_box_after_legacy_migration_and_reload() -> Result<()> {
        let mut world = scene()?;
        let actor = super::super::actor(&world).unwrap();
        let mut c = world
            .typed_component::<Contact>(actor, SETTING)?
            .unwrap()
            .into_owned();
        let (plate, floor) = (c.plate, c.floor);
        let shape = BoxCollider {
            surface: BoxSurface::Platform,
            center: Vec3::new(3., 0.2, 0.),
            half_extents: Vec3::new(0.17, 0.24, 0.21),
            layers: 3,
        };
        world.set_box_collider(plate, shape)?;
        // A legacy save can already contain an individually authored collider.
        world
            .entities
            .get_mut(&floor)
            .unwrap()
            .components
            .remove(COLLIDER);
        c.collider_schema = 0;
        c.auto_walk = false;
        c.play = None;
        world.set_typed_component(actor, SETTING, c);
        hydrate_contact(&mut world)?;
        assert!(
            world
                .typed_component::<BoxCollider>(floor, COLLIDER)?
                .is_some()
        );
        let birth = shape
            .box_transform(world.entities[&plate].local)
            .transform_point3(Vec3::Y * 0.5)
            + Vec3::Y * 0.015;
        world.set_local(actor, Mat4::from_translation(birth))?;
        world.set_typed_component(actor, dynamics::BODY, dynamics::Body::new(birth));
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("spring.json");
        world.save(&file)?;
        let mut world = World::load(&file)?;
        hydrate_contact(&mut world)?;
        assert_eq!(
            world
                .typed_component::<BoxCollider>(plate, COLLIDER)?
                .unwrap()
                .layers,
            3
        );
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        let mut schedule = Schedule::default();
        install(&world, &mut schedule)?;
        let mut world = Arc::new(world);
        for _ in 0..45 {
            schedule.tick(&mut world, 1. / 60., &pool)?;
        }
        let resolved = world
            .typed_component::<ResolvedMotion>(actor, RESOLVED)?
            .unwrap();
        assert_eq!(resolved.support, Some(plate));
        let face = resolved.surfaces.iter().find(|s| s.id == plate).unwrap();
        let expected = shape.box_transform(world.entities[&plate].local);
        assert!(expected.abs_diff_eq(face.world, 1e-5));
        let root = resolved.root_local.w_axis.truncate();
        assert!(face.below(root).unwrap().abs_diff_eq(root, 1e-5));
        assert!(face.below(face.center + Vec3::Z * 0.4).is_none());
        Ok(())
    }

    #[test]
    fn character_discovers_unbound_solids_on_a_different_parent_and_keeps_its_layer_mask()
    -> Result<()> {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        for hz in [30, 60, 120] {
            let mut world = scene()?;
            let actor = super::super::actor(&world).unwrap();
            let at = Vec3::new(8., 0.9, 0.);
            let mut c = world
                .typed_component::<Contact>(actor, SETTING)?
                .unwrap()
                .into_owned();
            let plate = c.plate;
            c.auto_walk = false;
            c.play = None;
            world.set_typed_component(actor, SETTING, c);
            world.set_local(actor, Mat4::from_translation(at))?;
            let mut body = dynamics::Body::new(at);
            body.capsule.mask = 1;
            body.particle.momentum = Vec3::new(4., 6., 0.) * body.particle.mass;
            let cap = body.capsule;
            world.set_typed_component(actor, dynamics::BODY, body);
            let parent = world.spawn(Entity {
                local: Mat4::from_translation(Vec3::X * 8.),
                ..Default::default()
            });
            let shape = |w: &mut World, at, extents, layers| -> Result<Id> {
                let id = w.spawn(Entity {
                    parent: Some(parent),
                    local: Mat4::from_translation(at),
                    ..Default::default()
                });
                w.set_box_collider(
                    id,
                    BoxCollider {
                        half_extents: extents,
                        layers,
                        ..Default::default()
                    },
                )?;
                Ok(id)
            };
            let floor = shape(&mut world, Vec3::new(0., 0., 0.), Vec3::new(2., 0.1, 2.), 1)?;
            shape(
                &mut world,
                Vec3::new(0.8, 1.5, 0.),
                Vec3::new(0.04, 1.5, 2.),
                1,
            )?;
            shape(
                &mut world,
                Vec3::new(0., 2.6, 0.),
                Vec3::new(2., 0.1, 2.),
                1,
            )?;
            shape(
                &mut world,
                Vec3::new(0.25, 1.5, 0.),
                Vec3::new(0.04, 1.5, 2.),
                2,
            )?;
            let barrier = world.spawn(Entity {
                local: Mat4::from_translation(Vec3::new(8., 1.5, 2.)),
                ..Default::default()
            });
            world.set_box_collider(
                barrier,
                BoxCollider {
                    half_extents: Vec3::new(2., 1.5, 0.04),
                    ..Default::default()
                },
            )?;
            let mut schedule = Schedule::default();
            install(&world, &mut schedule)?;
            schedule.set_tick_rate(character_clock().name, TickRate::Hertz(hz))?;
            let mut w = Arc::new(world);
            let initial = w.clone();
            let mut hit = [false; 2];
            let mut farthest = at.x;
            for _ in 0..hz {
                schedule.tick(&mut w, 1. / hz as f32, &pool)?;
                let solved = w
                    .typed_component::<ResolvedMotion>(actor, RESOLVED)?
                    .unwrap();
                let pos = solved.root_local.w_axis.truncate();
                farthest = farthest.max(pos.x);
                assert!(
                    pos.x + cap.radius <= 8.761 && pos.y + cap.height <= 2.501,
                    "tunneled at {hz} Hz: {pos:?}"
                );
                if let Some(n) = solved.blocking_normal {
                    hit[0] |= n.x < -0.9;
                    hit[1] |= n.y < -0.9;
                }
            }
            assert!(
                hit.into_iter().all(|v| v) && farthest > 8.50,
                "missing contact at {hz}Hz {hit:?}, {farthest}"
            );
            assert_eq!(
                w.typed_component::<ResolvedMotion>(actor, RESOLVED)?
                    .unwrap()
                    .support,
                Some(floor)
            );
            let before = initial
                .typed_component::<dynamics::Spring>(plate, dynamics::SPRING)?
                .unwrap();
            let after = w
                .typed_component::<dynamics::Spring>(plate, dynamics::SPRING)?
                .unwrap();
            assert!(
                std::ptr::eq(before.as_ref(), after.as_ref()),
                "an untouched plate was committed again"
            );
            // Moving the collider's parent moves the surface in the character's reference frame.
            let old = w.entities[&actor].local.w_axis.truncate();
            Arc::make_mut(&mut w)
                .set_local(parent, Mat4::from_translation(Vec3::new(8., 0., 0.01)))?;
            schedule.tick(&mut w, 1. / hz as f32, &pool)?;
            let moved = w.entities[&actor].local.w_axis.truncate();
            assert!((moved.z - old.z - 0.01).abs() < 1e-3);
            // A carrier impulse exceeds the original world query. A stationary thin wall still blocks it.
            Arc::make_mut(&mut w)
                .set_local(parent, Mat4::from_translation(Vec3::new(8., 0., 4.)))?;
            schedule.tick(&mut w, 1. / hz as f32, &pool)?;
            let clipped = w.entities[&actor].local.w_axis.z;
            assert!(
                (1.76..1.79).contains(&clipped),
                "missed wall outside initial support query at {hz}Hz: {clipped}"
            );
        }
        Ok(())
    }
}
