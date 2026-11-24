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
pub(super) struct RuntimeComponents(BTreeMap<(Id, &'static str), Arc<dyn Stored>>);
impl RuntimeComponents {
    pub(super) fn remove_component(&mut self, id: Id, key: &'static str) {
        self.0.remove(&(id, key));
    }
    pub(super) fn remove_entity(&mut self, entity: Id) {
        self.0.retain(|(id, _), _| *id != entity);
    }
}

impl World {
    pub(crate) fn restore_component_on_rollback(
        &self,
        id: Id,
        key: &'static str,
    ) -> impl FnOnce(&mut Self) + Send + use<> {
        let previous = self.entities[&id].components.get(key).cloned();
        let typed = self.runtime.0.get(&(id, key)).cloned();
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
                    world.runtime.0.insert((id, key), value);
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
        if let Some(value) = self.runtime.0.get(&(id, key)) {
            return Ok(Some(Cow::Borrowed(
                value
                    .value()
                    .downcast_ref::<T>()
                    .context("component type mismatch")?,
            )));
        }
        Ok(Some(Cow::Owned(serde_json::from_value(raw.clone())?)))
    }

    pub(crate) fn set_typed_component<T: Serialize + Send + Sync + 'static>(
        &mut self,
        id: Id,
        key: &'static str,
        value: T,
    ) {
        self.mark_component(id, key)
            .components
            .entry(key.into())
            .or_insert(Value::Null);
        self.runtime.0.insert((id, key), Arc::new(value));
    }

    pub(crate) fn hydrate_components<
        T: Clone + DeserializeOwned + Serialize + Send + Sync + 'static,
    >(
        &mut self,
        key: &'static str,
    ) -> Result<()> {
        let decoded = self
            .entities
            .iter()
            .filter_map(|(&id, entity)| {
                if self.runtime.0.contains_key(&(id, key)) {
                    return None;
                }
                entity
                    .components
                    .get(key)
                    .map(|raw| serde_json::from_value::<T>(raw.clone()).map(|v| (id, v)))
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
        #[derive(Serialize)]
        struct Document<'a> {
            version: u32,
            entities: &'a BTreeMap<Id, Entity>,
        }
        let owned;
        let entities: &BTreeMap<Id, Entity> = if self.runtime.0.is_empty() {
            &self.entities
        } else {
            owned = {
                let mut entities = BTreeMap::clone(&self.entities);
                for (&(id, key), value) in &self.runtime.0 {
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
            entities,
        }
        .serialize(serializer)
    }
}
