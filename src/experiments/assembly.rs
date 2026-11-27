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
    pub root_writers: &'static [crate::scene::RootWriter],
    pub primary: Primary,
    pub restore: Option<fn(&mut World) -> Result<()>>,
    pub install: fn(&World, &mut Assembly) -> Result<()>,
}

pub(super) struct Assembly {
    pub caps: RayCapabilities,
    pub materials: MaterialPrograms,
    pub schedule: Schedule,
    pub movement: Option<crate::engine::MovementDriver>,
    pub camera: Option<Camera>,
    pub technique: Option<Arc<dyn Technique>>,
    pub visibility: composition::GeometryModules,
}

pub(super) static ROOT_PROVIDERS: std::sync::LazyLock<Vec<crate::scene::RootWriter>> =
    std::sync::LazyLock::new(|| {
        MODULES
            .iter()
            .flat_map(|m| m.root_writers.iter().copied())
            .collect()
    });
const MODULES: &[Registration] = &[
    super::movement_lab::MODULE,
    clusters::MODULE,
    vegetation::MODULE,
    effects::MODULE,
    cartoon::MODULE,
    vegetation::WIND_SYSTEM,
    motion::MODULE,
    motion::silver::MODULE,
    motion::silver::contact::MODULE,
    lighting::MODULE,
    gaussian::MODULE,
    texture::MODULE,
];

pub(super) struct SceneConfiguration {
    camera: Camera,
    materials: Arc<MaterialPrograms>,
    schedule: Schedule,
    movement: Option<crate::engine::MovementDriver>,
    technique: Arc<dyn Technique>,
}
impl SceneConfiguration {
    pub fn restore(world: &mut World) -> Result<()> {
        world.install_motion_providers(&ROOT_PROVIDERS)?;
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
            movement: None,
            camera: None,
            technique: None,
            visibility: Default::default(),
        };
        for m in active {
            (m.install)(world, &mut a)?;
        }
        a.schedule.restore_clocks(world)?;
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
            movement: a.movement,
            technique,
        })
    }
    pub fn apply_layers(mut self, e: &mut Engine) {
        self.camera = e.camera;
        e.movement.rebind(self.movement);
        self.apply_rendering_and_schedule(e);
    }
    pub fn apply(self, e: &mut Engine) {
        e.movement = crate::engine::MovementControls::new(self.movement);
        self.apply_rendering_and_schedule(e);
    }
    fn apply_rendering_and_schedule(self, e: &mut Engine) {
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
    fn simulation_roles_are_resolved_before_workers_start() -> Result<()> {
        use crate::{
            movement::linear::LinearBody,
            scene::{Entity, MotionPolicy},
        };
        let mut world = World::default();
        let free = world.spawn(Entity::default());
        world.set_motion_policy(free, MotionPolicy::DYNAMIC)?;
        SceneConfiguration::restore(&mut world)?;
        assert!(world.linear_body(free)?.is_some());

        let animated = world.spawn(Entity {
            components: [(motion::silver::PLAYER.into(), serde_json::Value::Null)].into(),
            ..Default::default()
        });
        assert!(
            world
                .set_linear_body(animated, LinearBody::default())
                .is_err()
        );
        world.set_motion_policy(animated, MotionPolicy::DYNAMIC)?;
        assert!(
            world
                .install_motion_providers(&ROOT_PROVIDERS)
                .unwrap_err()
                .to_string()
                .contains("physics solver")
        );
        Ok(())
    }

    #[test]
    fn two_simulation_providers_cannot_claim_the_same_root() {
        let mut world = World::default();
        world.spawn(crate::scene::Entity {
            components: [
                (effects::SHARD.into(), serde_json::Value::Null),
                (
                    motion::silver::contact::TURNTABLE.into(),
                    serde_json::Value::Null,
                ),
            ]
            .into(),
            ..Default::default()
        });
        let error = SceneConfiguration::restore(&mut world)
            .unwrap_err()
            .to_string();
        assert!(error.contains("conflicting root motion providers"));
        assert!(
            error.contains(effects::SHARD) && error.contains(motion::silver::contact::TURNTABLE)
        );
    }

    #[test]
    #[ignore = "requires a Vulkan device"]
    fn failed_configuration_keeps_the_live_world_and_schedule() -> Result<()> {
        use crate::scene::{Entity, MotionPolicy};
        let mut engine = crate::experiments::open(movement_lab::scene()?)?;
        engine.schedule.set_tick_rate(
            crate::movement::linear::CLOCK,
            crate::scene::TickRate::Hertz(30),
        )?;
        engine
            .schedule
            .tick(&mut engine.world, 0.01, &engine.tasks)?;
        let previous = engine.schedule.describe_clocks();
        crate::experiments::configure(&mut engine)?;
        assert_eq!(engine.schedule.describe_clocks(), previous);
        let live = Arc::make_mut(&mut engine.world);
        let id = live.spawn(Entity::default());
        live.set_motion_policy(id, MotionPolicy::DYNAMIC)?;
        let mut incompatible = Entity::default();
        for key in [texture::SETTING, gaussian::SETTING] {
            incompatible
                .components
                .insert(key.into(), serde_json::json!({}));
        }
        live.spawn(incompatible);
        let before = engine.world.clone();
        let clocks = engine.schedule.describe();
        assert!(crate::experiments::configure(&mut engine).is_err());
        assert!(Arc::ptr_eq(&before, &engine.world));
        assert!(engine.world.linear_body(id)?.is_none());
        assert_eq!(clocks, engine.schedule.describe());
        Ok(())
    }

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
