use anyhow::{Context, Result, ensure};
use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
};
mod changes;
pub(crate) use changes::{Cursor as ChangeCursor, Write as EntityWrite};
pub use changes::{Entities, LocalPose};
mod components;
mod gi;
mod light;
pub use gi::{GiHitMaterial, GiHitModel, GiNode, GiTexture};
pub use light::{Light, LightKind};

pub type Id = u64;

#[derive(Clone, Serialize, Deserialize)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    pub normals: Vec<[f32; 3]>,
    pub uv: Vec<[f32; 2]>,
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
}

impl Mesh {
    pub fn edges(&self) -> Vec<Edge> {
        let mut edges: BTreeMap<[u32; 2], Vec<u32>> = BTreeMap::new();
        for (face, tri) in self.indices.chunks_exact(3).enumerate() {
            for j in 0..3 {
                let mut edge = [tri[j], tri[(j + 1) % 3]];
                edge.sort();
                edges.entry(edge).or_default().push(face as u32);
            }
        }
        edges
            .into_iter()
            .map(|(vertices, faces)| Edge { vertices, faces })
            .collect()
    }
}
// Each cluster indexes the base mesh. Optional hierarchy and adjacency are independent components.
#[derive(Clone, Serialize, Deserialize)]
pub struct Meshlet {
    pub triangles: Vec<u32>,
    pub refined_group: Option<u32>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ClusterNode {
    pub meshlets: Vec<u32>,
    pub center: [f32; 3],
    pub radius: f32,
    pub error: f32,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Edge {
    pub vertices: [u32; 2],
    pub faces: Vec<u32>,
}
/// Rest-space rig for procedural foliage and its optional distant representation.
#[derive(Clone, Serialize, Deserialize)]
#[serde(from = "PlantGeometryData")]
pub struct PlantGeometry {
    pub bones: Vec<PlantBone>,
    pub displacement_bound: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voxel_cell_size: Option<f32>,
}

#[derive(Deserialize)]
struct PlantGeometryData {
    bones: Vec<PlantBone>,
    displacement_bound: f32,
    voxel_cell_size: Option<f32>,
    // Older scenes stored a part-wide occupancy grid. Its cell size carries
    // over as the starting scale for building local voxel candidates.
    voxels: Option<LegacyVoxelGrid>,
}

impl From<PlantGeometryData> for PlantGeometry {
    fn from(data: PlantGeometryData) -> Self {
        Self {
            bones: data.bones,
            displacement_bound: data.displacement_bound,
            voxel_cell_size: data
                .voxel_cell_size
                .or_else(|| data.voxels.map(|v| v.cell_size)),
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct PlantBone {
    pub parent: Option<u16>,
    pub pivot: [f32; 3],
    pub bend: f32,
}

#[derive(Deserialize)]
struct LegacyVoxelGrid {
    cell_size: f32,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}
impl Texture {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.width > 0
                && self.height > 0
                && (self.width as usize)
                    .checked_mul(self.height as usize)
                    .and_then(|n| n.checked_mul(4))
                    == Some(self.rgba.len()),
            "invalid texture extent"
        );
        Ok(())
    }
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Material {
    #[serde(default)]
    pub shading: Shading,
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub texture: Option<Id>,
    #[serde(default, skip_serializing_if = "GiHitMaterial::is_default")]
    pub gi_hit: GiHitMaterial,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            shading: Shading::Pbr,
            base_color: [0.75, 0.76, 0.8, 1.0],
            metallic: 0.0,
            roughness: 0.55,
            gi_hit: Default::default(),
            texture: None,
        }
    }
}
#[derive(Debug, Default, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Shading {
    #[default]
    Pbr,
    Toon,
    Custom(String),
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Skin {
    pub joints: Vec<Id>,
    pub inverse_bind: Vec<Mat4>,
}
#[derive(Clone, Serialize, Deserialize)]
pub enum Interpolation {
    Step,
    Linear,
    CubicSpline,
}
#[derive(Clone, Serialize, Deserialize)]
pub enum TrackValues {
    Translation(Vec<[f32; 3]>),
    Rotation(Vec<[f32; 4]>),
    Scale(Vec<[f32; 3]>),
    MorphWeights(Vec<f32>),
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Track {
    pub target: Id,
    pub times: Vec<f32>,
    pub values: TrackValues,
    pub interpolation: Interpolation,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Instance {
    pub mesh: Id,
    pub material: Option<Id>,
    #[serde(default, rename = "shading", skip_serializing)]
    pub(crate) legacy_shading: Option<Shading>,
    pub outline: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Entity {
    pub name: String,
    pub parent: Option<Id>,
    pub local: Mat4,
    pub instance: Option<Instance>,
    pub mesh: Option<Arc<Mesh>>,
    pub texture: Option<Arc<Texture>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gaussian_payload: Option<Arc<Vec<u8>>>,
    pub material: Option<Material>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub light: Option<Light>,
    pub skin: Option<Skin>,
    pub skin_binding: Option<Id>,
    pub animation: Option<Arc<Vec<Track>>>,
    pub meshlets: Option<Arc<Vec<Meshlet>>>,
    pub hierarchy: Option<Arc<Vec<ClusterNode>>>,
    pub topology: Option<Arc<Vec<Edge>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plant: Option<Arc<PlantGeometry>>,
    // Namespaced plugin components round-trip even when their systems are absent.
    pub components: BTreeMap<String, Value>,
}
impl Instance {
    pub fn new(mesh: Id, material: Option<Id>) -> Self {
        Self {
            mesh,
            material,
            legacy_shading: None,
            outline: false,
        }
    }
}
impl Default for Entity {
    fn default() -> Self {
        Self {
            name: String::new(),
            parent: None,
            local: Mat4::IDENTITY,
            instance: None,
            mesh: None,
            texture: None,
            gaussian_payload: None,
            material: None,
            light: None,
            skin: None,
            skin_binding: None,
            animation: None,
            meshlets: None,
            hierarchy: None,
            topology: None,
            plant: None,
            components: BTreeMap::new(),
        }
    }
}
#[derive(Clone, Deserialize)]
pub struct World {
    version: u32,
    pub entities: Entities,
    #[serde(skip)]
    runtime: components::RuntimeComponents,
    #[serde(skip)]
    pub(crate) simulation_resources: BTreeMap<&'static str, Arc<dyn std::any::Any + Send + Sync>>,
}
impl Default for World {
    fn default() -> Self {
        Self {
            version: 1,
            entities: Entities::default(),
            runtime: Default::default(),
            simulation_resources: Default::default(),
        }
    }
}
impl World {
    pub fn spawn(&mut self, e: Entity) -> Id {
        let id = self
            .entities
            .last_key_value()
            .map_or(1, |(k, _)| k.checked_add(1).expect("entity IDs exhausted"));
        self.entities.insert(id, e);
        self.runtime.remove_entity(id);
        id
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1,
            "unsupported scene version {}",
            self.version
        );
        let transforms = self.transforms()?;
        for (&id, e) in &self.entities {
            if let Some(light) = e.light {
                light
                    .validate(transforms[&id])
                    .with_context(|| format!("light entity {id}"))?;
            }
            if let Some(i) = &e.instance {
                ensure!(
                    self.entities.get(&i.mesh).is_some_and(|e| e.mesh.is_some()),
                    "entity {id}: missing mesh"
                );
                if let Some(m) = i.material {
                    ensure!(
                        self.entities.get(&m).is_some_and(|e| e.material.is_some()),
                        "entity {id}: missing material"
                    );
                }
            }
            if let Some(m) = &e.material {
                m.gi_hit
                    .validate(self)
                    .with_context(|| format!("entity {id} GI material"))?;
            }
            if let Some(t) = e.material.as_ref().and_then(|m| m.texture) {
                ensure!(
                    self.entities.get(&t).is_some_and(|e| e.texture.is_some()),
                    "missing texture {t}"
                );
            }
            if let Some(t) = &e.texture {
                t.validate().with_context(|| format!("entity {id}"))?;
            }
            if let Some(m) = &e.mesh {
                let n = m.positions.len();
                ensure!(
                    m.indices.len() % 3 == 0 && m.indices.iter().all(|&i| (i as usize) < n),
                    "entity {id}: invalid triangle indices"
                );
                ensure!(
                    [m.normals.len(), m.uv.len(), m.joints.len(), m.weights.len()]
                        .iter()
                        .all(|&l| l == 0 || l == n)
                        && m.joints.len() == m.weights.len(),
                    "entity {id}: inconsistent vertex streams"
                );
            }
            if let Some(s) = &e.skin {
                ensure!(
                    s.joints.len() == s.inverse_bind.len()
                        && s.joints.iter().all(|j| self.entities.contains_key(j)),
                    "entity {id}: invalid skin"
                );
            }
            if let Some(plant) = &e.plant {
                let mesh = e.mesh.as_ref().context("plant rig requires a mesh")?;
                ensure!(
                    !plant.bones.is_empty()
                        && plant.bones.len() <= 64
                        && (0. ..=64.).contains(&plant.displacement_bound)
                        && plant.bones.iter().enumerate().all(|(i, b)| b
                            .parent
                            .is_none_or(|p| usize::from(p) < i)
                            && b.pivot.iter().all(|v| v.is_finite())
                            && (0. ..=0.5).contains(&b.bend)),
                    "entity {id}: invalid plant rig"
                );
                ensure!(
                    mesh.joints.len() == mesh.positions.len()
                        && mesh
                            .joints
                            .iter()
                            .zip(&mesh.weights)
                            .all(
                                |(j, w)| j.iter().zip(w).all(|(&joint, &weight)| weight == 0.
                                    || ((0. ..=1.).contains(&weight)
                                        && usize::from(joint) < plant.bones.len()))
                                    && (w.iter().sum::<f32>() - 1.).abs() < 0.0001
                            ),
                    "entity {id}: invalid plant weights"
                );
                ensure!(
                    plant
                        .voxel_cell_size
                        .is_none_or(|size| (0.001..=16.).contains(&size)),
                    "entity {id}: invalid plant voxel size"
                );
            }
            if let Some(s) = e.skin_binding {
                ensure!(
                    self.entities.get(&s).is_some_and(|e| e.skin.is_some()),
                    "entity {id}: missing skin"
                );
            }
            if let Some(tracks) = &e.animation {
                for track in tracks.iter() {
                    ensure!(
                        self.entities.contains_key(&track.target)
                            && !track.times.is_empty()
                            && track.times.iter().all(|t| t.is_finite() && *t >= 0.)
                            && track.times.windows(2).all(|t| t[1] > t[0]),
                        "invalid animation target or time"
                    );
                    let count = track.times.len()
                        * if matches!(track.interpolation, Interpolation::CubicSpline) {
                            3
                        } else {
                            1
                        };
                    ensure!(
                        match &track.values {
                            TrackValues::Rotation(v) => v.len() == count,
                            TrackValues::Translation(v) | TrackValues::Scale(v) => v.len() == count,
                            TrackValues::MorphWeights(v) =>
                                v.len() >= count && v.len() % count == 0,
                        },
                        "invalid keyframe count"
                    );
                }
            }
            if let Some(clusters) = &e.meshlets {
                let m = e.mesh.as_ref().context("meshlets require a base mesh")?;
                ensure!(
                    clusters
                        .iter()
                        .flat_map(|c| &c.triangles)
                        .all(|&t| (t as usize) < m.indices.len() / 3),
                    "invalid meshlet triangle"
                );
            }
            if let Some(nodes) = &e.hierarchy {
                let clusters = e
                    .meshlets
                    .as_ref()
                    .context("cluster hierarchy requires meshlets")?;
                let mut owned = vec![false; clusters.len()];
                for (index, node) in nodes.iter().enumerate() {
                    for &c in &node.meshlets {
                        let cluster = clusters.get(c as usize).context("invalid meshlet ID")?;
                        ensure!(
                            !std::mem::replace(&mut owned[c as usize], true)
                                && cluster.refined_group.is_none_or(|i| (i as usize) < index),
                            "invalid cluster DAG"
                        );
                    }
                }
                ensure!(owned.iter().all(|&v| v), "unowned meshlet");
            }
            if let Some(edges) = &e.topology {
                let m = e.mesh.as_ref().context("topology requires a mesh")?;
                ensure!(
                    edges.iter().all(|x| x
                        .vertices
                        .iter()
                        .all(|&v| (v as usize) < m.positions.len())
                        && x.faces.iter().all(|&f| (f as usize) < m.indices.len() / 3)),
                    "invalid edge adjacency"
                );
            }
        }
        Ok(())
    }
    pub fn transforms(&self) -> Result<BTreeMap<Id, Mat4>> {
        let mut output = BTreeMap::new();
        for id in self.transform_order()? {
            let entity = &self.entities[&id];
            let parent = entity.parent.map_or(Mat4::IDENTITY, |p| output[&p]);
            output.insert(id, parent * entity.local);
        }
        Ok(output)
    }
    pub(crate) fn transform_order(&self) -> Result<Vec<Id>> {
        let mut order = Vec::with_capacity(self.entities.len());
        let mut placed = BTreeSet::new();
        for &id in self.entities.keys() {
            let mut path = Vec::new();
            let mut seen = BTreeSet::new();
            let mut at = Some(id);
            while let Some(i) = at {
                if placed.contains(&i) {
                    break;
                }
                ensure!(seen.insert(i), "transform hierarchy cycle at {i}");
                let e = self.entities.get(&i).context("missing parent")?;
                path.push(i);
                at = e.parent;
            }
            for i in path.into_iter().rev() {
                placed.insert(i);
                order.push(i);
            }
        }
        Ok(order)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut file = tempfile::NamedTempFile::new_in(dir)?;
        {
            let mut writer = std::io::BufWriter::new(file.as_file_mut());
            serde_json::to_writer(&mut writer, self)?;
            std::io::Write::flush(&mut writer)?;
        }
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }
    pub fn load(path: &Path) -> Result<Self> {
        let mut world: Self =
            serde_json::from_reader(std::io::BufReader::new(std::fs::File::open(path)?))?;
        world.validate()?;
        // v1 stored the shading path on each use. Split only mismatching legacy uses
        // and share a material variant between all uses with the same old model.
        let mut variants = BTreeMap::new();
        let legacy: Vec<_> = world
            .entities
            .iter_mut()
            .filter_map(|(&id, e)| {
                let i = e.instance.as_mut()?;
                Some((id, i.material, i.legacy_shading.take()?))
            })
            .collect();
        for (id, source, model) in legacy {
            let mut m = source
                .and_then(|k| world.entities[&k].material.clone())
                .unwrap_or_default();
            if m.shading == model {
                continue;
            }
            let variant = *variants.entry((source, model.clone())).or_insert_with(|| {
                m.shading = model;
                world.spawn(Entity {
                    name: "Migrated material".into(),
                    material: Some(m),
                    ..Default::default()
                })
            });
            world
                .entities
                .get_mut(&id)
                .unwrap()
                .instance
                .as_mut()
                .unwrap()
                .material = Some(variant);
        }
        Ok(world)
    }
    pub fn demo() -> Self {
        let mut w = Self::default();
        let mesh = w.spawn(Entity {
            name: "Octahedron".into(),
            mesh: Some(Arc::new(Mesh {
                positions: vec![
                    [0., 1., 0.],
                    [1., 0., 0.],
                    [0., 0., 1.],
                    [-1., 0., 0.],
                    [0., 0., -1.],
                    [0., -1., 0.],
                ],
                indices: vec![
                    0, 2, 1, 0, 3, 2, 0, 4, 3, 0, 1, 4, 5, 1, 2, 5, 2, 3, 5, 3, 4, 5, 4, 1,
                ],
                normals: vec![],
                uv: vec![],
                joints: vec![],
                weights: vec![],
            })),
            ..Default::default()
        });
        let geometry = w.entities.get_mut(&mesh).unwrap();
        geometry.topology = Some(Arc::new(geometry.mesh.as_ref().unwrap().edges()));
        for (x, shading, color) in [
            (-1.1, Shading::Pbr, [0.2, 0.57, 0.92, 1.]),
            (1.1, Shading::Toon, [0.95, 0.42, 0.15, 1.]),
        ] {
            let material = w.spawn(Entity {
                material: Some(Material {
                    shading: shading.clone(),
                    base_color: color,
                    ..Default::default()
                }),
                ..Default::default()
            });
            w.spawn(Entity {
                name: "Specimen".into(),
                local: Mat4::from_translation(Vec3::X * x),
                instance: Some(Instance {
                    outline: shading == Shading::Toon,
                    ..Instance::new(mesh, Some(material))
                }),
                ..Default::default()
            });
        }
        w
    }
}
