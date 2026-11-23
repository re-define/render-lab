mod components;
use components::{ComponentOutputs, ComponentOwner};
pub use components::{SnapshotComponent, SnapshotContext, SnapshotValue};
mod incremental;
mod pages;
mod routes;
use super::lights::{RenderLight, SceneLights};
use super::{AtlasCache, Camera, MaterialPrograms, MeshCache, Snapshot};
use crate::{experiments, scene::*};
use anyhow::{Context, Result, ensure};
use glam::Mat4;
use incremental::Dependencies;
pub(crate) use pages::InstancePages;
pub(crate) use routes::GeometryRoutes;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MeshId(pub Id);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MaterialId(pub Id);

pub struct MeshAsset {
    ordinary: bool,
    pub mesh: Arc<Mesh>,
    pub meshlets: Option<Arc<Vec<Meshlet>>>,
    pub hierarchy: Option<Arc<Vec<ClusterNode>>>,
    pub topology: Option<Arc<Vec<Edge>>>,
    pub plant: Option<Arc<PlantGeometry>>,
}

fn same_arc<T: ?Sized>(a: &Option<Arc<T>>, b: &Option<Arc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

impl MeshAsset {
    pub(super) fn same_motion_geometry(&self, previous: &Self) -> bool {
        Arc::ptr_eq(&self.mesh, &previous.mesh)
            && same_arc(&self.meshlets, &previous.meshlets)
            && same_arc(&self.hierarchy, &previous.hierarchy)
            && same_arc(&self.plant, &previous.plant)
    }

    pub fn from_entity(entity: &Entity) -> Option<Self> {
        Some(Self {
            ordinary: Self::ordinary(entity),
            mesh: entity.mesh.clone()?,
            meshlets: entity.meshlets.clone(),
            hierarchy: entity.hierarchy.clone(),
            topology: entity.topology.clone(),
            plant: entity.plant.clone(),
        })
    }

    fn ordinary(entity: &Entity) -> bool {
        entity
            .components
            .get(experiments::clusters::ORDINARY)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    fn matches(&self, mesh: &Arc<Mesh>, entity: &Entity) -> bool {
        Arc::ptr_eq(&self.mesh, mesh)
            && self.ordinary == Self::ordinary(entity)
            && same_arc(&self.meshlets, &entity.meshlets)
            && same_arc(&self.hierarchy, &entity.hierarchy)
            && same_arc(&self.topology, &entity.topology)
            && same_arc(&self.plant, &entity.plant)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometryPath {
    Ordinary,
    ClusterLod,
}

#[derive(Clone)]
pub struct RenderInstance {
    pub id: Id,
    pub mesh: MeshId,
    pub material: Option<MaterialId>,
    pub model: Mat4,
    pub outline: bool,
    pub palette: Option<Arc<[Mat4]>>,
    pub(crate) emitter: bool,
    pub(crate) grid: bool,
    skinned: bool,
    /// Revision of the resolved world matrix as seen by this extractor.
    pub transform_version: u64,
}

impl RenderInstance {
    pub(super) fn is_skinned(&self) -> bool {
        self.skinned
    }

    pub fn geometry_path(&self, asset: &MeshAsset, cluster_lod: bool) -> GeometryPath {
        if cluster_lod
            && !asset.ordinary
            && !self.skinned
            && asset.meshlets.is_some()
            && asset.hierarchy.is_some()
            && !(self.outline && asset.topology.is_some())
        {
            GeometryPath::ClusterLod
        } else {
            GeometryPath::Ordinary
        }
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct ExtractionStats {
    /// Distinct ECS entity records inspected while extracting this snapshot.
    pub entities_visited: usize,
    pub transforms_updated: usize,
    pub palettes_updated: usize,
    pub instances_updated: usize,
}

#[derive(Clone)]
pub struct RenderScene {
    pub(crate) instances: Arc<InstancePages>,
    pub(crate) routes: Arc<GeometryRoutes>,
    pub(crate) models: crate::rdg::rows::Rows,
    pub(crate) meshes: Arc<BTreeMap<MeshId, Arc<MeshAsset>>>,
    pub(crate) materials: Arc<BTreeMap<MaterialId, Arc<Material>>>,
    pub(crate) textures: Arc<BTreeMap<Id, Arc<Texture>>>,
    pub(crate) mesh_cache: Arc<MeshCache>,
    pub(crate) atlas_cache: Arc<AtlasCache>,
    pub(crate) lights: Arc<SceneLights>,
    pub stats: ExtractionStats,
    components: Arc<ComponentOutputs>,
}

impl RenderScene {
    pub fn instances(&self) -> &[Arc<RenderInstance>] {
        self.instances.flat()
    }

    pub fn meshes(&self) -> &BTreeMap<MeshId, Arc<MeshAsset>> {
        &self.meshes
    }

    pub fn materials(&self) -> &BTreeMap<MaterialId, Arc<Material>> {
        &self.materials
    }

    pub fn lights(&self) -> &[RenderLight] {
        &self.lights.entries
    }
}

pub struct SceneExtractor {
    registry: BTreeMap<&'static str, SnapshotComponent>,
    transforms: TransformCache,
    palettes: BTreeMap<Id, Palette>,
    previous: Option<Arc<RenderScene>>,
    cursor: Option<ChangeCursor>,
    dependencies: Dependencies,
}

struct Palette {
    joints: Vec<(Id, u64)>,
    inverse_bind: Vec<Mat4>,
    matrices: Arc<[Mat4]>,
}

impl SceneExtractor {
    pub fn snapshot(
        &mut self,
        world: &World,
        camera: Camera,
        extent: [u32; 2],
        materials: Arc<MaterialPrograms>,
    ) -> Result<Snapshot> {
        // Take the cursor before trying an edit. A failed extraction is reconciled in full on retry.
        let cursor = self.cursor.take();
        let edits = cursor
            .as_ref()
            .and_then(|cursor| world.entities.changes(cursor));
        let partial = match edits {
            Some(edits) => self.incremental(world, edits)?,
            None => None,
        };
        let scene = match partial {
            Some(scene) => scene,
            None => self.rebuild(world)?,
        };
        self.cursor = Some(world.entities.cursor());
        self.previous = Some(scene.clone());
        Ok(Snapshot {
            buffer_atomic64: false,
            scene,
            materials,
            camera,
            extent,
            temporal: None,
        })
    }

    fn rebuild(&mut self, world: &World) -> Result<Arc<RenderScene>> {
        let mut dependencies = Dependencies::default();
        let mut stats = ExtractionStats {
            transforms_updated: self.transforms.update(world)?,
            ..Default::default()
        };
        let mut meshes = BTreeMap::new();
        let mut render_materials = BTreeMap::new();
        let mut textures = BTreeMap::new();
        let mut active_palettes = BTreeSet::new();
        let mut lights = vec![];
        let mut has_lights = false;
        let previous_scene = self.previous.clone();
        let previous = previous_scene.as_deref();
        let mut instances = Vec::with_capacity(previous.map_or(0, |scene| scene.instances.len()));
        let mut prior_instances = previous
            .into_iter()
            .flat_map(|scene| scene.instances.iter())
            .peekable();
        for (&id, entity) in &world.entities {
            stats.entities_visited += 1;
            if let Some(parent) = entity.parent {
                dependencies.children.entry(parent).or_default().push(id);
            }
            if let Some(light) = entity.light {
                has_lights = true;
                dependencies.lights.insert(id);
                if let Some(render_light) = self.light(id, light)? {
                    lights.push(render_light);
                }
            }
            if let Some(mesh) = &entity.mesh {
                let key = MeshId(id);
                let existing = previous
                    .and_then(|scene| scene.meshes.get(&key))
                    .filter(|asset| asset.matches(mesh, entity));
                meshes.insert(
                    key,
                    existing
                        .cloned()
                        .unwrap_or_else(|| Arc::new(MeshAsset::from_entity(entity).unwrap())),
                );
            }
            if let Some(material) = &entity.material {
                let key = MaterialId(id);
                let prior = previous
                    .and_then(|scene| scene.materials.get(&key))
                    .filter(|old| old.as_ref() == material);
                render_materials.insert(
                    key,
                    prior.cloned().unwrap_or_else(|| Arc::new(material.clone())),
                );
            }
            if let Some(texture) = &entity.texture {
                textures.insert(id, texture.clone());
            }
            for key in entity.components.keys() {
                if let Some(spec) = self.registry.get(key.as_str())
                    && spec.key == key
                {
                    dependencies
                        .components
                        .entry(spec.key)
                        .or_insert_with(|| ComponentOwner::new(id));
                }
            }
            let Some(instance) = entity.instance.as_ref().filter(|_| {
                !entity
                    .components
                    .contains_key(crate::experiments::composition::HIDDEN)
            }) else {
                continue;
            };
            let mesh = world
                .entities
                .get(&instance.mesh)
                .and_then(|entity| entity.mesh.as_ref())
                .context("missing render instance mesh")?;
            if let Some(id) = instance.material {
                ensure!(
                    world
                        .entities
                        .get(&id)
                        .is_some_and(|entity| entity.material.is_some()),
                    "missing render instance material {id}"
                );
            }
            let palette =
                if let Some(skin) = entity.skin_binding.filter(|_| !mesh.weights.is_empty()) {
                    if active_palettes.insert(skin) {
                        stats.palettes_updated += usize::from(self.update_palette(world, skin)?);
                        for &(joint, _) in &self.palettes[&skin].joints {
                            dependencies
                                .joint_skins
                                .entry(joint)
                                .or_default()
                                .push(skin);
                        }
                    }
                    dependencies.skin_users.entry(skin).or_default().push(id);
                    Some(self.palettes[&skin].matrices.clone())
                } else {
                    None
                };
            let transform = &self.transforms.entries[&id];
            let next = RenderInstance {
                id,
                mesh: MeshId(instance.mesh),
                material: instance.material.map(MaterialId),
                model: transform.world,
                outline: instance.outline,
                palette,
                emitter: entity
                    .components
                    .contains_key(experiments::lighting::EMITTER),
                grid: entity.components.contains_key(experiments::lighting::GRID),
                skinned: entity.skin_binding.is_some(),
                transform_version: transform.version,
            };
            while prior_instances.peek().is_some_and(|prior| prior.id < id) {
                prior_instances.next();
            }
            let match_prior = prior_instances
                .next_if(|prior| prior.id == id)
                .filter(|prior| {
                    prior.mesh == next.mesh
                        && prior.material == next.material
                        && prior.transform_version == next.transform_version
                        && prior.outline == next.outline
                        && prior.emitter == next.emitter
                        && prior.grid == next.grid
                        && prior.skinned == next.skinned
                        && same_arc(&prior.palette, &next.palette)
                });
            instances.push(match_prior.cloned().unwrap_or_else(|| {
                stats.instances_updated += 1;
                Arc::new(next)
            }));
        }
        self.palettes.retain(|id, _| active_palettes.contains(id));
        let mut components = ComponentOutputs::default();
        for (key, owner) in &mut dependencies.components {
            owner.refresh(self.registry[key], world, &self.transforms, &mut components)?;
        }
        let routes = Arc::new(GeometryRoutes::new(
            &instances,
            &meshes,
            &render_materials,
            previous.map(|p| p.routes.as_ref()),
        )?);
        let mut model_records =
            vec![[0; 20]; routes.model_slots.values().max().map_or(0, |n| n + 1)];
        for i in &instances {
            model_records[routes.model_slots[&i.id]] = i.gpu_model();
        }
        let models = crate::rdg::rows::Rows::new(&model_records);
        let scene = Arc::new(RenderScene {
            routes,
            models,
            meshes: Arc::new(meshes),
            materials: Arc::new(render_materials),
            textures: Arc::new(textures),
            instances: Arc::new(InstancePages::new(instances)),
            lights: previous
                .filter(|s| s.lights.present == has_lights && s.lights.entries == lights)
                .map(|s| s.lights.clone())
                .unwrap_or_else(|| Arc::new(SceneLights::new(has_lights, lights))),
            components: Arc::new(components),
            stats,
            mesh_cache: previous
                .map(|scene| scene.mesh_cache.clone())
                .unwrap_or_default(),
            atlas_cache: previous
                .map(|scene| scene.atlas_cache.clone())
                .unwrap_or_default(),
        });
        self.dependencies = dependencies;
        Ok(scene)
    }

    fn light(&self, id: Id, light: Light) -> Result<Option<RenderLight>> {
        let world = self.transforms.entries[&id].world;
        light
            .validate(world)
            .with_context(|| format!("light entity {id}"))?;
        Ok(
            (light.enabled && light.intensity > 0. && light.color.iter().any(|&c| c > 0.))
                .then(|| RenderLight::extract(id, light, world)),
        )
    }

    fn update_palette(&mut self, world: &World, id: Id) -> Result<bool> {
        let skin = world
            .entities
            .get(&id)
            .and_then(|entity| entity.skin.as_ref())
            .context("missing skin binding")?;
        ensure!(
            skin.joints.len() == skin.inverse_bind.len(),
            "invalid skin palette"
        );
        let joints = skin
            .joints
            .iter()
            .map(|&joint| {
                let entry = self
                    .transforms
                    .entries
                    .get(&joint)
                    .context("missing skin joint")?;
                Ok((joint, entry.version))
            })
            .collect::<Result<Vec<_>>>()?;
        if self
            .palettes
            .get(&id)
            .is_some_and(|p| p.joints == joints && p.inverse_bind == skin.inverse_bind)
        {
            return Ok(false);
        }
        let matrices = joints
            .iter()
            .zip(&skin.inverse_bind)
            .map(|(&(id, _), &inverse)| self.transforms.entries[&id].world * inverse)
            .collect();
        self.palettes.insert(
            id,
            Palette {
                joints,
                inverse_bind: skin.inverse_bind.clone(),
                matrices,
            },
        );
        Ok(true)
    }
}

#[derive(Default)]
struct TransformCache {
    entries: BTreeMap<Id, Transform>,
    order: Vec<Id>,
    revision: u64,
}

struct Transform {
    parent: Option<Id>,
    parent_version: u64,
    local: Mat4,
    world: Mat4,
    version: u64,
}

impl TransformCache {
    fn update(&mut self, world: &World) -> Result<usize> {
        let entities = &world.entities;
        if self.entries.len() != entities.len()
            || !self
                .entries
                .iter()
                .zip(entities)
                .all(|((&old, t), (&id, e))| old == id && t.parent == e.parent)
        {
            let order = world.transform_order()?;
            self.entries.retain(|id, _| entities.contains_key(id));
            self.order = order;
        }
        let mut updated = 0;
        for i in 0..self.order.len() {
            let id = self.order[i];
            updated += usize::from(self.update_one(id, &entities[&id])?);
        }
        Ok(updated)
    }

    fn update_one(&mut self, id: Id, entity: &Entity) -> Result<bool> {
        let (parent_world, parent_version) = entity.parent.map_or((Mat4::IDENTITY, 0), |parent| {
            let parent = &self.entries[&parent];
            (parent.world, parent.version)
        });
        if self.entries.get(&id).is_some_and(|entry| {
            entry.parent == entity.parent
                && entry.parent_version == parent_version
                && entry.local == entity.local
        }) {
            return Ok(false);
        }
        self.revision = self
            .revision
            .checked_add(1)
            .context("transform versions exhausted")?;
        self.entries.insert(
            id,
            Transform {
                local: entity.local,
                parent: entity.parent,
                parent_version,
                world: parent_world * entity.local,
                version: self.revision,
            },
        );
        Ok(true)
    }
}
