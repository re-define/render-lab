mod components;
use components::{ComponentOutputs, ComponentOwner};
pub use components::{SnapshotComponent, SnapshotContext, SnapshotValue};
mod incremental;
mod leaf_edits;
mod pose_space;
use pose_space::PoseSpace;
mod transforms;
use transforms::TransformCache;
mod pages;
mod routes;
use super::lights::{RenderLight, SceneLights};
use super::{AtlasCache, Camera, MaterialPrograms, MeshCache, Snapshot};
use crate::{experiments, scene::*};
use anyhow::{Context, Result, ensure};
use glam::Mat4;
use incremental::{Dependencies, SkinDependency};
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
    draw_template: std::sync::OnceLock<super::draw_list::MeshTemplate>,
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
    fn clustered(&self, outline: bool) -> bool {
        !self.ordinary
            && self.meshlets.is_some()
            && self.hierarchy.is_some()
            && !(outline && self.topology.is_some())
    }
    pub(super) fn ordinary_draw_template(
        &self,
        bounds: Option<LocalBounds>,
    ) -> super::draw_list::MeshTemplate {
        match bounds {
            Some(bounds) => {
                super::draw_list::MeshTemplate::authored(self.mesh.positions.len(), bounds)
            }
            None => *self
                .draw_template
                .get_or_init(|| super::draw_list::MeshTemplate::new(&self.mesh)),
        }
    }

    pub(super) fn same_motion_geometry(&self, previous: &Self) -> bool {
        Arc::ptr_eq(&self.mesh, &previous.mesh)
            && same_arc(&self.meshlets, &previous.meshlets)
            && same_arc(&self.hierarchy, &previous.hierarchy)
            && same_arc(&self.plant, &previous.plant)
    }

    pub fn from_entity(entity: &Entity) -> Option<Self> {
        let mesh = entity.mesh.clone()?;
        Some(Self {
            ordinary: Self::ordinary(entity),
            draw_template: Default::default(),
            mesh,
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
    pub geometry_assets: Option<Id>,
    pub outline: bool,
    /// Weighted matrices are expressed in palette_root space; unweighted vertices use model.
    pub palette: Option<Arc<[Mat4]>>,
    pub palette_root: Mat4,
    pub(crate) emitter: bool,
    pub(crate) grid: bool,
    skinned: bool,
    /// Revision of the resolved world matrix as seen by this extractor.
    pub transform_version: u64,
    pub(super) incarnation: u64,
}

impl RenderInstance {
    fn from_entity(
        entity: &Entity,
        transform: &transforms::Transform,
        incarnation: u64,
        palette: Option<Arc<[Mat4]>>,
        palette_root: Mat4,
    ) -> Self {
        let instance = entity.instance.as_ref().expect("instance entity");
        Self {
            geometry_assets: instance.geometry_assets,
            id: transform.id,
            incarnation,
            mesh: MeshId(instance.mesh),
            material: instance.material.map(MaterialId),
            model: transform.world,
            outline: instance.outline,
            palette,
            palette_root,
            emitter: entity
                .components
                .contains_key(experiments::lighting::EMITTER),
            grid: entity.components.contains_key(experiments::lighting::GRID),
            skinned: entity.skin_binding.is_some(),
            transform_version: transform.version,
        }
    }
    pub(super) fn is_skinned(&self) -> bool {
        self.skinned
    }

    pub fn geometry_path(&self, asset: &MeshAsset, cluster_lod: bool) -> GeometryPath {
        if cluster_lod && !self.skinned && asset.clustered(self.outline) {
            GeometryPath::ClusterLod
        } else {
            GeometryPath::Ordinary
        }
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct ExtractionStats {
    pub full_rebuild: bool,
    /// Distinct ECS entity records inspected while extracting this snapshot.
    pub entities_visited: usize,
    pub transforms_updated: usize,
    pub palettes_updated: usize,
    /// Per-actor pose records consumed from the journal since this reader's last snapshot.
    pub pose_blocks_read: usize,
    pub pose_locals_read: usize,
    /// Palette entries actually inspected, including a dense scan when most joints changed.
    pub palette_joints_examined: usize,
    /// Distinct cosmetic actors retaining their last published presentation pose.
    pub held_visual_actors: usize,
    /// Live instance records constructed or changed; deleting a draw retires its slot.
    pub instances_updated: usize,
}

#[derive(Clone)]
pub struct RenderScene {
    pub(crate) instances: Arc<InstancePages>,
    pub(crate) routes: Arc<GeometryRoutes>,
    pub(crate) models: crate::rdg::rows::Rows,
    pub(crate) meshes: Arc<BTreeMap<MeshId, Arc<MeshAsset>>>,
    geometry_assets: Arc<BTreeMap<Id, Arc<GeometryAssets>>>,
    pub(crate) geometry_revision: u64,
    pub(crate) materials: Arc<BTreeMap<MaterialId, Arc<Material>>>,
    pub(crate) textures: Arc<BTreeMap<Id, Arc<Texture>>>,
    pub(crate) mesh_cache: Arc<MeshCache>,
    pub(crate) atlas_cache: Arc<AtlasCache>,
    pub(crate) lights: Arc<SceneLights>,
    pub stats: ExtractionStats,
    components: Arc<ComponentOutputs>,
}

fn next_geometry_revision() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static REVISION: AtomicU64 = AtomicU64::new(1);
    REVISION.fetch_add(1, Ordering::Relaxed)
}

impl RenderScene {
    pub fn products_for(&self, instance: &RenderInstance) -> Option<&GeometryAssets> {
        self.geometry_assets
            .get(&instance.geometry_assets?)
            .map(AsRef::as_ref)
    }
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
    pose_tasks: Option<Arc<rayon::ThreadPool>>,
    registry: BTreeMap<&'static str, SnapshotComponent>,
    transforms: TransformCache,
    palettes: BTreeMap<Id, Palette>,
    pose_budget: Option<pose_space::VisualBudget>,
    pub(crate) focused_actor: Option<Id>,
    pending_visual: bool,
    previous: Option<Arc<RenderScene>>,
    cursor: Option<ChangeCursor>,
    dependencies: Dependencies,
}

struct Palette {
    dependency: usize,
    space: Option<PoseSpace>,
    slots: Vec<usize>,
    joints: Vec<(Id, u64)>,
    inverse_bind: Vec<Mat4>,
    matrices: Arc<[Mat4]>,
}

impl SceneExtractor {
    pub fn with_pose_workers(mut self, pool: Arc<rayon::ThreadPool>) -> Self {
        self.pose_tasks = Some(pool);
        self
    }
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
            Some(edits) => self.incremental(world, edits.writes(), edits.poses())?,
            None => None,
        };
        let scene = match partial {
            Some(scene) => scene,
            None => self.rebuild(world)?,
        };
        self.cursor = Some(world.entities.cursor());
        self.previous = Some(scene.clone());
        Ok(Snapshot {
            secondary_view: false,
            buffer_atomic64: false,
            stream_adapter: None,
            reference_draws: std::env::var_os("RENDER_LAB_REFERENCE_DRAWS").is_some(),
            ray_caps: Default::default(),
            scene,
            materials,
            camera,
            extent,
            temporal: None,
            previous_deformation: None,
        })
    }

    fn rebuild(&mut self, world: &World) -> Result<Arc<RenderScene>> {
        let mut dependencies = Dependencies::default();
        let mut stats = ExtractionStats {
            full_rebuild: true,
            transforms_updated: self.transforms.update(world)?,
            ..Default::default()
        };
        let mut meshes = BTreeMap::new();
        let mut geometry_assets = BTreeMap::new();
        let mut render_materials = BTreeMap::new();
        let mut textures = BTreeMap::new();
        let mut skin_indices = BTreeMap::new();
        let mut lights = vec![];
        let mut light_count = 0;
        let previous_scene = self.previous.clone();
        let previous = previous_scene.as_deref();
        let mut instances = Vec::with_capacity(previous.map_or(0, |scene| scene.instances.len()));
        let mut prior_instances = previous
            .into_iter()
            .flat_map(|scene| scene.instances.iter())
            .peekable();
        for (&id, entity) in &world.entities {
            stats.entities_visited += 1;
            if let Some(light) = entity.light {
                light_count += 1;
                self.transforms.row_mut(id).light = true;
                if let Some(render_light) = self.light(id, light)? {
                    lights.push(render_light);
                }
            }
            if let Some(products) = &entity.geometry_assets {
                products.validate(world)?;
                geometry_assets.insert(id, products.clone());
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
            let (palette, palette_root) =
                if let Some(skin) = entity.skin_binding.filter(|_| !mesh.weights.is_empty()) {
                    let index = if let Some(&index) = skin_indices.get(&skin) {
                        index
                    } else {
                        stats.palettes_updated += usize::from(self.update_palette(world, skin)?);
                        let palette = self.palettes.get_mut(&skin).unwrap();
                        palette.dependency = dependencies.skins.len();
                        stats.palette_joints_examined += palette.slots.len();
                        let index = dependencies.skins.len();
                        if let Some(space) = &palette.space {
                            for (node, row) in space.rows().enumerate() {
                                dependencies
                                    .pose_nodes
                                    .entry(row)
                                    .or_default()
                                    .add((index, node));
                            }
                        } else {
                            for (joint, &slot) in palette.slots.iter().enumerate() {
                                self.transforms.rows[slot].skins.add((index, joint));
                            }
                        }
                        dependencies.skins.push(SkinDependency {
                            id: skin,
                            joint_count: palette.slots.len(),
                            instances: Vec::new(),
                        });
                        skin_indices.insert(skin, index);
                        index
                    };
                    dependencies.skins[index].instances.push(instances.len());
                    let palette = &self.palettes[&skin];
                    (
                        Some(palette.matrices.clone()),
                        palette.space.as_ref().map_or(Mat4::IDENTITY, |space| {
                            self.transforms.rows[space.root].world
                        }),
                    )
                } else {
                    (None, Mat4::IDENTITY)
                };
            let transform = self.transforms.row_mut(id);
            transform.instance = Some(instances.len());
            world.validate_geometry_binding(entity, instance.geometry_assets)?;
            let next = RenderInstance::from_entity(
                entity,
                transform,
                world.entities.incarnation(id),
                palette,
                palette_root,
            );
            while prior_instances.peek().is_some_and(|prior| prior.id < id) {
                prior_instances.next();
            }
            let match_prior = prior_instances
                .next_if(|prior| prior.id == id)
                .filter(|prior| {
                    prior.geometry_assets == next.geometry_assets
                        && prior.mesh == next.mesh
                        && prior.material == next.material
                        && prior.incarnation == next.incarnation
                        && prior.transform_version == next.transform_version
                        && prior.outline == next.outline
                        && prior.emitter == next.emitter
                        && prior.grid == next.grid
                        && prior.skinned == next.skinned
                        && prior.palette_root == next.palette_root
                        && same_arc(&prior.palette, &next.palette)
                });
            instances.push(match_prior.cloned().unwrap_or_else(|| {
                stats.instances_updated += 1;
                Arc::new(next)
            }));
        }
        self.palettes.retain(|id, _| skin_indices.contains_key(id));
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
        let mut model_records = vec![[0; 20]; routes.model_capacity];
        for i in &instances {
            model_records[routes.model_slots[&i.id]] = i.gpu_model();
        }
        let models = crate::rdg::rows::Rows::new(&model_records);
        dependencies.light_count = light_count;
        let has_lights = light_count != 0;
        let scene = Arc::new(RenderScene {
            routes,
            models,
            geometry_revision: if geometry_assets.is_empty() {
                0
            } else {
                next_geometry_revision()
            },
            geometry_assets: Arc::new(geometry_assets),
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
        let roots = dependencies
            .skins
            .iter()
            .filter_map(|s| self.palettes[&s.id].space.as_ref().map(|space| space.root))
            .collect::<Vec<_>>();
        self.transforms.configure_pose_interiors(&roots);
        self.pending_visual = false;
        self.dependencies = dependencies;
        Ok(scene)
    }

    fn light(&self, id: Id, light: Light) -> Result<Option<RenderLight>> {
        let world = self.transforms.get(id).unwrap().world;
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
        let mut slots = Vec::with_capacity(skin.joints.len());
        let mut joints = Vec::with_capacity(skin.joints.len());
        for joint in &skin.joints {
            let &slot = self
                .transforms
                .slots
                .get(joint)
                .context("missing skin joint")?;
            slots.push(slot);
            joints.push((*joint, self.transforms.rows[slot].version));
        }
        let space = PoseSpace::bind(world, &self.transforms, &slots);
        let matrices: Arc<[Mat4]> = if let Some(space) = &space {
            space.initial(&skin.inverse_bind)
        } else {
            slots
                .iter()
                .zip(&skin.inverse_bind)
                .map(|(&slot, &inverse)| self.transforms.rows[slot].world * inverse)
                .collect()
        };
        let prior = self.palettes.get(&id).filter(|p| *p.matrices == *matrices);
        let changed = prior.is_none();
        let matrices = prior.map_or(matrices, |p| p.matrices.clone());
        self.palettes.insert(
            id,
            Palette {
                dependency: 0,
                space,
                slots,
                joints,
                inverse_bind: skin.inverse_bind.clone(),
                matrices,
            },
        );
        Ok(changed)
    }
}
