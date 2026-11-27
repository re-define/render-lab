//! Landmark-aligned playback on top of locomotion, with a palm constraint during the vault.
use super::*;
use glam::Vec3Swizzles;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(super) enum Section {
    #[default]
    Gap,
    Rail,
    Beam,
    Bend,
    Console,
}
impl Section {
    fn next(self) -> Self {
        match self {
            Self::Gap => Self::Rail,
            Self::Rail => Self::Beam,
            Self::Beam => Self::Bend,
            Self::Bend | Self::Console => Self::Console,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Gap => "gap jump",
            Self::Rail => "supported vault",
            Self::Beam => "duck walk",
            Self::Bend => "corner",
            Self::Console => "console",
        }
    }
    fn settings(self, actions: &profile::Actions) -> profile::Action {
        match self {
            Self::Gap => actions.jump,
            Self::Rail => actions.vault,
            Self::Beam => actions.duck,
            _ => profile::Action::default(),
        }
    }
    pub fn practice(self, w: &mut World, id: Id, c: &mut Contact) -> Result<()> {
        ensure!(
            c.course.is_some(),
            "action practice requires the obstacle course"
        );
        ensure!(
            w.motion_policy(id, MotionPolicy::DYNAMIC)?.physics == PhysicsParticipation::Simulated,
            "course action practice requires a simulated character"
        );
        let clip = self.clip().context("expected jump, vault or duck")?;
        let fixtures = c.course.context("missing course reference")?;
        let transforms = w.transforms()?;
        let parent = w.entities[&id]
            .parent
            .map_or(Mat4::IDENTITY, |id| transforms[&id]);
        ensure!(
            parent.determinant().abs() > 1e-8,
            "singular course parent frame"
        );
        let reference = CourseFrame::new(Some(parent.inverse() * transforms[&fixtures.rail]));
        let entry = reference.point(clip.entry);
        let mut p = player(w, id)?.into_owned();
        p.yaw = reference.yaw;
        p.turn = 0.;
        p.yaw_rate = 0.;
        p.velocity = Vec2::ZERO;
        p.standalone_velocity = None;
        p.acceleration = Vec2::ZERO;
        p.speed = 0.;
        p.side = 0.;
        p.transition = None;
        p.search_in = 0;
        w.set_typed_component(id, PLAYER, p);
        w.entities
            .get_mut(&id)
            .context("missing contact actor")?
            .local = Mat4::from_rotation_translation(Quat::from_rotation_y(reference.yaw), entry);
        let mut b = w
            .typed_component::<dynamics::Body>(id, dynamics::BODY)?
            .context("missing character body")?
            .into_owned();
        b.reset_motion(entry);
        w.set_typed_component(id, dynamics::BODY, b);
        c.palm_load = 0.;
        c.section = self;
        c.play = Some(reference.start(&clip, self, c, entry));
        c.auto_walk = false;
        c.reach = 0.;
        c.pelvis = 0.;
        c.feet = Default::default();
        Ok(())
    }
    fn clip(self) -> Option<Clip> {
        let (data, center): (&[u8], f32) = match self {
            Self::Gap => (
                include_bytes!("../../../../../assets/motion/traversal/jump.bin"),
                course::GAP_Z,
            ),
            Self::Rail => (
                include_bytes!("../../../../../assets/motion/traversal/vault.bin"),
                course::RAIL_Z + 0.055,
            ),
            Self::Beam => (
                include_bytes!("../../../../../assets/motion/traversal/duck.bin"),
                course::GATE_Z,
            ),
            _ => return None,
        };
        let mut clip = Clip {
            data,
            entry: Vec3::Z * center,
        };
        clip.entry.z -= 0.5 * clip.span();
        Some(clip)
    }
}
pub(super) fn validate_actions(actions: &profile::Actions) -> Result<()> {
    for section in [Section::Gap, Section::Rail, Section::Beam] {
        section
            .settings(actions)
            .validate(section.clip().unwrap().end())?;
    }
    Ok(())
}

/// Actor-parent frame of the course. Playback freezes its destination when an action starts.
struct CourseFrame {
    yaw: f32,
    origin: Vec3,
}
impl CourseFrame {
    fn new(rail: Option<Mat4>) -> Self {
        let Some(rail) = rail else {
            return Self {
                yaw: 0.,
                origin: Vec3::ZERO,
            };
        };
        let z = rail.z_axis.truncate();
        let yaw = z.x.atan2(z.z);
        let center = rail.w_axis.truncate();
        let origin = center
            - Quat::from_rotation_y(yaw) * Vec3::new(0., course::RAIL_TOP - 0.035, course::RAIL_Z);
        Self { yaw, origin }
    }
    fn point(&self, local: Vec3) -> Vec3 {
        self.origin + Quat::from_rotation_y(self.yaw) * local
    }
    fn start(&self, clip: &Clip, section: Section, c: &Contact, origin: Vec3) -> Playback {
        Playback {
            origin,
            yaw: self.yaw,
            destination: Some(self.point(clip.entry + Vec3::Z * clip.span())),
            events: Some(
                section.settings(c.actions.as_deref().unwrap_or(&profile::DEFAULT.actions)),
            ),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Playback {
    pub frame: f32,
    origin: Vec3,
    yaw: f32,
    destination: Option<Vec3>,
    events: Option<profile::Action>,
    pub launched: bool,
    previous_frame: f32,
    pub landed: bool,
    #[serde(alias = "landing_offset")]
    pub ground_offset: Vec3,
}
pub(super) struct Clip {
    data: &'static [u8],
    entry: Vec3,
}
// Each little-endian frame stores planar travel, a bind-local hip offset and 21 local quaternions.
const JOINTS: usize = 21;
const FRAME_BYTES: usize = (4 + JOINTS * 4) * 4;
impl Clip {
    fn read(&self, f: usize, k: usize) -> f32 {
        let offset = f * FRAME_BYTES + k * 4;
        f32::from_le_bytes(self.data[offset..offset + 4].try_into().unwrap())
    }
    fn end(&self) -> f32 {
        (self.data.len() / FRAME_BYTES - 1) as f32
    }
    fn span(&self) -> f32 {
        self.read(self.end() as usize, 0)
    }
    fn root(&self, play: Playback) -> Vec3 {
        let i = play.frame as usize;
        let j = (i + 1).min(self.end() as usize);
        let a = self.read(i, 0);
        let b = self.read(j, 0);
        let span = self.span();
        play.origin.lerp(
            play.destination.unwrap_or(self.entry + Vec3::Z * span),
            (a + (b - a) * play.frame.fract()) / span,
        )
    }
}
impl Playback {
    fn action(self, section: Section) -> profile::Action {
        self.events
            .unwrap_or_else(|| section.settings(&profile::DEFAULT.actions))
    }
    pub fn forward(self) -> Vec3 {
        Quat::from_rotation_y(self.yaw) * Vec3::Z
    }
    pub fn valid(self, section: Section) -> bool {
        self.origin.is_finite()
            && self.destination.is_none_or(Vec3::is_finite)
            && self.yaw.is_finite()
            && self.ground_offset.is_finite()
            && (!self.landed || self.launched)
            && section.clip().is_some_and(|clip| {
                (0.0..=clip.end()).contains(&self.frame)
                    && self.action(section).validate(clip.end()).is_ok()
            })
    }
}

pub(super) fn steer(
    c: &mut Contact,
    p: &mut State,
    root: Vec3,
    target: Vec3,
    rail: Option<Mat4>,
    dt: f32,
) {
    let reference = CourseFrame::new(rail);
    if c.course.is_some()
        && let Some(clip) = c.section.clip()
    {
        if c.play.is_none()
            && c.auto_walk
            && root.xz().distance(reference.point(clip.entry).xz()) < 0.03
        {
            c.play = Some(reference.start(&clip, c.section, c, root));
        }
        if let Some(play) = &mut c.play {
            if play.frame < clip.end() {
                let end = if play.launched && !play.landed {
                    play.action(c.section)
                        .flight
                        .map_or(clip.end(), |f| f.landing - 0.5)
                } else {
                    clip.end()
                };
                play.previous_frame = play.frame;
                play.frame = (play.frame + dt * 30.).min(end);
                p.move_root = true;
                p.speed = 0.;
                p.side = 0.;
                p.turn = 0.;
                p.velocity = Vec2::ZERO;
                p.acceleration = Vec2::ZERO;
                p.yaw_rate = 0.;
                return;
            }
            c.section = c.section.next();
            c.play = None;
        }
    }
    if !c.auto_walk {
        return;
    }
    let pos = root;
    if c.course.is_some()
        && c.section == Section::Bend
        && pos.xz().distance(reference.point(course::TURN).xz()) < 0.09
    {
        c.section = Section::Console;
    }
    let (goal, heading) = if c.course.is_none() {
        (
            target + Quat::from_rotation_y(p.yaw) * Vec3::new(0.37, 0., -0.28),
            p.yaw,
        )
    } else if let Some(clip) = c.section.clip() {
        (reference.point(clip.entry), reference.yaw)
    } else if c.section == Section::Bend {
        (reference.point(course::TURN), reference.yaw)
    } else {
        let h = reference.yaw + std::f32::consts::FRAC_PI_2;
        (
            target + Quat::from_rotation_y(h) * Vec3::new(0.37, 0., -0.28),
            h,
        )
    };
    let delta = Quat::from_rotation_y(-p.yaw) * (goal - pos);
    let diff = heading - p.yaw;
    let v = if delta.xz().length() < 0.018 {
        Vec2::ZERO
    } else {
        (delta.xz() * 4.).clamp_length_max(1.55)
    };
    p.move_root = true;
    p.side = v.x;
    p.speed = v.y;
    p.turn = (4. * diff.sin().atan2(diff.cos())).clamp(-2.4, 2.4);
}

// The bank stores local hip displacement followed by the same 21 rotations as locomotion.
pub(super) struct ActionRig {
    joints: Vec<usize>,
    semantics: Option<Arc<profile::RigMap>>,
    hip_origin: Vec3,
}
impl ActionRig {
    pub fn bind(w: &World, actor: Id, r: &Rig) -> Result<Self> {
        let state = player(w, actor)?;
        let joints = state
            .bones
            .iter()
            .map(|id| {
                r.ids
                    .iter()
                    .position(|v| v == id)
                    .context("unbound traversal bone")
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(joints.len() == JOINTS, "unexpected traversal skeleton");
        Ok(Self {
            joints,
            semantics: state.rig.clone(),
            hip_origin: state.hip_origin,
        })
    }
    pub fn apply(&self, pose: &mut rig::Pose<'_>, section: Section, play: Playback) {
        let clip = section.clip().unwrap();
        let frame = play.frame.clamp(0., clip.end());
        let i = frame.floor() as usize;
        let j = (i + 1).min(clip.end() as usize);
        let t = frame.fract();
        let blend = smooth(frame / 7.) * smooth((clip.end() - frame) / 7.);
        let rig = self.semantics.as_deref().unwrap_or(&profile::DEFAULT.rig);
        for (slot, &bone) in self.joints.iter().enumerate() {
            let (scale, base, mut at) = pose.local[bone].to_scale_rotation_translation();
            let q = |f| {
                Quat::from_array(std::array::from_fn(|a| clip.read(f, 4 + slot * 4 + a)))
                    .normalize()
            };
            if slot == rig.pelvis {
                let h = |f| Vec3::new(clip.read(f, 1), clip.read(f, 2), clip.read(f, 3));
                at = at.lerp(self.hip_origin + rig.hip_offset(h(i).lerp(h(j), t)), blend);
            }
            pose.local[bone] = Mat4::from_scale_rotation_translation(
                scale,
                base.slerp(rig.rotation(slot, q(i).slerp(q(j), t)), blend)
                    .normalize(),
                at,
            );
        }
        pose.forward();
        // The original in-place action already depicts a nominal flight. Express its
        // pelvis residual relative to that reference, then inherit the simulated root.
        pose.translate_world(
            self.joints[rig.pelvis],
            -Vec3::Y * section.reference_height(play),
        );
    }
}
fn smooth(x: f32) -> f32 {
    let x = x.clamp(0., 1.);
    x * x * (3. - 2. * x)
}
impl Section {
    fn reference_height(self, p: Playback) -> f32 {
        let Some(profile::Flight {
            takeoff: start,
            landing: end,
            ..
        }) = p.action(self).flight
        else {
            return 0.;
        };
        let t = ((p.frame - start) / 30.).clamp(0., (end - start) / 30.);
        0.5 * 9.81 * t * ((end - start) / 30. - t)
    }
    // Stretch the in-air jump pose to the actual ballistic phase. Impact still owns landing.
    pub fn sample_flight(self, p: &mut Playback, elapsed: f32, remaining: f32) {
        if let Some(profile::Flight {
            takeoff: start,
            landing: end,
            ..
        }) = p.action(self).flight
        {
            p.frame =
                (start + (end - start) * elapsed / (elapsed + remaining)).clamp(start, end - 0.5);
        }
    }
    pub fn ground_goal(self, p: Playback) -> Vec3 {
        self.clip().unwrap().root(p) + p.ground_offset
    }
    pub fn launch(self, p: Playback) -> Option<(f32, f32)> {
        let flight = p.action(self).flight?;
        let (start, end, scale) = (flight.takeoff, flight.landing, flight.impulse_scale);
        if p.launched || !flight.crossed_takeoff(p.previous_frame, p.frame) {
            return None;
        }
        let clip = self.clip()?;
        let z = |frame| clip.root(Playback { frame, ..p }).dot(p.forward());
        Some(((z(end) - z(start)) * 30. / (end - start), scale))
    }
    pub fn land(self, p: &mut Playback, at: Vec3) {
        if let Some(profile::Flight { landing: end, .. }) = p.action(self).flight {
            p.frame = end;
            p.landed = true;
            p.ground_offset = (at - self.clip().unwrap().root(*p)) * Vec3::new(1., 0., 1.);
        }
    }
    // Older saves stored only an authored frame and planar root. Reconstruct the
    // nominal incoming state once when loading an airborne save.
    pub fn migrate_flight(self, p: &mut Playback, b: &mut dynamics::Body) {
        let Some(profile::Flight {
            takeoff: start,
            landing: end,
            ..
        }) = p.action(self).flight
        else {
            return;
        };
        if p.frame >= start && p.frame < end {
            let t = (p.frame - start) / 30.;
            let speed = self
                .launch(Playback {
                    previous_frame: 0.,
                    launched: false,
                    ..*p
                })
                .unwrap()
                .0;
            b.particle.position.y += self.reference_height(*p);
            b.particle.momentum = b.particle.mass
                * (Vec3::Y * (9.81 * ((end - start) / 60. - t)) + p.forward() * speed);
            b.flight_time = t;
            p.launched = true;
        } else if p.frame >= end {
            p.launched = true;
            p.landed = true;
        }
    }
}
fn palm_goal(r: &Rig, rail: Mat4) -> Option<(Vec3, Quat)> {
    if !rail.is_finite() || rail.determinant().abs() <= 1e-12 {
        return None;
    }
    let goal = rail.transform_point3(Vec3::new(-0.18, 0.5, 0.34));
    let fingers = -rail.z_axis.truncate().normalize();
    let normal = rail
        .inverse()
        .transpose()
        .transform_vector3(Vec3::NEG_Y)
        .normalize();
    let surface = glam::Mat3::from_cols(fingers.cross(normal), fingers, normal);
    Some((
        goal,
        Quat::from_mat3(&(surface * r.palm_frame.transpose())).normalize(),
    ))
}
/// Physical palm support uses the reachable interval of the two arm segments, independent of display IK.
pub(super) fn grip_load(r: &Rig, pose: &rig::Pose<'_>, play: Playback, rail: Mat4) -> f32 {
    let weight = play.action(Section::Rail).palm_weight(play.frame);
    if weight == 0. {
        return 0.;
    }
    let [shoulder, elbow, wrist] = r.arm;
    let Some((goal, orientation)) = palm_goal(r, rail) else {
        return 0.;
    };
    let palm = pose.world[wrist].transform_point3(r.palm_offset);
    let offset = orientation * pose.rotation(wrist).conjugate() * (palm - pose.position(wrist));
    let a = pose.position(shoulder).distance(pose.position(elbow));
    let b = pose.position(elbow).distance(pose.position(wrist));
    let distance = (goal - offset).distance(pose.position(shoulder));
    let miss = (distance - (a + b)).max((a - b).abs() - distance).max(0.);
    weight * (1. - miss / 0.01).clamp(0., 1.)
}
pub(super) fn brace(r: &Rig, pose: &mut rig::Pose<'_>, play: Playback, rail: Mat4) {
    let weight = play.action(Section::Rail).palm_weight(play.frame);
    if weight == 0. {
        return;
    }
    let [shoulder, _, wrist] = r.arm;
    let Some((goal, orientation)) = palm_goal(r, rail) else {
        return;
    };
    let palm = pose.world[wrist].transform_point3(r.palm_offset);
    let offset = palm - pose.position(wrist);
    let base = pose.rotation(wrist);
    let rotation = base.slerp(orientation, weight).normalize();
    let wrist_goal = palm.lerp(goal, weight) - rotation * base.conjugate() * offset;
    let pole = (pose.position(r.arm[1]) - pose.position(shoulder))
        .normalize_or_zero()
        .lerp(Vec3::new(-0.8, 0.1, -0.5), weight);
    pose.solve(r.arm, wrist_goal, pole, rotation);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edited_action_asset_is_validated_with_a_cached_rig() -> Result<()> {
        let world = super::super::scene()?;
        let id = actor(&world).unwrap();
        let mut schedule = Schedule::default();
        super::super::install(&world, &mut schedule)?;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
        let dt = 1. / 60.;
        let mut world = Arc::new(world);
        schedule.tick(&mut world, dt, &pool)?;
        let mut c = world
            .typed_component::<Contact>(id, SETTING)?
            .unwrap()
            .into_owned();
        let mut actions = profile::DEFAULT.actions.clone();
        actions.vault.palm = Some([4., 2., 6., 7.]);
        c.actions = Some(Arc::new(actions));
        Arc::make_mut(&mut world).set_typed_component(id, SETTING, c);
        assert!(schedule.tick(&mut world, dt, &pool).is_err());
        Ok(())
    }

    #[test]
    fn palm_frame_tracks_the_transformed_surface_normal() -> Result<()> {
        let w = super::super::scene()?;
        let r = Rig::bind(&w, actor(&w).unwrap(), &rig::children(&w))?;
        let rail = Mat4::from_scale(Vec3::new(0.5, 2., 1.)) * Mat4::from_rotation_z(0.6);
        let (_, orientation) = palm_goal(&r, rail).unwrap();
        let face = crate::physics::SupportFace::from_box(1, rail, 0.).unwrap();
        assert!((orientation * r.palm_frame.z_axis).dot(-face.normal) > 0.99999);
        assert!(palm_goal(&r, Mat4::ZERO).is_none());
        Ok(())
    }

    #[test]
    fn saved_takeoff_windows_and_rotated_reference_drive_the_root_curve() -> Result<()> {
        let world = super::super::scene()?;
        let id = actor(&world).unwrap();
        let mut contact = world
            .typed_component::<Contact>(id, SETTING)?
            .unwrap()
            .into_owned();
        let mut actions = profile::DEFAULT.actions.clone();
        let flight = actions.jump.flight.as_mut().unwrap();
        flight.takeoff = 13.;
        flight.landing = 27.;
        flight.impulse_scale = 0.8;
        actions.vault.palm = Some([2., 3., 5., 6.]);
        validate_actions(&actions)?;
        contact.actions = Some(Arc::new(actions));
        let yaw = std::f32::consts::FRAC_PI_2;
        let rotate = Quat::from_rotation_y(yaw);
        let rail = Mat4::from_rotation_translation(
            rotate,
            Vec3::new(0., course::RAIL_TOP - 0.035, 0.) + rotate * (Vec3::Z * course::RAIL_Z),
        );
        let reference = CourseFrame::new(Some(rail));
        let section = Section::Gap;
        let clip = section.clip().unwrap();
        let mut play = reference.start(&clip, section, &contact, reference.point(clip.entry));
        play.frame = 11.;
        assert!(section.launch(play).is_none());
        play.previous_frame = 11.;
        play.frame = 14.;
        let loaded: Playback = serde_json::from_slice(&serde_json::to_vec(&play)?)?;
        assert!(
            section
                .launch(Playback {
                    launched: true,
                    ..loaded
                })
                .is_none()
        );
        assert!(
            section
                .launch(Playback {
                    previous_frame: loaded.frame,
                    ..loaded
                })
                .is_none()
        );
        let (speed, k) = section.launch(loaded).unwrap();
        assert!(speed > 0. && k == 0.8 && loaded.forward().abs_diff_eq(Vec3::X, 1e-6));
        let destination = Vec3::Z * (clip.entry.z + clip.span());
        let at = section.ground_goal(Playback {
            frame: clip.end(),
            ..loaded
        });
        assert!(at.abs_diff_eq(rotate * destination, 1e-5));
        assert_eq!(contact.actions.as_ref().unwrap().vault.palm_weight(4.), 1.);
        assert_eq!(profile::DEFAULT.actions.vault.palm_weight(4.), 0.);
        Ok(())
    }
}
