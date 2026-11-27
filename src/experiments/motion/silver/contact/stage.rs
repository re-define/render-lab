use super::*;
use crate::engine::SimulationData::{Component, LocalTransformsById};
use glam::Vec3Swizzles;

pub(super) fn register_motion(systems: &mut CharacterSystems, bank: Arc<SilverMatcher>) {
    systems.register(move |actors| Prepare {
        actors: actors.values().cloned().collect(),
        bank: bank.clone(),
    });
}

pub(super) fn register_ik(systems: &mut CharacterSystems) {
    systems.register(|actors| Feedback(actors.values().cloned().collect()));
}

struct Prepare {
    actors: Vec<Arc<Stage>>,
    bank: Arc<SilverMatcher>,
}
struct Feedback(Vec<Arc<Stage>>);

impl Stage {
    pub(super) fn state(&self, cx: &SystemContext<'_>) -> Result<Contact> {
        Ok(cx
            .component::<Contact>(self.actor, SETTING)?
            .context("missing contact state")?
            .into_owned())
    }
}
fn local(cx: &SystemContext<'_>, id: Id) -> Result<Mat4> {
    cx.local(id)?.context("missing contact transform")
}

impl SimulationSystem for Prepare {
    fn order(&self) -> SystemOrder {
        SystemOrder {
            key: Some(PLAYER),
            after: &[],
        }
    }
    fn access(&self) -> SystemAccess {
        SystemAccess {
            name: "Locomotion and traversal intent",
            reads: &[
                Component(SETTING),
                Component(PLAYER),
                Component(MOTION_POLICY),
                LocalTransformsById,
            ],
            writes: &[Component(SETTING), Component(PLAYER)],
        }
    }
    fn bound_transforms(&self) -> Option<TransformTargets> {
        let mut targets = TransformTargets::default();
        for stage in &self.actors {
            targets.reads.extend([stage.actor, stage.target]);
            targets.reads.extend_from_slice(&stage.binding.bones);
        }
        Some(targets)
    }
    fn run(&self, cx: &mut SystemContext<'_>, dt: f32) -> Result<()> {
        cx.for_each_parallel(&self.actors, |s, cx| {
            let id = s.actor;
            let mut p = cx
                .component::<State>(id, PLAYER)?
                .context("missing motion controller")?
                .into_owned();
            let mut c = s.state(cx)?;
            let inherited_velocity = p.standalone_velocity.take();
            if inherited_velocity.is_some() {
                c.play = None;
            }
            if inherited_velocity.is_some()
                || c.source_binding
                    .as_ref()
                    .is_some_and(|old| !Arc::ptr_eq(old, &s.binding) && **old != *s.binding)
            {
                c.source.clear();
                c.feet = Default::default();
                (c.pelvis, c.reach, c.palm_load) = (0., 0., 0.);
                p.transition = None;
                p.search_in = 0;
            }
            c.source_binding = Some(s.binding.clone());
            let root = local(cx, id)?;
            let policy = cx.motion_policy(id, MotionPolicy::DYNAMIC)?;
            if policy.physics != PhysicsParticipation::Simulated {
                // Traversal waits for physical take-off and landing; free locomotion still samples the pose.
                c.play = None;
            } else if c.auto_walk || c.play.is_some() {
                traversal::steer(
                    &mut c,
                    &mut p,
                    root.w_axis.truncate(),
                    target_point(local(cx, s.target)?),
                    dt,
                );
            }
            let (mut root, base) = self.bank.sample_pose(&mut p, root, dt, |i, bone| {
                match c.source.get(s.rig.source_slots[i]) {
                    Some(previous) => Ok(*previous),
                    None => local(cx, bone),
                }
            })?;
            c.source.resize(base.len(), Mat4::IDENTITY);
            for (&slot, joint) in s.rig.source_slots.iter().zip(base) {
                c.source[slot] = joint;
            }
            if let Some(play) = c.play {
                root.w_axis = c.section.ground_goal(play).extend(1.);
            }
            c.request = Some(ContactMotion {
                root: RootMotionRequest::new(root)?,
                inherited_velocity,
            });
            cx.set_typed_component(id, SETTING, c)?;
            cx.set_typed_component(id, PLAYER, p)
        })
    }
}

impl SimulationSystem for Feedback {
    fn order(&self) -> SystemOrder {
        SystemOrder {
            key: Some(SETTING),
            after: &[dynamics::BODY],
        }
    }
    fn access(&self) -> SystemAccess {
        SystemAccess {
            name: "Foot placement and fingertip IK",
            reads: &[
                Component(SETTING),
                Component(dynamics::BODY),
                LocalTransformsById,
            ],
            writes: &[Component(SETTING), LocalTransformsById],
        }
    }
    fn bound_transforms(&self) -> Option<TransformTargets> {
        let mut targets = TransformTargets::default();
        for s in &self.0 {
            targets.reads.extend_from_slice(&s.rig.ids);
            targets.reads.extend(s.rail);
            targets.reads.push(s.target);
            targets
                .writes
                .extend_from_slice(s.rig.published_joints.joints());
        }
        Some(targets)
    }
    fn run(&self, cx: &mut SystemContext<'_>, dt: f32) -> Result<()> {
        cx.for_each_parallel(&self.0, |s, cx| feedback(s, cx, dt))
    }
}

fn feedback(s: &Stage, cx: &mut SystemContext<'_>, dt: f32) -> Result<()> {
    let r = &s.rig;
    let mut c = s.state(cx)?;
    let mut pose = r.pose(cx, &c.source)?;
    if let (Some(binding), Some(play)) = (&s.action_rig, c.play) {
        binding.apply(&mut pose, c.section, play);
    }
    let body = cx
        .component::<dynamics::Body>(s.actor, dynamics::BODY)?
        .context("missing physics feedback")?;
    let airborne = body.support.is_none();
    let boxes = body.faces.as_ref();
    let facing = pose.rotation(0);
    let mut goals = [None; 2];
    let mut height = 0.;
    let mut ceiling = f32::INFINITY;
    for (i, [a, b, ankle, toe]) in r.legs.into_iter().enumerate() {
        if airborne {
            c.feet[i] = Foot::default();
            continue;
        }
        let ball = pose.position(toe);
        let Some((hit, point)) =
            crate::physics::SupportFace::highest_below(ball, boxes, pose.position(0).y + 0.18)
        else {
            c.feet[i] = Foot::default();
            continue;
        };
        let clearance = (ball.y - pose.position(0).y - 0.024).max(0.);
        // A foot reaching across the trench can see its service floor far below.
        // Only a walkable step within leg reach may alter a grounded pose.
        if point.y < pose.position(0).y - 0.16 {
            c.feet[i] = Foot::default();
            continue;
        }
        let foot = &mut c.feet[i];
        let animated = point + Vec3::Y * clearance;
        let mut sole = animated;
        if foot.anchor.is_none() && clearance < 0.034 {
            foot.anchor = Some((hit.id, hit.world.inverse().transform_point3(point)));
            foot.releasing = false;
            foot.weight = 0.;
        }
        if let Some((owner, p)) = foot.anchor {
            let Some(support) = boxes.iter().find(|s| s.id == owner) else {
                *foot = Foot::default();
                continue;
            };
            let lock = support.world.transform_point3(p);
            foot.releasing |=
                owner != hit.id || clearance > 0.052 || lock.xz().distance(ball.xz()) > 0.085;
            let rate = if foot.releasing { -10. } else { 12. };
            foot.weight = (foot.weight + dt * rate).clamp(0., 1.);
            let t = foot.weight;
            sole = animated.lerp(lock, t * t * (3. - 2. * t));
            if foot.weight == 0. {
                foot.anchor = None;
            }
        }
        let tilt = Quat::from_rotation_arc(Vec3::Y, hit.normal);
        let goal = sole + Vec3::Y * 0.024 + tilt * (pose.position(ankle) - ball);
        let thigh = pose.position(b) - pose.position(a);
        let axis = (pose.position(ankle) - pose.position(a)).normalize_or_zero();
        let authored_bend = thigh - axis * thigh.dot(axis);
        // Retain the clip's knee plane. A nearly straight leg has an ill-defined
        // plane, so use facing as a continuous bias through extension.
        let pole = authored_bend + 0.025 * (facing * Vec3::Z);
        goals[i] = Some((goal, tilt * pose.rotation(ankle), pole));
        height += 0.5 * (point.y - pose.position(0).y);
        // Keep the trailing foot reachable when only the leading foot is on the step.
        let length = pose.position(a).distance(pose.position(b))
            + pose.position(b).distance(pose.position(ankle));
        let horiz = goal.xz().distance_squared(pose.position(a).xz());
        ceiling =
            ceiling.min(goal.y + (length * length - horiz).max(0.).sqrt() - pose.position(a).y);
    }
    if c.foot_ik {
        c.pelvis += (height - c.pelvis) * (1. - (-18. * dt).exp());
        c.pelvis = c.pelvis.min(ceiling).clamp(-0.15, 0.24);
        pose.translate_world(r.hip, Vec3::Y * c.pelvis);
        for (i, [a, b, ankle, _]) in r.legs.into_iter().enumerate() {
            if let Some((goal, rotation, pole)) = goals[i] {
                pose.solve([a, b, ankle], goal, pole, rotation);
            }
        }
    } else {
        c.pelvis = 0.;
    }
    let target = target_point(local(cx, s.target)?);
    let [shoulder, _, wrist] = r.arm;
    let from = pose.position(shoulder);
    let tip = pose.world[r.finger].transform_point3(r.fingertip);
    let offset = tip - pose.position(wrist);
    c.palm_load = 0.;
    if c.hand_ik
        && c.section == traversal::Section::Rail
        && let (Some(play), Some(rail)) = (c.play, s.rail)
    {
        c.palm_load = traversal::brace(r, &mut pose, play.frame, local(cx, rail)?);
    }
    let desire = if c.hand_ik
        && c.play.is_none()
        && (c.course.is_none() || !c.auto_walk || c.section == traversal::Section::Console)
    {
        ((0.77 - target.distance(from)) / 0.19).clamp(0., 1.)
    } else {
        0.
    };
    c.reach += (desire - c.reach).clamp(-3.2 * dt, 3.2 * dt);
    if c.reach > 0. {
        let direction = (target - from).try_normalize().unwrap_or(facing * Vec3::Z);
        let aim = Quat::from_rotation_arc(offset.normalize(), direction);
        let swing = Quat::IDENTITY.slerp(aim, c.reach);
        let wrist_goal = tip.lerp(target, c.reach) - swing * offset;
        let animated_pole = (pose.position(r.arm[1]) - from).normalize_or_zero();
        let elbow_pole = animated_pole.lerp(facing * Vec3::new(-0.55, -1., -0.3), c.reach);
        pose.solve(r.arm, wrist_goal, elbow_pole, swing * pose.rotation(wrist));
    }
    c.error_m = pose.world[r.finger]
        .transform_point3(r.fingertip)
        .distance(target);
    cx.publish_pose(
        s.actor,
        r.published_joints.clone(),
        r.corrected.iter().map(|&i| pose.local[i]).collect(),
    )?;
    cx.set_typed_component(s.actor, SETTING, c)
}

#[cfg(test)]
mod request_tests {
    use super::*;

    #[test]
    fn sampling_only_publishes_a_request_until_the_solver_commits() -> Result<()> {
        let mut world = scene()?;
        let id = actor(&world).unwrap();
        let mut c = world
            .typed_component::<Contact>(id, SETTING)?
            .unwrap()
            .into_owned();
        c.auto_walk = false;
        world.set_typed_component(id, SETTING, c);
        set_actor_goal(&mut world, id, 0, 1)?;
        let before = world.entities[&id].local;
        let stage = Arc::new(bind_actor(&world, id, &rig::children(&world))?);
        let mut schedule = Schedule::default();
        schedule.add_fixed_system(
            character_clock(),
            Arc::new(Prepare {
                actors: vec![stage],
                bank: Arc::new(SilverMatcher::load()?),
            }),
        )?;
        let mut world = Arc::new(world);
        let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
        schedule.tick(&mut world, 1. / 60., &pool)?;
        let c = world.typed_component::<Contact>(id, SETTING)?.unwrap();
        assert_eq!(before, world.entities[&id].local);
        assert!(c.request.unwrap().root.target_local().w_axis.z > before.w_axis.z);
        Ok(())
    }
}
