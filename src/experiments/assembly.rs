//! Scene components select providers for both freshly constructed and restored worlds.
use super::*;
use crate::rhi::RayCapabilities;
use anyhow::ensure;

pub(super) enum Primary {
    Visibility,
    Exclusive,
    Simulation,
}
pub(super) struct Registration {
    pub key: &'static str,
    pub primary: Primary,
    pub restore: Option<fn(&mut World) -> Result<()>>,
    pub install: fn(&World, &mut Assembly) -> Result<()>,
}

pub(super) struct Assembly {
    pub caps: RayCapabilities,
    pub materials: MaterialPrograms,
    pub schedule: Schedule,
    pub camera: Option<Camera>,
    pub technique: Option<Arc<dyn Technique>>,
    pub visibility: composition::GeometryModules,
}

const MODULES: &[Registration] = &[
    clusters::MODULE,
    vegetation::MODULE,
    effects::MODULE,
    cartoon::MODULE,
    vegetation::WIND_SYSTEM,
    motion::MODULE,
    motion::silver::MODULE,
    lighting::MODULE,
    gaussian::MODULE,
    texture::MODULE,
];

pub(super) struct SceneConfiguration {
    camera: Camera,
    materials: Arc<MaterialPrograms>,
    schedule: Schedule,
    technique: Arc<dyn Technique>,
}
impl SceneConfiguration {
    pub fn restore(world: &mut World) -> Result<()> {
        for m in MODULES {
            if let Some(restore) = m.restore
                && world.first_raw_component(m.key).is_some()
            {
                restore(world)?;
            }
        }
        Ok(())
    }
    pub fn prepare(
        world: &World,
        tasks: &rayon::ThreadPool,
        caps: RayCapabilities,
    ) -> Result<Self> {
        let has = |key| world.first_raw_component(key).is_some();
        let active = MODULES.iter().filter(|m| has(m.key));
        let lights = light_stack::REGISTRY
            .iter()
            .find(|r| has(r.key) && composition::enabled(world, r.feature));
        let mut owners = active
            .clone()
            .filter(|m| matches!(m.primary, Primary::Exclusive));
        if let Some(exclusive) = owners.next() {
            let other = owners
                .next()
                .map(|m| m.key)
                .or_else(|| {
                    active
                        .clone()
                        .find(|m| matches!(m.primary, Primary::Visibility))
                        .map(|m| m.key)
                })
                .or_else(|| lights.map(|m| m.key));
            if let Some(other) = other {
                anyhow::bail!(
                    "incompatible primary render techniques: {} and {other}",
                    exclusive.key
                );
            }
        }
        for light in light_stack::REGISTRY {
            if let Some(check) = light.validate_device
                && has(light.key)
                && composition::enabled(world, light.feature)
            {
                check(world, caps)?;
            }
        }
        if let Some((_, view)) = world.first_raw_component(crate::render::rays::VIEW) {
            ensure!(
                view.as_str() != Some("hardware") || caps.pipeline,
                "hardware inspection requires VK_KHR_ray_tracing_pipeline"
            );
        }
        let mut a = Assembly {
            caps,
            materials: MaterialPrograms::default(),
            schedule: Schedule::default(),
            camera: None,
            technique: None,
            visibility: Default::default(),
        };
        for m in active {
            (m.install)(world, &mut a)?;
        }
        a.schedule.bind_transforms(world)?;
        let (technique, camera) = match a.technique {
            Some(technique) => (
                technique,
                a.camera
                    .ok_or_else(|| anyhow::anyhow!("primary experiment needs a camera"))?,
            ),
            None => composition::assemble(world, tasks, &mut a.materials, a.visibility, a.camera)?,
        };
        Ok(Self {
            camera,
            materials: Arc::new(a.materials),
            schedule: a.schedule,
            technique,
        })
    }
    pub fn apply_layers(mut self, e: &mut Engine, uses_same_systems: bool) {
        self.camera = e.camera;
        if uses_same_systems {
            self.schedule = std::mem::take(&mut e.schedule);
        }
        self.apply(e);
    }
    pub fn apply(self, e: &mut Engine) {
        e.camera = self.camera;
        e.materials = self.materials;
        e.schedule = self.schedule;
        e.technique = self.technique;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incompatible_primary_modules_are_rejected_before_assets_are_opened() {
        let mut world = World::default();
        let mut entity = crate::scene::Entity::default();
        for key in [texture::SETTING, gaussian::SETTING] {
            entity.components.insert(key.into(), serde_json::json!({}));
        }
        world.spawn(entity);
        let tasks = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let error = SceneConfiguration::prepare(&world, &tasks, Default::default())
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains(texture::SETTING) && error.contains(gaussian::SETTING));
    }
}
