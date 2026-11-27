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

#[derive(Clone)]
pub(crate) struct SceneLights {
    /// An authored table remains selected when every light is disabled.
    pub present: bool,
    pub entries: Vec<RenderLight>,
    pub bytes: Arc<[u8]>,
    pub signature: u64,
    pub diffuse_sky: [f32; 4],
}

impl SceneLights {
    fn hash(light: &RenderLight) -> u64 {
        let mut hash = DefaultHasher::new();
        light.id.hash(&mut hash);
        bytemuck::bytes_of(&GpuLight::from(light)).hash(&mut hash);
        hash.finish()
    }

    pub(super) fn entry(&self, id: Id) -> Option<&RenderLight> {
        self.entries
            .binary_search_by_key(&id, |light| light.id)
            .ok()
            .map(|i| &self.entries[i])
    }

    pub(super) fn update(&mut self, changes: Vec<(Id, Option<RenderLight>)>) {
        let mut layout_changed = false;
        let mut sky_changed = false;
        for (id, new) in changes {
            let at = self.entries.binary_search_by_key(&id, |light| light.id);
            if let Ok(i) = at {
                let old = &self.entries[i];
                self.signature ^= Self::hash(old);
                sky_changed |= old.sky_fraction != 0.;
            }
            if let Some(new) = new {
                self.signature ^= Self::hash(&new);
                sky_changed |= new.sky_fraction != 0.;
                match at {
                    Ok(i) => {
                        self.entries[i] = new;
                        if !layout_changed {
                            let stride = size_of::<GpuLight>();
                            let start = 16 + stride * i;
                            Arc::make_mut(&mut self.bytes)[start..start + stride]
                                .copy_from_slice(bytemuck::bytes_of(&GpuLight::from(&new)));
                        }
                    }
                    Err(i) => {
                        self.entries.insert(i, new);
                        layout_changed = true;
                    }
                }
            } else if let Ok(i) = at {
                self.entries.remove(i);
                layout_changed = true;
            }
        }
        if layout_changed {
            self.bytes = pack_lights(&self.entries.iter().map(GpuLight::from).collect::<Vec<_>>());
        }
        if sky_changed {
            self.diffuse_sky = Self::sky(&self.entries);
        }
    }

    fn sky(entries: &[RenderLight]) -> [f32; 4] {
        entries
            .iter()
            .map(|l| Vec3::from(l.radiance) * l.sky_fraction)
            .sum::<Vec3>()
            .extend(0.)
            .to_array()
    }

    pub(super) fn new(present: bool, entries: Vec<RenderLight>) -> Self {
        let bytes = pack_lights(&entries.iter().map(GpuLight::from).collect::<Vec<_>>());
        let signature = entries.iter().fold(0, |sum, light| sum ^ Self::hash(light));
        let diffuse_sky = Self::sky(&entries);
        Self {
            present,
            entries,
            bytes,
            signature,
            diffuse_sky,
        }
    }
}
