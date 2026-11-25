use super::*;
use crate::engine::SimulationData::{Component, LocalTransformsById};
use glam::Vec3Swizzles;

pub(super) struct Prepare(pub Arc<Stage>);
pub(super) struct Feedback(pub Arc<Stage>);

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
            key: Some(PREPARE),
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
    fn transform_targets(&self, _: &TransformBindings<'_>) -> Result<TransformTargets> {
        let s = &self.0;
        let writes = s.corrected();
        Ok(TransformTargets {
            reads: vec![s.actor, s.target],
            writes,
        })
    }
    fn run(&self, cx: &mut SystemContext<'_>, dt: f32) -> Result<()> {
        let s = &self.0;
        let mut c = s.state(cx)?;
        // Restore the unmodified source for clip-transition inertialization.
        cx.set_pose(s.corrected().into_iter().zip(c.source.iter().copied()))?;
        if c.auto_walk || c.play.is_some() {
            let mut p = cx
                .component::<State>(s.actor, PLAYER)?
                .context("missing motion controller")?
                .into_owned();
            let root = local(cx, s.actor)?;
            traversal::steer(
                &mut c,
                &mut p,
                root.w_axis.truncate(),
                target_point(local(cx, s.target)?),
                dt,
            );
            cx.set_typed_component(s.actor, PLAYER, p)?;
        }
        cx.set_typed_component(s.actor, SETTING, c)
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
        let s = &self.0;
        let mut reads = s.rig.ids.clone();
        reads.extend_from_slice(&s.supports);
        reads.extend(s.rail);
        reads.push(s.target);
        Ok(TransformTargets {
            reads,
            writes: s.corrected(),
        })
    }
    fn run(&self, cx: &mut SystemContext<'_>, dt: f32) -> Result<()> {
        let s = &self.0;
        let r = &s.rig;
        let mut c = s.state(cx)?;
        let mut pose = r.pose(cx)?;
        c.source = r.corrected.iter().map(|&i| pose.local[i]).collect();
        if let (Some(binding), Some(play)) = (&s.action_rig, c.play) {
            binding.apply(&mut pose, c.section, play);
        }
        let body = cx
            .component::<dynamics::Body>(s.actor, dynamics::BODY)?
            .context("missing physics feedback")?;
        let airborne = body.support.is_none();
        let boxes = dynamics::surfaces(s, cx)?;
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
            let Some((hit, point)) = dynamics::highest(ball, &boxes, pose.position(0).y + 0.18)
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
        let tip = pose.world[r.finger].transform_point3(FINGERTIP);
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
            .transform_point3(FINGERTIP)
            .distance(target);
        cx.set_pose(r.corrected.iter().map(|&i| (r.ids[i], pose.local[i])))?;
        cx.set_typed_component(s.actor, SETTING, c)
    }
}
