//! Trajectory integration and Motion Matching query features for one actor.
use super::*;

impl State {
    pub(super) fn observe_motion(
        &mut self,
        root: Mat4,
        velocity: Vec3,
        support_velocity: Vec3,
        normal: Option<Vec3>,
        feet: [bool; 2],
    ) {
        let facing = root.to_scale_rotation_translation().1.conjugate();
        let planar = |v: Vec3| {
            let local = facing * v;
            Vec2::new(local.x, local.z)
        };
        let blocking_normal = normal
            .filter(|n| n.y.abs() < 0.7)
            .and_then(|n| planar(n).try_normalize());
        let observation = locomotion::Observed {
            velocity: planar(velocity - support_velocity),
            blocking_normal,
            planted: [feet[1], feet[0]],
        };
        if self.observed.and_then(|p| p.blocking_normal).is_some() != blocking_normal.is_some() {
            self.search_in = 0;
        }
        self.observed = Some(observation);
    }
    pub(super) fn query_features(&self, predicted: &Pose) -> search::Feature {
        let mut raw = [0.; 24];
        for (i, time) in HORIZONS.into_iter().enumerate() {
            let (path, facing) = self.predict(time);
            raw[4 * i..4 * i + 2].copy_from_slice(&path.to_array());
            raw[4 * i + 2..4 * i + 4].copy_from_slice(&facing.to_array());
        }
        raw[12..].copy_from_slice(&predicted.feet);
        if let Some(m) = self.observed {
            for (foot, planted) in m.planted.into_iter().enumerate() {
                if planted {
                    // A planted ankle on its support moves opposite the root in bank-local space.
                    raw[18 + foot * 3] = -m.velocity.x;
                    raw[20 + foot * 3] = -m.velocity.y;
                }
            }
        }
        raw
    }
    pub(super) fn target_velocity(&self) -> Vec2 {
        Vec2::new(self.side, self.speed)
    }

    pub(super) fn advance(&self, time: f32) -> locomotion::Travel {
        locomotion::travel(
            self.velocity,
            self.acceleration,
            self.yaw_rate,
            self.target_velocity(),
            self.turn,
            time,
        )
    }

    pub(super) fn predict(&self, time: f32) -> (Vec2, Vec2) {
        if self.move_root {
            let travel = self.observed.map_or_else(
                || self.advance(time),
                |m| {
                    locomotion::travel(
                        m.velocity,
                        m.allowed(self.acceleration),
                        self.yaw_rate,
                        m.allowed(self.target_velocity()),
                        self.turn,
                        time,
                    )
                },
            );
            return (
                travel.displacement,
                Vec2::new(travel.turn.sin(), travel.turn.cos()),
            );
        }
        let (sin, cos) = (self.turn * time).sin_cos();
        let displacement = if self.turn.abs() < 0.0001 {
            self.target_velocity() * time
        } else {
            (Vec2::new(1. - cos, sin) * self.speed + Vec2::new(sin, cos - 1.) * self.side)
                / self.turn
        };
        (displacement, Vec2::new(sin, cos))
    }
}
