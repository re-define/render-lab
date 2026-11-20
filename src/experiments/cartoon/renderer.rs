use super::*;
use crate::{
    rdg::*,
    render::{Atlas, Forward, Snapshot, SurfaceBuffers, Targets, Technique, build_frame_with_post},
};
use glam::Mat4;
use std::sync::Mutex;

macro_rules! spirv {
    ($s:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/", $s, ".spv"))
    };
}

struct UploadedEdges {
    source: Arc<Vec<Edge>>,
    edges: Arc<[u8]>,
    incidence: Arc<[u8]>,
}
impl UploadedEdges {
    fn new(source: Arc<Vec<Edge>>) -> Result<Self> {
        let mut records = Vec::with_capacity(source.len());
        let mut incidence = Vec::new();
        for e in source.iter() {
            records.push([
                e.vertices[0],
                e.vertices[1],
                incidence.len().try_into()?,
                e.faces.len().try_into()?,
            ]);
            incidence.extend_from_slice(&e.faces);
        }
        Ok(Self {
            source,
            edges: bytemuck::cast_slice(&records).into(),
            incidence: bytemuck::cast_slice(&incidence).into(),
        })
    }
}

#[derive(Default)]
pub struct Cartoon {
    atlas: Mutex<Option<Arc<Atlas>>>,
    edges: Mutex<BTreeMap<Id, UploadedEdges>>,
}
impl Cartoon {
    pub fn register(materials: &mut crate::render::MaterialPrograms) -> Result<()> {
        if materials.bin(&Shading::Custom(PROGRAM.into())).is_err() {
            materials.register(PROGRAM, spirv!("character.comp"))?;
        }
        Ok(())
    }
    fn draw_outline(
        &self,
        g: &mut Graph,
        t: &Targets,
        view: Resource,
        s: &Snapshot,
        (mesh, surface): (Id, SurfaceBuffers),
        mode: Outline,
    ) -> Result<()> {
        use Access::Mesh as M;
        let mut bindings = vec![(surface.vertices, M), (surface.faces, M), (view, M)];
        let (name, shader, count): (&str, &'static [u8], u32) = match mode {
            Outline::Hull => (
                "Normal-extruded backfaces",
                spirv!("outline_hull.mesh"),
                surface.count,
            ),
            Outline::Quad => {
                let Some(topology) = s.world.entities[&mesh].topology.as_ref() else {
                    return Ok(());
                };
                if topology.is_empty() {
                    return Ok(());
                }
                let mut cache = self.edges.lock().unwrap();
                if !cache
                    .get(&mesh)
                    .is_some_and(|entry| Arc::ptr_eq(&entry.source, topology))
                {
                    cache.insert(mesh, UploadedEdges::new(topology.clone())?);
                }
                let entry = &cache[&mesh];
                for (name, data) in [
                    ("Silhouette edges", &entry.edges),
                    ("Incident faces", &entry.incidence),
                ] {
                    bindings.push((
                        g.resource(
                            name,
                            Storage::Buffer {
                                bytes: data.len(),
                                initial: Some(data.clone()),
                            },
                        ),
                        M,
                    ));
                }
                (
                    "Degenerate edge quads",
                    spirv!("outline_quad.mesh"),
                    topology.len().try_into()?,
                )
            }
            _ => return Ok(()),
        };
        let groups = count.div_ceil(32);
        g.pass(
            name,
            Op::Draw {
                program: Dispatch {
                    shader: Shader::Mesh(shader, spirv!("visibility.frag")),
                    bindings,
                    push: count.to_ne_bytes().to_vec(),
                },
                color: t.visibility,
                depth: t.depth,
                clear: false,
                count: DrawCount::Mesh([groups.min(32768), groups.div_ceil(32768), 1]),
            },
        );
        Ok(())
    }
}

impl Technique for Cartoon {
    fn build(&self, s: &Snapshot) -> Result<Graph> {
        let (_, cfg) = Settings::read(&s.world)?;
        let azimuth = cfg.azimuth.to_radians();
        let light = Vec3::new(azimuth.sin(), 0.45, azimuth.cos()).normalize();
        let atlas = {
            let mut cached = self.atlas.lock().unwrap();
            if !cached.as_ref().is_some_and(|a| a.matches(&s.world)) {
                *cached = Some(Arc::new(Atlas::from(&s.world)?));
            }
            cached.as_ref().unwrap().clone()
        };
        let bin = s.materials.bin(&Shading::Custom(PROGRAM.into()))?;
        build_frame_with_post(
            s,
            atlas,
            |g, t, transforms| {
                t.background = 0xffe4e5ebu32;
                let head = transforms.get(&cfg.head).context("missing head joint")?;
                let local = head.inverse().transform_vector3(light);
                let signed_angle = local.x.atan2(local.z) / std::f32::consts::PI;
                let sdf = t
                    .texture_region(cfg.sdf)
                    .context("missing face SDF texture")?;
                ensure!(
                    sdf[1] == 7 * sdf[2] && sdf[2] > 0,
                    "face SDF requires seven square tiles"
                );
                let mut outlines = vec![];
                for (i, posed) in Forward::default().posed_surfaces(s, transforms, |_, _| true)? {
                    let mut obj = t.material(s, i, Mat4::IDENTITY)?;
                    let cartoon = obj.routing[0] == bin;
                    if cartoon {
                        obj.params = [light.x, light.y, light.z, cfg.threshold];
                        if cfg.face_shadow && i.material == Some(cfg.face_material) {
                            obj.routing[1..].copy_from_slice(&[
                                sdf[0],
                                signed_angle.to_bits(),
                                sdf[2],
                            ]);
                        }
                    }
                    let surface = posed.draw(g, t, s, obj)?;
                    if cartoon && matches!(cfg.outline, Outline::Hull | Outline::Quad) {
                        outlines.push((i.mesh, surface));
                    }
                }
                if !outlines.is_empty() {
                    let mut camera = s.camera.view_projection(s.extent).to_cols_array().to_vec();
                    camera.extend(s.camera.eye().extend(1.).to_array());
                    camera.extend([s.extent[0] as f32, s.extent[1] as f32, cfg.width, 0.]);
                    let view = g.input("Outline view", bytemuck::cast_slice(&camera).to_vec());
                    for surface in outlines {
                        self.draw_outline(g, t, view, s, surface, cfg.outline)?;
                    }
                }
                Ok(())
            },
            |g, ids, depth, color| {
                if cfg.outline == Outline::Image {
                    let [w, h] = s.extent;
                    let z = g.resource(
                        "Outline depth samples",
                        Storage::Buffer {
                            bytes: w as usize * h as usize * 4,
                            initial: None,
                        },
                    );
                    g.pass(
                        "Read outline depth on GPU",
                        Op::ReadPixels {
                            image: depth,
                            buffer: z,
                        },
                    );
                    // Descriptor indices take 12 bytes. The following uvec2 is 8-byte aligned.
                    let push = bytemuck::bytes_of(&[0u32, w, h, cfg.width.to_bits()]).to_vec();
                    g.pass(
                        "Image-space outlines",
                        Op::Compute {
                            program: Dispatch {
                                shader: Shader::Compute(spirv!("outline_image.comp")),
                                bindings: vec![
                                    (ids, Access::ComputeRead),
                                    (z, Access::ComputeRead),
                                    (color, Access::ComputeReadWrite),
                                ],
                                push,
                            },
                            groups: [w.div_ceil(8), h.div_ceil(8), 1],
                        },
                    );
                }
                Ok(())
            },
        )
    }
}
