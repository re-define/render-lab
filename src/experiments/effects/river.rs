//! A continuous channel shared by terrain carving, plant placement and the GPU water grid.
use super::*;
use crate::rdg::{Access, Dispatch, Op, PassPhase, Resource, Shader, Storage};
const LANES: u32 = 33;
const ROWS: u32 = 1201;
pub(super) const VERTICES: usize = (LANES * ROWS) as usize;
pub(in crate::experiments) const START: f32 = -530.;
pub(in crate::experiments) const END: f32 = 70.;
pub(super) const WATER: &str = "Forest river water";

pub(in crate::experiments) fn center(z: f32) -> f32 {
    19. * (z * 0.024).sin() + 8.2 * (z * 0.057 + 0.83).sin()
}
pub(in crate::experiments) fn width(z: f32) -> f32 {
    6.6 + 1.75 * (z * 0.039 + 0.24).sin() + 0.79 * (z * 0.105).sin()
}
pub(in crate::experiments) fn level(z: f32) -> f32 {
    0.8 - z * 0.014
}
pub(in crate::experiments) fn shore(x: f32, z: f32) -> f32 {
    (x - center(z)).abs() - width(z)
}
pub(in crate::experiments) fn ground(x: f32, z: f32) -> f32 {
    let d = shore(x, z);
    if d < 0. {
        return level(z) - 0.10 - 1.45 * (-d * 0.34).tanh();
    }
    let slope = 1. - (-d * 0.12).exp();
    let hills = 2.6
        + 1.14 * (x * 0.046 + z * 0.031).sin()
        + 0.57 * (z * 0.105).cos()
        + 4.3 * (1. - (-d * 0.025).exp()) * (0.55 + 0.45 * (x * 0.025 - z * 0.018).sin());
    level(z) - 0.10 + 0.24 * d.min(3.) + slope * hills
}

pub(in crate::experiments) fn install(world: &mut World) {
    world.spawn(Entity {
        name: "River clock".into(),
        components: [(
            SETTING.into(),
            serde_json::json!(Settings {
                kind: Kind::River,
                time: 0.
            }),
        )]
        .into(),
        ..Default::default()
    });
    let mut data = Mesh {
        positions: Vec::with_capacity(VERTICES),
        uv: Vec::with_capacity(VERTICES),
        normals: vec![],
        indices: vec![],
        joints: vec![],
        weights: vec![],
    };
    for row in 0..ROWS {
        let z = START + (END - START) * row as f32 / (ROWS - 1) as f32;
        for lane in 0..LANES {
            let u = 2. * lane as f32 / (LANES - 1) as f32 - 1.;
            data.positions
                .push([center(z) + u * (width(z) + 0.95), level(z), z]);
            data.uv.push([u, z]);
            if lane + 1 < LANES && row + 1 < ROWS {
                let a = row * LANES + lane;
                data.indices
                    .extend([a, a + LANES, a + 1, a + 1, a + LANES, a + LANES + 1]);
            }
        }
    }
    let grid = mesh(world, "River channel grid", data);
    world.set_setting(grid, crate::experiments::clusters::ORDINARY, true.into());
    let mat = material(world, "Flowing river", WATER, [0.036, 0.145, 0.118]);
    instance(world, "Forest river", grid, mat, Mat4::IDENTITY);
}

fn surface(
    g: &mut FrameGraph,
    rest: Resource,
    time: f64,
    previous: Option<(f64, Mat4)>,
) -> Resource {
    let (phase, name, shader, stride) = if previous.is_some() {
        (
            PassPhase::Motion,
            "Advect river previous positions",
            spv!("river_previous.comp").as_slice(),
            16,
        )
    } else {
        (
            PassPhase::Animation,
            "Advect forest river surface",
            spv!("river_surface.comp").as_slice(),
            32,
        )
    };
    let output = g.resource(
        name,
        Storage::Buffer {
            bytes: stride * VERTICES,
            initial: None,
        },
    );
    g.prefer_local_buffer(output);
    let (old, model) = previous.unwrap_or((time, Mat4::IDENTITY));
    let dz = (END - START) / (ROWS - 1) as f32;
    let mut params = bytemuck::bytes_of(&[LANES, ROWS]).to_vec();
    params.extend(bytemuck::bytes_of(&[old as f32, (time - old) as f32, dz]));
    if previous.is_some() {
        // Two descriptor indices precede the push data. Align the matrix to absolute offset 32.
        params.resize(24, 0);
        params.extend(bytemuck::bytes_of(&model.to_cols_array()));
    }
    g.pass_in(
        phase,
        name,
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(shader),
                bindings: vec![(rest, Access::ComputeRead), (output, Access::ComputeWrite)],
                push: params,
            },
            groups: [LANES.div_ceil(8), ROWS.div_ceil(8), 1],
        },
    );
    output
}
pub(super) fn deform(
    g: &mut FrameGraph,
    rest: Resource,
    time: f64,
    previous: Option<(f64, Mat4)>,
) -> (Resource, Option<Resource>) {
    let vertices = surface(g, rest, time, None);
    let history = previous.map(|p| surface(g, rest, time, Some(p)));
    (vertices, history)
}

pub(in crate::experiments) struct Reflector {
    pub(super) id: u32,
    pub(super) surface: SurfaceBuffers,
}

/// Sample only non-river pixels. Reflecting pixels can update their own color in-place.
pub(in crate::experiments) fn reflections(
    g: &mut FrameGraph,
    s: &Snapshot,
    ids: Resource,
    depth: Resource,
    color: Resource,
    rivers: &[Reflector],
) {
    if rivers.is_empty() {
        return;
    }
    let vp = s.view_projection();
    let mut data = bytemuck::bytes_of(&vp.to_cols_array()).to_vec();
    data.extend(bytemuck::bytes_of(&vp.inverse().to_cols_array()));
    data.extend(bytemuck::bytes_of(&s.camera.eye().extend(1.).to_array()));
    data.extend(bytemuck::bytes_of(&[rivers.len() as u32, 0, 0, 0]));
    let mut buffers = Vec::new();
    for water in rivers {
        data.extend(bytemuck::bytes_of(
            &water.surface.model.inverse().to_cols_array(),
        ));
        data.extend(bytemuck::bytes_of(&[water.id, 0, 0, 0]));
        buffers.extend([water.surface.vertices, water.surface.faces]);
    }
    let streams = g.address_table(buffers);
    let params = g.input("River reflection view and surfaces", data);
    let [w, h] = s.extent;
    g.pass_in(
        PassPhase::Lighting,
        "Reflect visible banks in river",
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(spv!("river_reflections.comp")),
                bindings: vec![
                    (ids, Access::ComputeSample),
                    (depth, Access::ComputeSample),
                    (color, Access::ComputeReadWrite),
                    (params, Access::ComputeRead),
                    (streams, Access::ComputeRead),
                ],
                push: vec![],
            },
            groups: [w.div_ceil(8), h.div_ceil(8), 1],
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires Vulkan compute"]
    fn river_moves_downstream_and_backtraces_to_the_previous_bend() -> Result<()> {
        let mut world = World::default();
        install(&mut world);
        let grid = world
            .entities
            .values()
            .find_map(|e| e.mesh.as_ref())
            .unwrap();
        let packed: Vec<[f32; 8]> = grid
            .positions
            .iter()
            .zip(&grid.uv)
            .map(|(p, uv)| [p[0], p[1], p[2], uv[0], 0., 1., 0., uv[1]])
            .collect();
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        let mut gpu = crate::rhi::Backend::open()?;
        let mut read_surface =
            |time: f64, previous: Option<(f64, Mat4)>| -> Result<Vec<[f32; 4]>> {
                let mut g = FrameGraph::new([LANES * if previous.is_some() { 4 } else { 8 }, ROWS]);
                let rest = g.input("Rest river", bytemuck::cast_slice(&packed).to_vec());
                let data = surface(&mut g, rest, time, previous);
                g.set_output(data);
                Ok(gpu
                    .execute(g.compile()?, &pool)?
                    .pixels
                    .chunks_exact(16)
                    .map(bytemuck::pod_read_unaligned)
                    .collect())
            };
        let start = read_surface(0., None)?;
        let end = read_surface(0.25, None)?;
        let offset = Vec3::new(2., 1., -3.);
        let prior = read_surface(0.25, Some((0., Mat4::from_translation(offset))))?;
        assert!(end.iter().flatten().all(|v| v.is_finite()));
        assert_eq!(
            prior[LANES as usize / 2][3],
            0.,
            "new inlet water has no previous position"
        );
        for lane in [0, LANES / 2, LANES - 1] {
            let i = (390 * LANES + lane) as usize;
            let current = end[2 * i];
            let u = grid.uv[i][0];
            let speed = 1.10 + 0.61 * (1. - u * u);
            let old = Vec3::from_array(prior[i][0..3].try_into().unwrap()) - offset;
            assert!((old.z - (current[2] - 0.25 * speed)).abs() < 0.0001);
            assert!((old.x - (center(old.z) + u * (width(old.z) + 0.95))).abs() < 0.01);
            assert!((start[2 * i][3] - current[3] - 0.25 * 1.52).abs() < 0.001);
            assert_ne!(start[2 * i][1], current[1]);
        }
        Ok(())
    }
}
