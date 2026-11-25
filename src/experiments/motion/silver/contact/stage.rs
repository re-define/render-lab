use super::*;
use crate::engine::SimulationData::{Component, LocalTransformsById};
use glam::Vec3Swizzles;

pub(super) struct Prepare {
    pub actors: BTreeMap<Id, Arc<Stage>>,
    pub bank: Arc<SilverMatcher>,
}
pub(super) struct Feedback(pub Vec<Arc<Stage>>);

impl Stage {
    pub(super) fn state(&self, cx: &SystemContext<'_>) -> Result<Contact> {
        Ok(cx
            .component::<Contact>(self.actor, SETTING)?
            .context("missing contact state")?
            .into_owned())
    }
    fn corrected(&self) -> Vec<Id> {
        self.rig
            .corrected
            .iter()
            .map(|&i| self.rig.ids[i])
            .collect()
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
            reads: &[Component(SETTING), Component(PLAYER), LocalTransformsById],
            writes: &[Component(SETTING), Component(PLAYER), LocalTransformsById],
        }
    }
    fn transform_targets(&self, bindings: &TransformBindings<'_>) -> Result<TransformTargets> {
        let mut targets = TransformTargets::default();
        for &id in bindings.component_owners(PLAYER)? {
            let p = bindings
                .component::<State>(id, PLAYER)?
                .context("missing player")?;
            targets.reads.push(id);
            targets.reads.extend_from_slice(&p.bones);
            targets.writes.push(id);
            if let Some(s) = self.actors.get(&id) {
                targets.reads.push(s.target);
            } else {
                targets.writes.extend_from_slice(&p.bones);
            }
        }
        Ok(targets)
    }
    fn run(&self, cx: &mut SystemContext<'_>, dt: f32) -> Result<()> {
        cx.for_each_parallel(cx.component_owners(PLAYER)?, |&id, cx| {
            let mut p = cx
                .component::<State>(id, PLAYER)?
                .context("missing motion controller")?
                .into_owned();
            if let Some(s) = self.actors.get(&id) {
                let mut c = s.state(cx)?;
                if let Some(old) = &c.source_binding
                    && !Arc::ptr_eq(old, &s.binding)
                    && **old != *s.binding
                {
                    c.source.clear();
                    c.feet = Default::default();
                    (c.pelvis, c.reach, c.palm_load) = (0., 0., 0.);
                    p.transition = None;
                    p.search_in = 0;
                }
                c.source_binding = Some(s.binding.clone());
                let root = local(cx, id)?;
                if c.auto_walk || c.play.is_some() {
                    traversal::steer(
                        &mut c,
                        &mut p,
                        root.w_axis.truncate(),
                        target_point(local(cx, s.target)?),
                        dt,
                    );
                }
                let (root, base) = self.bank.sample_pose(&mut p, root, dt, |i, bone| {
                    match c.source.get(s.rig.source_slots[i]) {
                        Some(previous) => Ok(*previous),
                        None => local(cx, bone),
                    }
                })?;
                c.source.resize(base.len(), Mat4::IDENTITY);
                for (&slot, joint) in s.rig.source_slots.iter().zip(base) {
                    c.source[slot] = joint;
                }
                cx.set_pose([(id, root)])?;
                cx.set_typed_component(id, SETTING, c)?;
            } else {
                self.bank.publish_locomotion(id, &mut p, cx, dt)?;
            }
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
    fn transform_targets(&self, _: &TransformBindings<'_>) -> Result<TransformTargets> {
        let mut targets = TransformTargets::default();
        for s in &self.0 {
            targets.reads.extend_from_slice(&s.rig.ids);
            targets.reads.extend(s.rail);
            targets.reads.push(s.target);
            targets.writes.extend(s.corrected());
        }
        Ok(targets)
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
            let support = boxes
                .iter()
                .find(|s| s.id == owner)
                .context("missing planted support")?;
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
    cx.set_pose(r.corrected.iter().map(|&i| (r.ids[i], pose.local[i])))?;
    cx.set_typed_component(s.actor, SETTING, c)
}
