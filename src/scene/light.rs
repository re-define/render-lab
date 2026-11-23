use super::*;
use glam::Quat;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightKind {
    Directional,
    Point { range: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Light {
    pub kind: LightKind,
    pub color: [f32; 3],
    pub intensity: f32,
    /// Apparent half angle of a directional emitter. Zero gives a hard shadow.
    #[serde(default)]
    pub angular_radius_degrees: f32,
    /// Fraction of this directional emitter integrated as diffuse sky irradiance.
    #[serde(default)]
    pub sky_fraction: f32,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

impl Light {
    pub fn point(color: [f32; 3], intensity: f32, range: f32) -> Self {
        Self {
            kind: LightKind::Point { range },
            color,
            intensity,
            enabled: true,
            angular_radius_degrees: 0.,
            sky_fraction: 0.,
        }
    }

    pub fn directional(color: [f32; 3], intensity: f32) -> Self {
        Self {
            kind: LightKind::Directional,
            color,
            intensity,
            enabled: true,
            angular_radius_degrees: 0.,
            sky_fraction: 0.,
        }
    }

    pub fn aiming(direction: Vec3) -> Result<Mat4> {
        let direction = direction.normalize_or_zero();
        ensure!(
            direction.is_finite() && direction != Vec3::ZERO,
            "expected a finite nonzero light direction"
        );
        Ok(Mat4::from_quat(Quat::from_rotation_arc(
            Vec3::NEG_Z,
            direction,
        )))
    }

    pub(crate) fn validate(self, transform: Mat4) -> Result<()> {
        ensure!(
            transform.is_finite()
                && self.intensity.is_finite()
                && self.intensity >= 0.
                && self
                    .color
                    .iter()
                    .all(|&c| c.is_finite() && c >= 0. && (c * self.intensity).is_finite()),
            "invalid light transform, linear color or intensity"
        );
        ensure!(
            (0. ..=89.).contains(&self.angular_radius_degrees)
                && (self.kind == LightKind::Directional || self.angular_radius_degrees == 0.),
            "angular radius must be 0 to 89 degrees and only directional lights have extent"
        );
        ensure!(
            (0. ..=1.).contains(&self.sky_fraction)
                && (self.kind == LightKind::Directional || self.sky_fraction == 0.),
            "sky fraction must be in [0,1] and only applies to directional lights"
        );
        match self.kind {
            LightKind::Directional => ensure!(
                transform.transform_vector3(Vec3::NEG_Z).normalize_or_zero() != Vec3::ZERO,
                "directional light transform has no direction"
            ),
            LightKind::Point { range } => ensure!(
                range.is_finite() && range > 0.,
                "point light range must be positive"
            ),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directional_extent_round_trips_and_old_scenes_keep_hard_shadows() -> Result<()> {
        let mut sun = Light::directional([1.; 3], 2.);
        let mut old = serde_json::to_value(sun)?;
        old.as_object_mut()
            .unwrap()
            .remove("angular_radius_degrees");
        old.as_object_mut().unwrap().remove("sky_fraction");
        assert_eq!(serde_json::from_value::<Light>(old)?, sun);
        sun.angular_radius_degrees = 0.27;
        sun.sky_fraction = 0.78;
        sun.validate(Mat4::IDENTITY)?;
        assert_eq!(
            serde_json::from_value::<Light>(serde_json::to_value(sun)?)?,
            sun
        );
        for radius in [-0.1, 90., f32::NAN] {
            assert!(
                Light {
                    angular_radius_degrees: radius,
                    ..sun
                }
                .validate(Mat4::IDENTITY)
                .is_err()
            );
        }
        sun.kind = LightKind::Point { range: 5. };
        assert!(sun.validate(Mat4::IDENTITY).is_err());
        sun.angular_radius_degrees = 0.;
        assert!(sun.validate(Mat4::IDENTITY).is_err());
        for fraction in [-0.1, 1.01, f32::NAN] {
            assert!(
                Light {
                    sky_fraction: fraction,
                    ..Light::directional([1.; 3], 1.)
                }
                .validate(Mat4::IDENTITY)
                .is_err()
            );
        }
        Ok(())
    }
}
