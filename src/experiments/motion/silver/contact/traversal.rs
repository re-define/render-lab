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
        let mut p = player(w, id)?.into_owned();
        p.yaw = 0.;
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
            .local = Mat4::from_translation(clip.entry);
        let mut b = w
            .typed_component::<dynamics::Body>(id, dynamics::BODY)?
            .context("missing character body")?
            .into_owned();
        b.reset_motion(clip.entry);
        w.set_typed_component(id, dynamics::BODY, b);
        c.palm_load = 0.;
        c.section = self;
        c.play = Some(Playback {
            frame: 0.,
            origin: clip.entry,
            ..Default::default()
        });
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
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Playback {
    pub frame: f32,
    origin: Vec3,
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
            self.entry + Vec3::Z * span,
            (a + (b - a) * play.frame.fract()) / span,
        )
    }
}
impl Playback {
    pub fn valid(self, section: Section) -> bool {
        self.origin.is_finite()
            && self.ground_offset.is_finite()
            && (!self.landed || self.launched)
            && section
                .clip()
                .is_some_and(|clip| (0.0..=clip.end()).contains(&self.frame))
    }
}

pub(super) fn steer(c: &mut Contact, p: &mut State, root: Vec3, target: Vec3, dt: f32) {
    if c.course.is_some()
        && let Some(clip) = c.section.clip()
    {
        if c.play.is_none() && c.auto_walk && root.xz().distance(clip.entry.xz()) < 0.03 {
            c.play = Some(Playback {
                frame: 0.,
                origin: root,
                ..Default::default()
            });
        }
        if let Some(play) = &mut c.play {
            if play.frame < clip.end() {
                let end = if play.launched && !play.landed {
                    events::flight(c.section).map_or(clip.end(), |f| f.landing - 0.5)
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
        && pos.xz().distance(course::TURN.xz()) < 0.09
    {
        c.section = Section::Console;
    }
    let (goal, heading) = if c.course.is_none() {
        (
            target + Quat::from_rotation_y(p.yaw) * Vec3::new(0.37, 0., -0.28),
            p.yaw,
        )
    } else if let Some(clip) = c.section.clip() {
        (clip.entry, 0.)
    } else if c.section == Section::Bend {
        (course::TURN, 0.)
    } else {
        let h = std::f32::consts::FRAC_PI_2;
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
        for (slot, &bone) in self.joints.iter().enumerate() {
            let (scale, base, mut at) = pose.local[bone].to_scale_rotation_translation();
            let q = |f| {
                Quat::from_array(std::array::from_fn(|a| clip.read(f, 4 + slot * 4 + a)))
                    .normalize()
            };
            if slot == 0 {
                let h = |f| Vec3::new(clip.read(f, 1), clip.read(f, 2), clip.read(f, 3));
                at = at.lerp(self.hip_origin + h(i).lerp(h(j), t), blend);
            }
            pose.local[bone] = Mat4::from_scale_rotation_translation(
                scale,
                base.slerp(q(i).slerp(q(j), t), blend).normalize(),
                at,
            );
        }
        pose.forward();
        // The original in-place action already depicts a nominal flight. Express its
        // pelvis residual relative to that reference, then inherit the simulated root.
        pose.translate_world(self.joints[0], -Vec3::Y * section.reference_height(frame));
    }
}
fn smooth(x: f32) -> f32 {
    let x = x.clamp(0., 1.);
    x * x * (3. - 2. * x)
}
impl Section {
    fn reference_height(self, frame: f32) -> f32 {
        let Some(events::Flight {
            takeoff: start,
            landing: end,
            ..
        }) = events::flight(self)
        else {
            return 0.;
        };
        let t = ((frame - start) / 30.).clamp(0., (end - start) / 30.);
        0.5 * 9.81 * t * ((end - start) / 30. - t)
    }
    // Stretch the in-air jump pose to the actual ballistic phase. Impact still owns landing.
    pub fn sample_flight(self, p: &mut Playback, elapsed: f32, remaining: f32) {
        if let Some(events::Flight {
            takeoff: start,
            landing: end,
            ..
        }) = events::flight(self)
        {
            p.frame =
                (start + (end - start) * elapsed / (elapsed + remaining)).clamp(start, end - 0.5);
        }
    }
    pub fn ground_goal(self, p: Playback) -> Vec3 {
        self.clip().unwrap().root(p) + p.ground_offset
    }
    pub fn launch(self, p: Playback) -> Option<(f32, f32)> {
        let flight = events::flight(self)?;
        let (start, end, scale) = (flight.takeoff, flight.landing, flight.impulse_scale);
        if p.launched || !flight.crossed_takeoff(p.previous_frame, p.frame) {
            return None;
        }
        let clip = self.clip()?;
        let z = |frame| clip.root(Playback { frame, ..p }).z;
        Some(((z(end) - z(start)) * 30. / (end - start), scale))
    }
    pub fn land(self, p: &mut Playback, at: Vec3) {
        if let Some(events::Flight { landing: end, .. }) = events::flight(self) {
            p.frame = end;
            p.landed = true;
            p.ground_offset = (at - self.clip().unwrap().root(*p)) * Vec3::new(1., 0., 1.);
        }
    }
    // Older saves stored only an authored frame and planar root. Reconstruct the
    // nominal incoming state once when loading an airborne save.
    pub fn migrate_flight(self, p: &mut Playback, b: &mut dynamics::Body) {
        let Some(events::Flight {
            takeoff: start,
            landing: end,
            ..
        }) = events::flight(self)
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
            b.particle.position.y += self.reference_height(p.frame);
            b.particle.momentum =
                b.particle.mass * Vec3::new(0., 9.81 * ((end - start) / 60. - t), speed);
            b.flight_time = t;
            p.launched = true;
        } else if p.frame >= end {
            p.launched = true;
            p.landed = true;
        }
    }
}
// Heel of the right glove, in wrist bind space. The contact frame keeps the palm flat.
pub(super) const PALM: Vec3 = Vec3::new(0.02223109, 0.02361779, -0.01044704);
pub(super) fn brace(r: &Rig, pose: &mut rig::Pose<'_>, frame: f32, rail: Mat4) -> f32 {
    let weight = events::VAULT_PALM.weight(frame);
    if weight == 0. {
        return 0.;
    }
    let [shoulder, _, wrist] = r.arm;
    let goal = rail.transform_point3(Vec3::new(-0.18, 0.5, 0.34));
    let palm = pose.world[wrist].transform_point3(PALM);
    let offset = palm - pose.position(wrist);
    let local_normal = Vec3::new(0.51, 0., -0.86).normalize();
    let local_frame = glam::Mat3::from_cols(Vec3::Y.cross(local_normal), Vec3::Y, local_normal);
    let fingers = -rail.z_axis.truncate().normalize();
    let normal = -rail.y_axis.truncate().normalize();
    let surface = glam::Mat3::from_cols(fingers.cross(normal), fingers, normal);
    let base = pose.rotation(wrist);
    let rotation = base
        .slerp(
            Quat::from_mat3(&(surface * local_frame.transpose())),
            weight,
        )
        .normalize();
    let wrist_goal = palm.lerp(goal, weight) - rotation * base.conjugate() * offset;
    let pole = (pose.position(r.arm[1]) - pose.position(shoulder))
        .normalize_or_zero()
        .lerp(Vec3::new(-0.8, 0.1, -0.5), weight);
    pose.solve(r.arm, wrist_goal, pole, rotation);
    let gap = pose.world[wrist].transform_point3(PALM).distance(goal);
    weight * (1. - gap / 0.01).clamp(0., 1.)
}
