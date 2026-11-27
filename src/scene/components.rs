use super::*;
use serde::{Serializer, de::DeserializeOwned};
use std::{any::Any, borrow::Cow, collections::HashMap};

pub(crate) trait Stored: Send + Sync {
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
    values: HashMap<Id, BTreeMap<&'static str, Arc<dyn Stored>>>,
    binding_edits: BTreeMap<&'static str, u64>,
}
impl RuntimeComponents {
    pub(super) fn binding_changed(&mut self, key: &'static str) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        self.binding_edits
            .insert(key, NEXT.fetch_add(1, Ordering::Relaxed));
    }
    fn get(&self, id: Id, key: &str) -> Option<&Arc<dyn Stored>> {
        self.values.get(&id)?.get(key)
    }
    fn insert(&mut self, id: Id, key: &'static str, value: Arc<dyn Stored>) {
        self.values.entry(id).or_default().insert(key, value);
    }
    pub(super) fn remove_component(&mut self, id: Id, key: &'static str) {
        if let Some(row) = self.values.get_mut(&id) {
            row.remove(key);
            if row.is_empty() {
                self.values.remove(&id);
            }
        }
    }
    pub(super) fn remove_entity(&mut self, entity: Id) {
        self.values.remove(&entity);
    }
}

impl World {
    pub(crate) fn restore_component_on_rollback(
        &self,
        id: Id,
        key: &'static str,
    ) -> Option<impl FnOnce(&mut Self) + Send + use<>> {
        let previous = self.entities.get(&id)?.components.get(key).cloned();
        let typed = self.runtime.get(id, key).cloned();
        Some(move |world: &mut Self| {
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
                    world.runtime.insert(id, key, value);
                }
                None => world.runtime.remove_component(id, key),
            }
        })
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
        if let Some(value) = self.runtime.get(id, key) {
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
        assert!(
            self.set_stored_if_present(id, key, Arc::new(value), true),
            "missing component entity"
        );
    }
    pub(crate) fn remove_typed_component(&mut self, id: Id, key: &'static str) {
        self.entities.remove_extension(id, key);
        self.runtime.remove_component(id, key);
        self.runtime.binding_changed(key);
    }
    pub(crate) fn binding_version(&self, key: &'static str) -> u64 {
        self.runtime.binding_edits.get(key).copied().unwrap_or(0)
    }
    /// Dynamic writes preserve the binding version; introducing a key still invalidates membership.
    pub(crate) fn set_stored_if_present(
        &mut self,
        id: Id,
        key: &'static str,
        value: Arc<dyn Stored>,
        changes_binding: bool,
    ) -> bool {
        if !self.entities.mark_typed_value(id, key) {
            return false;
        }
        if changes_binding {
            self.runtime.binding_changed(key);
        }
        self.runtime.insert(id, key, value);
        true
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
            .filter(|id| self.runtime.get(*id, key).is_none())
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
            #[serde(skip_serializing_if = "Option::is_none")]
            control_target: Option<Option<Id>>,
            entities: &'a BTreeMap<Id, Entity>,
        }
        let owned;
        let entities: &BTreeMap<Id, Entity> = if self.runtime.values.is_empty() {
            &self.entities
        } else {
            owned = {
                let mut entities = BTreeMap::clone(&self.entities);
                for (id, fields) in &self.runtime.values {
                    let Some(entity) = entities.get_mut(id) else {
                        continue;
                    };
                    for (&key, value) in fields {
                        if let Some(raw) = entity.components.get_mut(key) {
                            *raw = value.to_json().map_err(serde::ser::Error::custom)?;
                        }
                    }
                }
                entities
            };
            &owned
        };
        Document {
            version: self.version,
            clocks,
            control_target: self.control_selection(),
            entities,
        }
        .serialize(serializer)
    }
}
