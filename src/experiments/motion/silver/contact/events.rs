//! Authored contact events, in source frames at 30 Hz.
use super::traversal::Section;
#[derive(Clone, Copy)]
pub(super) struct Flight {
    pub takeoff: f32,
    pub landing: f32,
    pub impulse_scale: f32,
}
impl Flight {
    pub fn crossed_takeoff(self, previous: f32, current: f32) -> bool {
        previous < self.takeoff && current >= self.takeoff
    }
}
pub(super) struct ContactWindow {
    start: f32,
    planted: f32,
    release: f32,
    end: f32,
}
impl ContactWindow {
    pub fn weight(&self, frame: f32) -> f32 {
        let smooth = |t: f32| {
            let t = t.clamp(0., 1.);
            t * t * (3. - 2. * t)
        };
        smooth((frame - self.start) / (self.planted - self.start))
            * smooth((self.end - frame) / (self.end - self.release))
    }
}
pub(super) const VAULT_PALM: ContactWindow = ContactWindow {
    start: 9.,
    planted: 13.5,
    release: 17.5,
    end: 22.,
};
pub(super) fn flight(section: Section) -> Option<Flight> {
    let (takeoff, landing, impulse_scale) = match section {
        Section::Gap => (10.5, 28., 1.),
        Section::Rail => (9.5, 31., 1.16),
        _ => return None,
    };
    Some(Flight {
        takeoff,
        landing,
        impulse_scale,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn skipped_samples_fire_once_and_a_held_pose_does_not_retrigger() {
        let jump = flight(Section::Gap).unwrap();
        assert!(jump.crossed_takeoff(9., 12.));
        assert!(!jump.crossed_takeoff(10.5, 10.5));
        assert!(!jump.crossed_takeoff(12., 13.));
        assert_eq!(VAULT_PALM.weight(13.5), 1.);
        assert_eq!(VAULT_PALM.weight(22.), 0.);
    }
}
