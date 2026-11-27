//! Scene-wide render components use the lowest entity ID carrying their key.
use super::*;
use serde::de::DeserializeOwned;
use std::{any::Any, borrow::Cow};

pub type SnapshotValue = Arc<dyn Any + Send + Sync>;
pub type ExtractComponent = fn(&mut SnapshotContext<'_>) -> Result<Option<SnapshotValue>>;

/// Settings and optional immutable output supplied by one component owner.
#[derive(Clone, Copy)]
pub struct SnapshotComponent {
    pub key: &'static str,
    pub copy_setting: bool,
    pub related_settings: &'static [&'static str],
    pub extract: Option<ExtractComponent>,
}

impl SnapshotComponent {
    pub const fn setting(key: &'static str) -> Self {
        Self {
            key,
            copy_setting: true,
            related_settings: &[],
            extract: None,
        }
    }
}

/// Component reads are local to the owner. Referenced world transforms become dependencies.
pub struct SnapshotContext<'a> {
    pub owner: Id,
    key: &'static str,
    world: &'a World,
    transforms: &'a TransformCache,
    watched: &'a mut BTreeMap<Id, Option<u64>>,
    reads: &'a mut Vec<Id>,
    settings: &'a mut BTreeMap<&'static str, Option<Id>>,
}

impl SnapshotContext<'_> {
    pub fn raw(&self) -> &Value {
        &self.world.entities[&self.owner].components[self.key]
    }

    pub fn typed<T: Clone + DeserializeOwned + 'static>(&self) -> Result<Cow<'_, T>> {
        self.world
            .typed_component(self.owner, self.key)?
            .context("missing component owner")
    }

    /// Subscribe to the membership and values of every owner of a typed component.
    pub(crate) fn all_typed<T: Clone + DeserializeOwned + 'static>(
        &mut self,
        key: &'static str,
    ) -> Result<Vec<(Id, T)>> {
        let owners = self.world.entities.component_owners(key);
        self.settings.insert(key, owners.first().copied());
        self.reads.extend_from_slice(owners);
        owners
            .iter()
            .map(|&id| {
                let value = self
                    .world
                    .typed_component::<T>(id, key)?
                    .context("missing component")?;
                Ok((id, value.into_owned()))
            })
            .collect()
    }

    /// Subscribe to a scene-wide raw setting, including its later addition or removal.
    pub fn first_setting(&mut self, key: &'static str) -> Option<&Value> {
        let value = self.world.first_raw_component(key);
        self.settings.insert(key, value.map(|(id, _)| id));
        value.map(|(_, value)| value)
    }

    pub fn transform(&mut self, id: Id) -> Option<Mat4> {
        let slot = self.transforms.slots.get(&id).copied();
        self.watched
            .insert(id, slot.map(|s| self.transforms.world_version(s)));
        slot.map(|s| self.transforms.world_matrix(s))
    }
}

#[derive(Clone, Default)]
pub(super) struct ComponentOutputs {
    settings: BTreeMap<&'static str, Value>,
    derived: BTreeMap<&'static str, SnapshotValue>,
}

impl RenderScene {
    pub fn setting(&self, key: &str) -> Option<&Value> {
        self.components.settings.get(key)
    }

    pub fn has_derived(&self, key: &str) -> bool {
        self.components.derived.contains_key(key)
    }

    /// Read an experiment-owned value from this immutable render snapshot.
    pub fn derived<T: Any>(&self, key: &str) -> Option<&T> {
        self.components.derived.get(key)?.downcast_ref()
    }
}

pub(super) struct ComponentOwner {
    pub id: Id,
    watched: BTreeMap<Id, Option<u64>>,
    pub reads: Vec<Id>,
    pub watched_settings: BTreeMap<&'static str, Option<Id>>,
}

impl ComponentOwner {
    pub fn new(id: Id) -> Self {
        Self {
            id,
            watched: Default::default(),
            reads: Vec::new(),
            watched_settings: Default::default(),
        }
    }

    pub fn changed(&self, transforms: &TransformCache) -> bool {
        self.watched.iter().any(|(id, version)| {
            transforms
                .slots
                .get(id)
                .map(|&s| transforms.world_version(s))
                != *version
        })
    }

    pub fn refresh(
        &mut self,
        spec: SnapshotComponent,
        world: &World,
        transforms: &TransformCache,
        outputs: &mut ComponentOutputs,
    ) -> Result<()> {
        let values = &world.entities[&self.id].components;
        for key in spec
            .related_settings
            .iter()
            .copied()
            .chain(spec.copy_setting.then_some(spec.key))
        {
            if let Some(value) = values.get(key) {
                if outputs.settings.get(key) != Some(value) {
                    outputs.settings.insert(key, value.clone());
                }
            } else {
                outputs.settings.remove(key);
            }
        }
        if let Some(extract) = spec.extract {
            self.reads.clear();
            self.watched.clear();
            self.watched_settings.clear();
            let value = extract(&mut SnapshotContext {
                owner: self.id,
                key: spec.key,
                world,
                transforms,
                watched: &mut self.watched,
                reads: &mut self.reads,
                settings: &mut self.watched_settings,
            })
            .with_context(|| format!("extract {} on entity {}", spec.key, self.id))?;
            if let Some(value) = value {
                outputs.derived.insert(spec.key, value);
            } else {
                outputs.derived.remove(spec.key);
            }
        }
        Ok(())
    }
}

impl SceneExtractor {
    /// Construct an extractor with an explicit set of experiment contributions.
    pub fn with_components(
        components: impl IntoIterator<Item = SnapshotComponent>,
    ) -> Result<Self> {
        let mut registry = BTreeMap::new();
        for entry in components {
            for key in std::iter::once(entry.key).chain(entry.related_settings.iter().copied()) {
                ensure!(
                    registry.insert(key, entry).is_none(),
                    "duplicate snapshot component key {key}"
                );
            }
        }
        Ok(Self {
            registry,
            transforms: Default::default(),
            pose_budget: None,
            focused_actor: None,
            pending_visual: false,
            palettes: Default::default(),
            previous: None,
            cursor: None,
            dependencies: Default::default(),
        })
    }
}

impl Default for SceneExtractor {
    fn default() -> Self {
        Self::with_components(experiments::snapshot_components())
            .expect("unique built-in snapshot component keys")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    const ANCHOR: &str = "test.anchor";
    const DISPLAY: &str = "test.anchor.display";
    const ENABLED: &str = "test.enabled";

    fn extract_anchor(context: &mut SnapshotContext<'_>) -> Result<Option<SnapshotValue>> {
        if context.first_setting(ENABLED).and_then(Value::as_bool) == Some(false) {
            return Ok(None);
        }
        let id = context.raw().as_u64().context("invalid anchor")?;
        Ok(context.transform(id).map(|m| Arc::new(m) as SnapshotValue))
    }

    #[test]
    fn registered_inputs_track_transform_dependencies_first_owner_and_in_flight_data() -> Result<()>
    {
        let entry = SnapshotComponent {
            related_settings: &[DISPLAY],
            extract: Some(extract_anchor),
            ..SnapshotComponent::setting(ANCHOR)
        };
        let registry = [entry, SnapshotComponent::setting(ENABLED)];
        assert!(
            SceneExtractor::with_components([entry, SnapshotComponent::setting(DISPLAY)]).is_err()
        );
        let mut extractor = SceneExtractor::with_components(registry)?;
        let mut world = World::default();
        let owner = world.spawn(Default::default());
        let parent = world.spawn(Default::default());
        let child = world.spawn(Entity {
            parent: Some(parent),
            ..Default::default()
        });
        let other = world.spawn(Default::default());
        world.set_setting(other, ANCHOR, parent.into());
        world.set_setting(other, DISPLAY, 9.into());
        let snapshot = |extractor: &mut SceneExtractor, world: &World| {
            extractor.snapshot(world, Camera::default(), [32, 32], Default::default())
        };
        let initial = snapshot(&mut extractor, &world)?;
        assert_eq!(initial.scene.setting(DISPLAY), Some(&Value::from(9)));
        world.set_setting(owner, ANCHOR, child.into());
        let active = snapshot(&mut extractor, &world)?;
        assert!(active.scene.setting(DISPLAY).is_none());
        world.set_setting(owner, DISPLAY, 3.into());
        world.set_setting(other, DISPLAY, 7.into());
        world.set_local(parent, Mat4::from_translation(Vec3::X))?;
        let moved = snapshot(&mut extractor, &world)?;
        assert_eq!(moved.scene.derived::<Mat4>(ANCHOR).unwrap().w_axis.x, 1.);
        assert_eq!(active.scene.derived::<Mat4>(ANCHOR), Some(&Mat4::IDENTITY));
        assert_eq!(moved.scene.setting(DISPLAY), Some(&Value::from(3)));
        // Rebinding to a new transform must drop the old subscription.
        world.set_setting(owner, ANCHOR, other.into());
        let rebound = snapshot(&mut extractor, &world)?;
        world.set_local(child, Mat4::from_translation(Vec3::Y))?;
        let unchanged = snapshot(&mut extractor, &world)?;
        assert!(std::ptr::eq(
            rebound.scene.derived::<Mat4>(ANCHOR).unwrap(),
            unchanged.scene.derived::<Mat4>(ANCHOR).unwrap()
        ));
        world.set_setting(other, ENABLED, false.into());
        assert!(!snapshot(&mut extractor, &world)?.scene.has_derived(ANCHOR));
        world.set_setting(other, ENABLED, true.into());
        let enabled = snapshot(&mut extractor, &world)?;
        let cold = snapshot(&mut SceneExtractor::with_components(registry)?, &world)?;
        assert_eq!(
            enabled.scene.derived::<Mat4>(ANCHOR),
            cold.scene.derived::<Mat4>(ANCHOR)
        );
        assert_eq!(initial.scene.setting(DISPLAY), Some(&Value::from(9)));
        Ok(())
    }
    #[test]
    fn repeated_simulation_component_updates_keep_unrelated_render_bindings() -> Result<()> {
        let mut world = World::demo();
        let id = world.spawn(Entity::default());
        world.set_typed_component(id, "test.physics", 1u32);
        let mut extractor = SceneExtractor::default();
        extractor.snapshot(&world, Camera::default(), [64, 64], Default::default())?;
        for i in 0..5000 {
            world.set_typed_component(id, "test.physics", i);
        }
        let frame = extractor.snapshot(&world, Camera::default(), [64, 64], Default::default())?;
        assert!(!frame.scene.stats.full_rebuild);
        assert_eq!(frame.scene.stats.instances_updated, 0);
        Ok(())
    }
}
