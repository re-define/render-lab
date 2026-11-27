//! Sample selection, root extraction, retargeting and inertial pose transitions.
use super::*;

impl SilverMatcher {
    pub(super) fn step(&self, p: &mut State) -> Result<Option<(usize, usize)>> {
        let outgoing = p.sample;
        let (span, _) = self
            .clips
            .iter()
            .find(|(r, _)| r.contains(&p.sample))
            .context("invalid saved sample")?;
        let next = (p.sample + 1).min(span.end - 1);
        let predicted = &self.poses[next];
        if !p.move_root {
            let delta = predicted.heading - self.poses[p.sample].heading;
            p.yaw = (p.yaw + delta.sin().atan2(delta.cos())) % std::f32::consts::TAU;
        }
        // Once the controller settles, keep start/stop clips out of the steady
        // loop. During acceleration, braking and changes of direction all poses
        // remain available.
        let target = p
            .observed
            .map_or(p.target_velocity(), |m| m.allowed(p.target_velocity()));
        let speed = p.observed.map_or(p.velocity, |m| m.velocity);
        let class = if target.length_squared() < 1e-10 {
            (!p.move_root || speed.length_squared() < 0.08f32.powi(2))
                .then_some(search::Candidate::Idle)
        } else {
            (p.move_root && (speed - target).length() < target.length() * 0.26)
                .then_some(search::Candidate::Steady)
        };
        if p.search_in > 0
            && next + LOOKAHEAD < span.end
            && class.is_none_or(|candidate| candidate == self.search.candidate(p.sample))
        {
            p.sample = next;
            p.search_in -= 1;
            return Ok(None);
        }
        let raw = p.query_features(predicted);
        let hit = self
            .search
            .nearest(&raw, p.mode, next, span, &self.clips, class);
        ensure!(hit.cost.is_finite(), "motion query exceeds feature range");
        p.cost = hit.cost;
        p.sample = hit.sample;
        p.evaluated = hit.evaluated;
        let clip = self
            .clips
            .partition_point(|(range, _)| range.end <= hit.sample);
        p.clip.clone_from(&self.clips[clip].1);
        let jumped = hit.sample != next;
        p.search_in = if jumped { 7 } else { 4 };
        Ok(jumped.then_some((outgoing, next)))
    }

    pub(super) fn publish_locomotion(
        &self,
        id: Id,
        p: &mut State,
        cx: &mut SystemContext<'_>,
        dt: f32,
    ) -> Result<()> {
        let root = cx.local(id)?.context("missing motion root")?;
        let inherited = if p.standalone_velocity.is_none() {
            cx.component::<crate::movement::character::ResolvedMotion>(
                id,
                crate::movement::character::RESOLVED,
            )?
            .map(|m| m.velocity)
        } else {
            None
        };
        if let Some(velocity) = inherited {
            let local = root.to_scale_rotation_translation().1.conjugate() * velocity;
            p.velocity = Vec2::new(local.x, local.z);
            p.acceleration = Vec2::ZERO;
        }
        p.observed = None;
        let (mut requested, joints) = self.sample_pose(p, root, dt, |_, id| {
            cx.local(id)?.context("missing motion joint")
        })?;
        if let Some(velocity) = inherited {
            requested.w_axis = (root.w_axis.truncate() + velocity * dt).extend(1.);
        }
        if p.layout
            .as_ref()
            .is_none_or(|layout| layout.joints() != p.bones)
        {
            p.layout = Some(crate::scene::PoseLayout::new(p.bones.iter().copied())?);
        }
        let policy = cx.motion_policy(id, MotionPolicy::KINEMATIC)?;
        let resolved = RootMotionRequest::new(requested)?.resolve(policy, root, None)?;
        p.standalone_velocity = Some((resolved.w_axis - root.w_axis).truncate() / dt);
        if resolved != root {
            cx.set_pose([(id, resolved)])?;
        }
        cx.publish_pose(id, p.layout.as_ref().unwrap().clone(), joints)
    }

    /// The caller owns the previous base pose. Contact corrections must not feed into inertialization.
    pub(super) fn sample_pose(
        &self,
        p: &mut State,
        root: Mat4,
        dt: f32,
        mut previous_local: impl FnMut(usize, Id) -> Result<Mat4>,
    ) -> Result<(Mat4, Vec<Mat4>)> {
        ensure!(
            p.sample < self.poses.len()
                && (0. ..1.).contains(&p.fraction)
                && p.bones.len() == self.poses[p.sample].rotations.len(),
            "invalid motion binding"
        );
        let time = dt.min(0.25);
        let scale = root.to_scale_rotation_translation().0;
        let mut root = root.w_axis.truncate();
        let clip_root = p.move_root && matches!(p.root_source, profile::RootSource::Clip);
        if p.move_root {
            let travel = p.advance(time);
            let delta = travel.displacement;
            if !clip_root {
                root += Quat::from_rotation_y(p.yaw) * Vec3::new(delta.x, 0., delta.y);
                p.yaw = (p.yaw + travel.turn).rem_euclid(std::f32::consts::TAU);
            }
            p.velocity = travel.velocity;
            p.acceleration = travel.acceleration;
            p.yaw_rate = travel.yaw_rate;
        }
        let mut playback = 1.;
        if p.move_root {
            let motion = self.poses[p.sample].ground_velocity;
            let speed = motion.length();
            if speed > 0.25 && p.velocity.dot(motion) > 0. {
                // Adapt cadence within a small range while the controller
                // and baked clip are moving in the same direction.
                playback = (p.velocity.dot(motion) / speed.powi(2)).clamp(0.85, 1.15);
            }
        }
        let mut samples = time * HZ * playback;
        let mut jump = None;
        while samples > 0. {
            let weight = samples.min(1. - p.fraction);
            if clip_root {
                let pose = &self.poses[p.sample];
                let delta = pose.root_step * weight;
                // All fractions of a sample share its original facing, including after a save/reload.
                let facing = p.yaw - p.fraction * pose.turn_step;
                root += Quat::from_rotation_y(facing) * Vec3::new(delta.x, 0., delta.y);
                p.yaw += pose.turn_step * weight;
            }
            samples -= weight;
            p.fraction += weight;
            if p.fraction >= 1. {
                p.fraction = 0.;
                jump = self.step(p)?.or(jump);
            }
        }
        let pose = &self.poses[p.sample];
        let end = self
            .clips
            .iter()
            .find(|(r, _)| r.contains(&p.sample))
            .unwrap()
            .0
            .end;
        let after = &self.poses[(p.sample + 1).min(end - 1)];
        let rig = p.rig.as_deref().unwrap_or(&profile::DEFAULT.rig);
        let mut transition = jump.map(|(old, next)| {
            let old = &self.poses[old];
            let old_next = &self.poses[next];
            (
                old,
                old_next,
                locomotion::Transition {
                    elapsed: 0.,
                    hip: locomotion::Offset {
                        value: Vec3::ZERO,
                        velocity: rig.hip_offset((old_next.hip - old.hip) - (after.hip - pose.hip))
                            * HZ,
                    },
                    joints: Vec::with_capacity(p.bones.len()),
                },
            )
        });
        if let Some(active) = &mut p.transition {
            active.elapsed += time;
            if active.elapsed >= 0.4 || active.joints.len() != p.bones.len() {
                p.transition = None;
            }
        }
        let target_hip = p.hip_origin + rig.hip_offset(pose.hip.lerp(after.hip, p.fraction));
        let mut joints = Vec::with_capacity(p.bones.len());
        for (i, &bone) in p.bones.iter().enumerate() {
            let (scale, previous, mut translation) =
                previous_local(i, bone)?.to_scale_rotation_translation();
            let target = rig.rotation(i, pose.rotations[i].slerp(after.rotations[i], p.fraction));
            let rotation = if let Some((old, next, blend)) = &mut transition {
                blend.joints.push(locomotion::Offset {
                    value: locomotion::rotation_difference(target, previous),
                    velocity: (locomotion::rotation_difference(
                        rig.rotation(i, old.rotations[i]),
                        rig.rotation(i, next.rotations[i]),
                    ) - locomotion::rotation_difference(
                        rig.rotation(i, pose.rotations[i]),
                        rig.rotation(i, after.rotations[i]),
                    )) * HZ,
                });
                previous
            } else if let Some(blend) = &p.transition {
                Quat::from_scaled_axis(blend.joints[i].at(blend.elapsed)) * target
            } else {
                target
            };
            if i == rig.pelvis {
                if let Some((_, _, blend)) = &mut transition {
                    blend.hip.value = translation - target_hip;
                } else {
                    translation = target_hip
                        + p.transition
                            .as_ref()
                            .map_or(Vec3::ZERO, |blend| blend.hip.at(blend.elapsed));
                }
            }
            joints.push(Mat4::from_scale_rotation_translation(
                scale,
                rotation.normalize(),
                translation,
            ));
        }
        if let Some((_, _, blend)) = transition {
            p.transition = Some(blend);
        }
        Ok((
            Mat4::from_scale_rotation_translation(scale, Quat::from_rotation_y(p.yaw), root),
            joints,
        ))
    }
}
