use super::*;
use std::hash::{DefaultHasher, Hash, Hasher};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderLight {
    pub id: Id,
    pub kind: LightKind,
    pub position: Vec3,
    pub direction: Vec3,
    pub radiance: [f32; 3],
    pub angular_radius_degrees: f32,
    pub sky_fraction: f32,
}

impl RenderLight {
    pub(super) fn extract(id: Id, light: Light, world: Mat4) -> Self {
        let directional = light.kind == LightKind::Directional;
        Self {
            id,
            kind: light.kind,
            position: if directional {
                Vec3::ZERO
            } else {
                world.w_axis.truncate()
            },
            direction: if directional {
                world.transform_vector3(Vec3::NEG_Z).normalize()
            } else {
                Vec3::ZERO
            },
            angular_radius_degrees: light.angular_radius_degrees,
            sky_fraction: light.sky_fraction,
            radiance: light.color.map(|c| c * light.intensity),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct GpuLight {
    position_kind: [f32; 4],
    direction_range: [f32; 4],
    radiance: [f32; 4],
}

impl GpuLight {
    /// A zero range is used for the legacy unbounded benchmark lamps.
    pub(crate) fn point(position: Vec3, radiance: Vec3, range: f32) -> Self {
        Self {
            position_kind: position.extend(1.).into(),
            direction_range: [0., 0., 0., range],
            radiance: radiance.extend(0.).into(),
        }
    }
}

impl From<&RenderLight> for GpuLight {
    fn from(light: &RenderLight) -> Self {
        let radiance = Vec3::from(light.radiance);
        match light.kind {
            LightKind::Point { range } => Self::point(light.position, radiance, range),
            LightKind::Directional => Self {
                position_kind: [0.; 4],
                direction_range: (-light.direction)
                    .extend(light.angular_radius_degrees.to_radians().sin())
                    .into(),
                radiance: radiance.extend(light.sky_fraction).into(),
            },
        }
    }
}

pub(crate) fn pack_lights(lights: &[GpuLight]) -> Arc<[u8]> {
    let mut packed = Vec::with_capacity(16 + size_of_val(lights));
    packed.extend_from_slice(bytemuck::bytes_of(&[lights.len() as u32, 0, 0, 0]));
    packed.extend_from_slice(bytemuck::cast_slice(lights));
    packed.into()
}

pub(crate) struct SceneLights {
    /// An authored table remains selected when every light is disabled.
    pub present: bool,
    pub entries: Vec<RenderLight>,
    pub bytes: Arc<[u8]>,
    pub signature: u64,
    pub diffuse_sky: [f32; 4],
}

impl SceneLights {
    pub(super) fn new(present: bool, entries: Vec<RenderLight>) -> Self {
        let bytes = pack_lights(&entries.iter().map(GpuLight::from).collect::<Vec<_>>());
        let mut key = DefaultHasher::new();
        bytes.hash(&mut key);
        entries.iter().for_each(|light| light.id.hash(&mut key));
        let diffuse_sky = entries
            .iter()
            .map(|l| Vec3::from(l.radiance) * l.sky_fraction)
            .sum::<Vec3>()
            .extend(0.)
            .to_array();
        Self {
            present,
            entries,
            bytes,
            signature: key.finish(),
            diffuse_sky,
        }
    }
}
