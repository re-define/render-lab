//! Rest curves are shared between the solver and the skinned render mesh.
use super::{solver::*, *};
use crate::{
    experiments::lighting::scenes::{cube, sphere},
    scene::{Entity, Instance, Material, Mesh, MotionPolicy, Skin},
};
use std::f32::consts::TAU;
pub(super) const FOLLOWERS: usize = 8;
const TONES: usize = 4;

fn material(w: &mut World, name: &str, color: [f32; 3], roughness: f32, metallic: f32) -> Id {
    w.spawn(Entity {
        name: name.into(),
        material: Some(Material {
            base_color: [color[0], color[1], color[2], 1.],
            roughness,
            metallic,
            ..Default::default()
        }),
        ..Default::default()
    })
}
fn mesh(w: &mut World, name: &str, data: Mesh) -> Id {
    w.spawn(Entity {
        name: name.into(),
        mesh: Some(Arc::new(data)),
        ..Default::default()
    })
}
fn piece(
    w: &mut World,
    name: &str,
    parent: Option<Id>,
    geometry: Id,
    mat: Id,
    position: Vec3,
    size: Vec3,
) -> Id {
    w.spawn(Entity {
        name: name.into(),
        parent,
        local: Mat4::from_scale_rotation_translation(size, Quat::IDENTITY, position),
        instance: Some(Instance::new(geometry, Some(mat))),
        ..Default::default()
    })
}

fn empty_mesh() -> Mesh {
    Mesh {
        positions: vec![],
        indices: vec![],
        normals: vec![],
        uv: vec![],
        joints: vec![],
        weights: vec![],
    }
}
// A tapered triangular fibre. Its interpolated rest centre and guide weights agree
// at both sides of each guide boundary, including the seam at the back of the ring.
fn fibres(guides: &[Guide], tone: usize) -> Mesh {
    let mut m = empty_mesh();
    for (i, g) in guides.iter().enumerate() {
        let next = (i + 1) % GUIDES;
        for k in 0..FOLLOWERS {
            let seed = (i * FOLLOWERS + k) as f32;
            if (i * 7 + k * 3 + (i / 5)) % TONES != tone {
                continue;
            }
            let blend = (k as f32 + 0.5) / FOLLOWERS as f32;
            let curve: Curve = std::array::from_fn(|j| {
                let t = j as f32 / (POINTS - 1) as f32;
                let base = g.rest[j].lerp(guides[next].rest[j], blend);
                let n = (base - CENTER) * Vec3::new(1., 0., 1.);
                base + n.normalize_or_zero()
                    * t
                    * (0.0035 * (seed * 3.7).sin() + 0.005 * t * (11. * t + seed * 1.17).sin())
            });
            add_fibre(
                &mut m,
                &curve,
                i,
                blend,
                0.0026 + 0.0005 * (seed * 5.3).sin(),
            );
        }
    }
    m
}

fn add_fibre(m: &mut Mesh, curve: &Curve, i: usize, blend: f32, radius: f32) {
    let next = (i + 1) % GUIDES;
    let first = m.positions.len() as u32;
    for j in 0..POINTS {
        let t = j as f32 / (POINTS - 1) as f32;
        let dir = tangent(curve, j);
        let az = TAU * (i as f32 + blend) / GUIDES as f32;
        let side = Vec3::new(az.cos(), 0., -az.sin());
        let a = (side - side.dot(dir) * dir).normalize_or_zero();
        let b = dir.cross(a);
        let r = radius * (1. - 0.75 * t.powi(3));
        for edge in 0..3 {
            let (s, c) = (TAU * edge as f32 / 3.).sin_cos();
            let n = c * a + s * b;
            m.positions.push((curve[j] + n * r).to_array());
            m.normals.push(n.to_array());
            m.uv.push([blend, t]);
            m.joints
                .push([(i * POINTS + j) as u16, (next * POINTS + j) as u16, 0, 0]);
            m.weights.push([1. - blend, blend, 0., 0.]);
            if j + 1 < POINTS {
                let u = first + (j * 3 + edge) as u32;
                let v = first + (j * 3 + (edge + 1) % 3) as u32;
                m.indices.extend([u, v, v + 3, u, v + 3, u + 3]);
            }
        }
    }
}

pub(super) fn build() -> Result<World> {
    let mut w = World::default();
    let owner = w.spawn(Entity {
        name: "Strand groom".into(),
        ..Default::default()
    });
    w.set_motion_policy(owner, MotionPolicy::STATIC)?;
    let skull = w.spawn(Entity {
        name: "Animated head mount".into(),
        parent: Some(owner),
        ..Default::default()
    });
    let ball = mesh(&mut w, "Studio sphere", sphere(48, 32));
    let block = mesh(&mut w, "Studio box", cube());
    let glaze = material(&mut w, "Celadon mannequin", [0.19, 0.36, 0.35], 0.43, 0.08);
    let brass = material(&mut w, "Brass collar", [0.54, 0.30, 0.10], 0.33, 0.63);
    let base = material(&mut w, "Slate pedestal", [0.074, 0.11, 0.13], 0.52, 0.15);
    piece(
        &mut w,
        "Head collision sphere",
        Some(skull),
        ball,
        glaze,
        CENTER,
        Vec3::splat(2. * HEAD),
    );
    let mut cap = sphere(48, 32);
    let cap_triangles: Vec<_> = cap
        .indices
        .chunks_exact(3)
        .filter(|tri| {
            tri.iter().all(|&idx| {
                let n = Vec3::from(cap.positions[idx as usize]) * 2.;
                n.y > 0.18 - 0.25 * n.z
            })
        })
        .flatten()
        .copied()
        .collect();
    cap.indices = cap_triangles;
    let scalp = mesh(&mut w, "Scalp cap", cap);
    let roots = material(&mut w, "Auburn roots", [0.10, 0.031, 0.018], 0.72, 0.);
    piece(
        &mut w,
        "Hairline",
        Some(skull),
        scalp,
        roots,
        CENTER,
        Vec3::splat(2. * (HEAD + 0.001)),
    );
    piece(
        &mut w,
        "Nose",
        Some(skull),
        ball,
        glaze,
        CENTER + Vec3::new(0., -0.02, -0.266),
        Vec3::new(0.072, 0.123, 0.092),
    );
    piece(
        &mut w,
        "Neck",
        Some(owner),
        ball,
        glaze,
        Vec3::new(0., 1.08, 0.),
        Vec3::new(0.19, 0.43, 0.20),
    );
    // A cylinder with spherical ends matches the analytic shoulder capsule.
    let mut capsule = sphere(56, 32);
    for p in &mut capsule.positions {
        let local = Vec3::from(*p) * 2. * SHOULDER_RADIUS;
        let v = Vec3::new(local.y + SHOULDER_X * local.y.signum(), local.z, local.x);
        *p = v.to_array();
    }
    for n in &mut capsule.normals {
        *n = [n[1], n[2], n[0]];
    }
    let shoulder = mesh(&mut w, "Shoulder capsule", capsule);
    piece(
        &mut w,
        "Shoulders",
        Some(owner),
        shoulder,
        glaze,
        Vec3::Y * SHOULDER_Y,
        Vec3::ONE,
    );
    piece(
        &mut w,
        "Bust",
        Some(owner),
        ball,
        glaze,
        Vec3::Y * 0.67,
        Vec3::new(0.63, 0.42, 0.29),
    );
    piece(
        &mut w,
        "Stand",
        Some(owner),
        block,
        base,
        Vec3::Y * 0.39,
        Vec3::new(0.13, 0.25, 0.13),
    );
    piece(
        &mut w,
        "Collar",
        Some(owner),
        block,
        brass,
        Vec3::Y * 0.48,
        Vec3::new(0.16, 0.025, 0.16),
    );
    piece(
        &mut w,
        "Plinth",
        Some(owner),
        block,
        base,
        Vec3::Y * 0.25,
        Vec3::new(0.73, 0.075, 0.48),
    );

    let guides: Vec<_> = (0..GUIDES).map(groom).collect();
    let mut joints = Vec::with_capacity(GUIDES * POINTS);
    let mut inverse_bind = Vec::with_capacity(GUIDES * POINTS);
    for (i, g) in guides.iter().enumerate() {
        for (j, &p) in g.rest.iter().enumerate() {
            joints.push(w.spawn(Entity {
                name: format!("Guide {i} point {j}"),
                parent: Some(owner),
                local: Mat4::from_translation(p),
                ..Default::default()
            }));
            inverse_bind.push(Mat4::from_translation(-p));
        }
    }
    let skin = w.spawn(Entity {
        name: "Guide palette".into(),
        skin: Some(Skin {
            joints,
            inverse_bind,
        }),
        ..Default::default()
    });
    let colors = [
        [0.15, 0.043, 0.026],
        [0.31, 0.099, 0.051],
        [0.48, 0.185, 0.083],
        [0.25, 0.067, 0.035],
    ];
    let mut followers = [0; TONES];
    for (tone, color) in colors.into_iter().enumerate() {
        let mat = material(&mut w, "Copper fibre", color, 0.5, 0.04);
        let geo = mesh(&mut w, "Follow strands", fibres(&guides, tone));
        let inst = piece(
            &mut w,
            "Groom fibres",
            Some(owner),
            geo,
            mat,
            Vec3::ZERO,
            Vec3::ONE,
        );
        w.entities.get_mut(&inst).unwrap().skin_binding = Some(skin);
        followers[tone] = inst;
    }
    let mut lines = empty_mesh();
    for (i, g) in guides.iter().enumerate() {
        add_fibre(&mut lines, &g.rest, i, 0., 0.0033);
    }
    let debug = mesh(&mut w, "Guide curves", lines);
    let mat = material(&mut w, "Guide amber", [0.94, 0.52, 0.08], 0.65, 0.);
    let display = piece(
        &mut w,
        "Guide display",
        Some(owner),
        debug,
        mat,
        Vec3::ZERO,
        Vec3::ONE,
    );
    w.entities.get_mut(&display).unwrap().skin_binding = Some(skin);
    w.set_setting(display, super::super::composition::HIDDEN, true.into());
    w.set_typed_component(
        owner,
        SETTING,
        Mount {
            head: skull,
            skin,
            display,
            followers,
        },
    );
    w.set_typed_component(owner, CONTROL, Controls::default());
    w.set_typed_component(owner, STATE, Dynamics::default());
    w.validate()?;
    Ok(w)
}
