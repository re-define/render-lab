use glam::{Quat, Vec2, Vec3};
use serde::{Deserialize, Serialize};

/// Measurements expressed in the character facing frame, in the bank's metres and seconds.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub(super) struct Observed {
    pub velocity: Vec2,
    pub blocking_normal: Option<Vec2>,
    pub planted: [bool; 2], // bank order: left, right
}
impl Observed {
    pub fn allowed(self, intent: Vec2) -> Vec2 {
        match self.blocking_normal {
            Some(n) => intent - n * intent.dot(n).min(0.),
            None => intent,
        }
    }
}

/// Integrate a critically damped velocity target and its displacement together.
/// Prediction and real movement use this same step with different time horizons.
pub(super) fn integrate(
    velocity: Vec2,
    acceleration: Vec2,
    target: Vec2,
    time: f32,
) -> (Vec2, Vec2, Vec2) {
    let rate = if target == Vec2::ZERO { 17. } else { 14. };
    let gap = velocity - target;
    let slope = acceleration + rate * gap;
    let decay = (-rate * time).exp();
    let area = -(-rate * time).exp_m1() / rate;
    (
        target * time + gap * area + slope * ((area - time * decay) / rate),
        target + (gap + slope * time) * decay,
        (acceleration - rate * slope * time) * decay,
    )
}

pub(super) struct Travel {
    pub displacement: Vec2,
    pub velocity: Vec2,
    pub acceleration: Vec2,
    pub turn: f32,
    pub yaw_rate: f32,
}

/// Predict the arc in the character's current facing frame. The velocity
/// springs live in local space, so movement rotates as the character turns.
pub(super) fn travel(
    velocity: Vec2,
    acceleration: Vec2,
    yaw_rate: f32,
    target: Vec2,
    target_turn: f32,
    time: f32,
) -> Travel {
    let (distance, velocity_end, acceleration_end) =
        integrate(velocity, acceleration, target, time);
    let yaw_at = |t: f32| target_turn * t + (yaw_rate - target_turn) * (-(-11. * t).exp_m1() / 11.);
    let turning = target_turn != 0. || yaw_rate.abs() >= 0.001;
    let mut displacement = distance;
    if turning {
        let steps = ((time * 60.).ceil() as u32).max(1);
        let step = time / steps as f32;
        displacement = Vec2::ZERO;
        let mut last = Vec2::ZERO;
        for i in 1..=steps {
            let dt = i as f32 * step;
            let next = integrate(velocity, acceleration, target, dt).0;
            let angle = yaw_at(dt - step * 0.5);
            let (sin, cos) = angle.sin_cos();
            let delta = next - last;
            displacement += Vec2::new(cos * delta.x + sin * delta.y, cos * delta.y - sin * delta.x);
            last = next;
        }
    }
    Travel {
        displacement,
        velocity: velocity_end,
        acceleration: acceleration_end,
        turn: if turning { yaw_at(time) } else { 0. },
        yaw_rate: if turning {
            target_turn + (yaw_rate - target_turn) * (-11. * time).exp()
        } else {
            0.
        },
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Offset {
    pub value: Vec3,
    pub velocity: Vec3,
}

impl Offset {
    pub fn at(&self, time: f32) -> Vec3 {
        let decay = 24.;
        (self.value + (self.velocity + self.value * decay) * time) * (-decay * time).exp()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Transition {
    pub elapsed: f32,
    pub hip: Offset,
    pub joints: Vec<Offset>,
}

pub(super) fn rotation_difference(start: Quat, end: Quat) -> Vec3 {
    let relative = (end * start.conjugate()).normalize();
    (if relative.w < 0. { -relative } else { relative }).to_scaled_axis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forecast_curves_and_matches_substeps_under_held_steering() {
        let target = Vec2::Y * 1.35;
        let seconds = 23. / 30.;
        for turn in [-1.35, 1.35] {
            let first = travel(Vec2::ZERO, Vec2::ZERO, 0., target, turn, 0.2);
            let end = travel(Vec2::ZERO, Vec2::ZERO, 0., target, turn, seconds);
            assert!((first.displacement.perp_dot(end.displacement)).abs() > 0.015);
            assert_eq!(end.displacement.x.signum(), turn.signum());
            assert_eq!(end.turn.signum(), turn.signum());

            let (mut velocity, mut acceleration, mut yaw_rate) = (Vec2::ZERO, Vec2::ZERO, 0.);
            let (mut position, mut yaw) = (Vec3::ZERO, 0.);
            for _ in 0..23 {
                let sample = travel(velocity, acceleration, yaw_rate, target, turn, 1. / 30.);
                position += Quat::from_rotation_y(yaw)
                    * Vec3::new(sample.displacement.x, 0., sample.displacement.y);
                yaw += sample.turn;
                (velocity, acceleration, yaw_rate) =
                    (sample.velocity, sample.acceleration, sample.yaw_rate);
            }
            assert!(
                position.abs_diff_eq(Vec3::new(end.displacement.x, 0., end.displacement.y), 0.001)
            );
            assert!((yaw - end.turn).abs() < 1e-5);
            let reverse = travel(velocity, acceleration, yaw_rate, target, -turn, 0.03);
            assert_eq!(
                reverse.turn.signum(),
                turn.signum(),
                "yaw should not snap on reversal"
            );
        }
    }
}
