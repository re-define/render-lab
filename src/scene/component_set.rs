//! Each key has one authoritative representation. JSON inspection of typed values is lazy.
use super::*;
use serde::{Serializer, ser::SerializeMap};
use std::{borrow::Cow, ops::Index, sync::OnceLock};

#[derive(Default, Clone, Deserialize)]
#[serde(from = "BTreeMap<String, Value>")]
pub struct ComponentSet(BTreeMap<String, Field>);

pub(crate) enum Field {
    Json(Value),
    Typed(Arc<dyn Stored>, OnceLock<Result<Box<Value>, String>>),
}

impl Clone for Field {
    fn clone(&self) -> Self {
        match self {
            Self::Json(v) => Self::Json(v.clone()),
            Self::Typed(value, _) => Self::typed(value.clone()),
        }
    }
}

impl Field {
    pub(crate) fn typed(v: Arc<dyn Stored>) -> Self {
        Self::Typed(v, OnceLock::new())
    }
    fn json(&self) -> Result<&Value> {
        match self {
            Self::Json(v) => Ok(v),
            Self::Typed(v, cache) => cache
                .get_or_init(|| v.to_json().map(Box::new).map_err(|e| e.to_string()))
                .as_deref()
                .map_err(|e| anyhow::anyhow!("{e}")),
        }
    }
}

impl Serialize for ComponentSet {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (k, field) in &self.0 {
            match field {
                Field::Json(v) => map.serialize_entry(k, v)?,
                Field::Typed(v, _) => {
                    let value = v.to_json().map_err(serde::ser::Error::custom)?;
                    map.serialize_entry(k, &value)?;
                }
            }
        }
        map.end()
    }
}
impl From<BTreeMap<String, Value>> for ComponentSet {
    fn from(values: BTreeMap<String, Value>) -> Self {
        Self(
            values
                .into_iter()
                .map(|(k, v)| (k, Field::Json(v)))
                .collect(),
        )
    }
}
impl<const N: usize> From<[(String, Value); N]> for ComponentSet {
    fn from(values: [(String, Value); N]) -> Self {
        BTreeMap::from(values).into()
    }
}
impl Index<&str> for ComponentSet {
    type Output = Value;
    fn index(&self, key: &str) -> &Value {
        self.try_get(key)
            .expect("component JSON projection")
            .expect("component key")
    }
}
impl ComponentSet {
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.0.keys()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn contains_key(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }
    pub fn try_get(&self, key: &str) -> Result<Option<&Value>> {
        self.0.get(key).map(Field::json).transpose()
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.try_get(key).ok().flatten()
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        let field = self.0.get_mut(key)?;
        if let Field::Typed(v, _) = field {
            *field = Field::Json(v.to_json().ok()?);
        }
        match field {
            Field::Json(v) => Some(v),
            _ => unreachable!(),
        }
    }
    pub fn insert(&mut self, key: String, value: Value) {
        self.0.insert(key, Field::Json(value));
    }
    pub fn remove(&mut self, key: &str) {
        self.0.remove(key);
    }
    pub(super) fn read<T: Clone + serde::de::DeserializeOwned + 'static>(
        &self,
        key: &str,
    ) -> Result<Option<Cow<'_, T>>> {
        self.0
            .get(key)
            .map(|f| match f {
                Field::Typed(v, _) => v
                    .value()
                    .downcast_ref()
                    .map(Cow::Borrowed)
                    .context("component type mismatch"),
                Field::Json(v) => Ok(Cow::Owned(serde_json::from_value(v.clone())?)),
            })
            .transpose()
    }
    pub(super) fn is_typed(&self, key: &str) -> bool {
        matches!(self.0.get(key), Some(Field::Typed(..)))
    }
    pub(super) fn replace(&mut self, key: &'static str, value: Field) -> Option<Field> {
        if let Some(old) = self.0.get_mut(key) {
            return Some(std::mem::replace(old, value));
        }
        self.0.insert(key.to_owned(), value);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_edits_and_archive_see_same_typed_row() -> Result<()> {
        let mut w = World::default();
        let id = w.spawn(Entity::default());
        w.set_typed_component(id, "test.value", vec![42u32]);
        assert_eq!(
            w.first_raw_component("test.value").unwrap().1,
            &serde_json::json!([42])
        );
        let c = &mut w.entities.get_mut(&id).unwrap().components;
        c.get_mut("test.value").unwrap()[0] = 7.into();
        c.insert("plugin.unregistered".into(), serde_json::json!({"x":3}));
        assert_eq!(
            w.typed_component::<Vec<u32>>(id, "test.value")?.unwrap()[0],
            7
        );
        let path = tempfile::NamedTempFile::new()?.into_temp_path();
        w.save(&path)?;
        let copy = World::load(&path)?;
        assert_eq!(
            copy.first_raw_component("plugin.unregistered").unwrap().1["x"],
            3
        );
        w.entities.remove(&id);
        w.entities.insert(id, Entity::default());
        assert!(w.typed_component::<Vec<u32>>(id, "test.value")?.is_none());
        assert!(
            w.set_local(id, Mat4::from_scale(Vec3::splat(f32::NAN)))
                .is_err()
        );
        assert_eq!(w.entities[&id].local, Mat4::IDENTITY);
        Ok(())
    }
}
