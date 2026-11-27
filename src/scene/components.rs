use super::*;
use serde::{Serializer, de::DeserializeOwned};
use std::{any::Any, borrow::Cow};

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

impl World {
    pub(crate) fn binding_changed(&mut self, key: &'static str) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        self.binding_edits
            .insert(key, NEXT.fetch_add(1, Ordering::Relaxed));
    }
    /// First entity (in stable ID order) carrying this serialized extension key.
    pub fn first_raw_component(&self, key: &str) -> Option<(Id, &Value)> {
        let &id = self.entities.component_owners(key).first()?;
        Some((id, &self.entities[&id].components[key]))
    }

    pub(crate) fn typed_component<T: Clone + DeserializeOwned + 'static>(
        &self,
        id: Id,
        key: &'static str,
    ) -> Result<Option<Cow<'_, T>>> {
        match self.entities.get(&id) {
            Some(row) => row.components.read(key),
            None => Ok(None),
        }
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
        self.binding_changed(key);
    }
    pub(crate) fn binding_version(&self, key: &'static str) -> u64 {
        self.binding_edits.get(key).copied().unwrap_or(0)
    }
    /// Dynamic writes preserve the binding version; introducing a key still invalidates membership.
    pub(crate) fn set_stored_if_present(
        &mut self,
        id: Id,
        key: &'static str,
        value: Arc<dyn Stored>,
        changes_binding: bool,
    ) -> bool {
        if self
            .entities
            .replace_field(id, key, component_set::Field::typed(value))
            .is_none()
        {
            return false;
        }
        if changes_binding {
            self.binding_changed(key);
        }
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
            .filter(|id| !self.entities[id].components.is_typed(key))
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
        self.serialize_with_layout(&self.clocks, self.partition.as_deref(), serializer)
    }
}
impl World {
    pub(super) fn serialize_with_layout<S: Serializer>(
        &self,
        clocks: &BTreeMap<String, super::ClockCheckpoint>,
        partition: Option<&super::partition::Layout>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Document<'a> {
            version: u32,
            time_seconds: f64,
            #[serde(skip_serializing_if = "BTreeMap::is_empty")]
            clocks: &'a BTreeMap<String, super::ClockCheckpoint>,
            #[serde(skip_serializing_if = "Option::is_none")]
            control_target: Option<Option<Id>>,
            #[serde(skip_serializing_if = "Option::is_none")]
            partition: Option<&'a super::partition::Layout>,
            entities: &'a BTreeMap<Id, Entity>,
        }
        Document {
            version: self.version,
            time_seconds: self.time_seconds,
            clocks,
            control_target: self.control_selection(),
            entities: &self.entities,
            partition,
        }
        .serialize(serializer)
    }
}
