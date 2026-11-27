use super::*;
use serde::{Serializer, de::DeserializeOwned};
use std::{any::Any, borrow::Cow};

trait Stored: Send + Sync {
    fn value(&self) -> &dyn Any;
    fn to_json(&self) -> serde_json::Result<Value>;
}

impl<T: Serialize + Send + Sync + 'static> Stored for T {
    fn value(&self) -> &dyn Any {
        self
    }
    fn to_json(&self) -> serde_json::Result<Value> {
        serde_json::to_value(self)
    }
}

#[derive(Clone, Default)]
pub(super) struct RuntimeComponents {
    values: BTreeMap<(Id, &'static str), Arc<dyn Stored>>,
    binding_edits: BTreeMap<&'static str, u64>,
}
impl RuntimeComponents {
    pub(super) fn binding_changed(&mut self, key: &'static str) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        self.binding_edits
            .insert(key, NEXT.fetch_add(1, Ordering::Relaxed));
    }
    pub(super) fn remove_component(&mut self, id: Id, key: &'static str) {
        self.values.remove(&(id, key));
    }
    pub(super) fn remove_entity(&mut self, entity: Id) {
        // Removing the highest ID can make the next spawn reuse it. Clear only that ID's values.
        let from = std::ops::Bound::Included((entity, ""));
        let to = entity
            .checked_add(1)
            .map_or(std::ops::Bound::Unbounded, |id| {
                std::ops::Bound::Excluded((id, ""))
            });
        let keys: Vec<_> = self.values.range((from, to)).map(|(&key, _)| key).collect();
        for key in keys {
            self.values.remove(&key);
        }
    }
}

impl World {
    pub(crate) fn restore_component_on_rollback(
        &self,
        id: Id,
        key: &'static str,
    ) -> impl FnOnce(&mut Self) + Send + use<> {
        let previous = self.entities[&id].components.get(key).cloned();
        let typed = self.runtime.values.get(&(id, key)).cloned();
        move |world| {
            let row = world
                .entities
                .get_mut(&id)
                .expect("component rollback entity");
            match previous {
                Some(value) => {
                    row.components.insert(key.into(), value);
                }
                None => {
                    row.components.remove(key);
                }
            }
            match typed {
                Some(value) => {
                    world.runtime.values.insert((id, key), value);
                }
                None => world.runtime.remove_component(id, key),
            }
        }
    }
    /// First entity (in stable ID order) carrying this serialized extension key.
    /// Typed runtime components may store a JSON null marker here.
    pub fn first_raw_component(&self, key: &str) -> Option<(Id, &Value)> {
        let &id = self.entities.component_owners(key).first()?;
        Some((id, &self.entities[&id].components[key]))
    }

    pub(crate) fn typed_component<T: Clone + DeserializeOwned + 'static>(
        &self,
        id: Id,
        key: &'static str,
    ) -> Result<Option<Cow<'_, T>>> {
        let Some(raw) = self.entities.get(&id).and_then(|e| e.components.get(key)) else {
            return Ok(None);
        };
        self.component_value(id, key, raw).map(Some)
    }

    pub(super) fn component_value<T: Clone + DeserializeOwned + 'static>(
        &self,
        id: Id,
        key: &'static str,
        raw: &Value,
    ) -> Result<Cow<'_, T>> {
        if let Some(value) = self.runtime.values.get(&(id, key)) {
            return Ok(Cow::Borrowed(
                value
                    .value()
                    .downcast_ref::<T>()
                    .context("component type mismatch")?,
            ));
        }
        Ok(Cow::Owned(serde_json::from_value(raw.clone())?))
    }

    pub(crate) fn set_typed_component<T: Serialize + Send + Sync + 'static>(
        &mut self,
        id: Id,
        key: &'static str,
        value: T,
    ) {
        self.runtime.binding_changed(key);
        self.set_bound_state(id, key, value);
    }
    pub(crate) fn binding_version(&self, key: &'static str) -> u64 {
        self.runtime.binding_edits.get(key).copied().unwrap_or(0)
    }
    /// Runtime state produced from an existing binding. New membership still invalidates the owner index.
    pub(crate) fn set_bound_state<T: Serialize + Send + Sync + 'static>(
        &mut self,
        id: Id,
        key: &'static str,
        value: T,
    ) {
        self.mark_component(id, key)
            .components
            .entry(key.into())
            .or_insert(Value::Null);
        self.runtime.values.insert((id, key), Arc::new(value));
    }

    pub(crate) fn hydrate_components<
        T: Clone + DeserializeOwned + Serialize + Send + Sync + 'static,
    >(
        &mut self,
        key: &'static str,
    ) -> Result<()> {
        let decoded = self
            .entities
            .component_owners(key)
            .iter()
            .copied()
            .filter(|id| !self.runtime.values.contains_key(&(*id, key)))
            .map(|id| {
                serde_json::from_value::<T>(self.entities[&id].components[key].clone())
                    .map(|value| (id, value))
            })
            .collect::<serde_json::Result<Vec<_>>>()?;
        for (id, value) in decoded {
            self.set_typed_component(id, key, value);
        }
        Ok(())
    }
}

impl Serialize for World {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.serialize_with_clocks(&self.clocks, serializer)
    }
}
impl World {
    pub(super) fn serialize_with_clocks<S: Serializer>(
        &self,
        clocks: &BTreeMap<String, super::ClockCheckpoint>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Document<'a> {
            version: u32,
            #[serde(skip_serializing_if = "BTreeMap::is_empty")]
            clocks: &'a BTreeMap<String, super::ClockCheckpoint>,
            entities: &'a BTreeMap<Id, Entity>,
        }
        let owned;
        let entities: &BTreeMap<Id, Entity> = if self.runtime.values.is_empty() {
            &self.entities
        } else {
            owned = {
                let mut entities = BTreeMap::clone(&self.entities);
                for (&(id, key), value) in &self.runtime.values {
                    if let Some(raw) = entities
                        .get_mut(&id)
                        .and_then(|e| e.components.get_mut(key))
                    {
                        *raw = value.to_json().map_err(serde::ser::Error::custom)?;
                    }
                }
                entities
            };
            &owned
        };
        Document {
            version: self.version,
            clocks,
            entities,
        }
        .serialize(serializer)
    }
}
