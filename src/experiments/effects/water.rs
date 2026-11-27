use super::*;
use crate::rdg::{Access, Dispatch, Op, PassPhase, Resource};

// One vertex per lateral streamline and parcel age. Bed and raster share these units.
pub(super) const LANES: u32 = 49;
pub(super) const ROWS: u32 = 241;
pub(super) const VERTICES: usize = (LANES * ROWS) as usize;
const STEPS: u32 = 5;
const START: f32 = -4.;
const TREAD: f32 = 1.38;
const RISE: f32 = 0.50;
const FLOOR: f32 = 0.14;
const AGE: f32 = 5.23;
const OUTLET: f32 = 6.25;

pub(super) fn scene() -> Result<World> {
    let mut world = setup(Kind::Water);
    let cube = mesh(
        &mut world,
        "Unit block",
        super::super::lighting::scenes::cube(),
    );
    let basalt = material(&mut world, "Blue basalt", STONE, [0.17, 0.23, 0.24]);
    let trim = material(&mut world, "Bronze lip", STONE, [0.52, 0.29, 0.095]);
    let ground = material(&mut world, "Quarry paving", STONE, [0.095, 0.127, 0.14]);
    slab(
        &mut world,
        "Quarry floor",
        cube,
        ground,
        [0., -0.32, 0.],
        [42., 0.6, 42.],
    );
    slab(
        &mut world,
        "Tailrace",
        cube,
        basalt,
        [0., FLOOR * 0.5, 1.0],
        [3.72, FLOOR, 11.6],
    );
    for i in 0..STEPS {
        let top = FLOOR + (STEPS - i) as f32 * RISE;
        let z = START + (i as f32 + 0.5) * TREAD;
        slab(
            &mut world,
            "Stone tread",
            cube,
            basalt,
            [0., top * 0.5, z],
            [3.54, top, TREAD],
        );
        for side in [-1., 1.] {
            slab(
                &mut world,
                "Channel curb",
                cube,
                basalt,
                [side * 1.69, top + 0.08, z],
                [0.17, 0.16, TREAD - 0.025],
            );
        }
    }
    let tail = START + STEPS as f32 * TREAD;
    for side in [-1., 1.] {
        slab(
            &mut world,
            "Tailrace wall",
            cube,
            basalt,
            [side * 1.72, FLOOR + 0.075, (tail + OUTLET) * 0.5],
            [0.22, 0.15, OUTLET - tail],
        );
    }
    // The last parcel row meets the outlet beneath the slotted drain cover.
    for bar in 0..6 {
        slab(
            &mut world,
            "Outlet grille",
            cube,
            trim,
            [0., FLOOR + 0.10, OUTLET - 0.05 + bar as f32 * 0.095],
            [3.48, 0.048, 0.047],
        );
    }
    let head = FLOOR + STEPS as f32 * RISE;
    slab(
        &mut world,
        "Headworks",
        cube,
        basalt,
        [0., head * 0.5, START - 0.38],
        [3.6, head, 0.76],
    );
    slab(
        &mut world,
        "Bronze inlet",
        cube,
        trim,
        [0., head + 0.09, START - 0.26],
        [2.48, 0.18, 0.32],
    );
    let mut grid = Mesh {
        positions: Vec::with_capacity(VERTICES),
        indices: vec![],
        normals: vec![],
        uv: vec![],
        joints: vec![],
        weights: vec![],
    };
    for row in 0..ROWS {
        for lane in 0..LANES {
            // Persist the rest grid as a normal ECS mesh for inspection and scene bounds.
            let z = START + row as f32 / (ROWS - 1) as f32 * 9.0;
            let level = (STEPS as f32 - ((z - START) / TREAD).floor()).clamp(0., STEPS as f32);
            grid.positions.push([
                (lane as f32 / (LANES - 1) as f32 - 0.5) * 2.18,
                FLOOR + RISE * level + 0.08,
                z,
            ]);
            if lane + 1 < LANES && row + 1 < ROWS {
                let i = row * LANES + lane;
                grid.indices
                    .extend([i, i + LANES, i + 1, i + 1, i + LANES, i + LANES + 1]);
            }
        }
    }
    let surface = mesh(&mut world, "Cascade grid", grid);
    world.set_setting(surface, crate::experiments::clusters::ORDINARY, true.into());
    let water = material(&mut world, "Aerated water", WATER, [0.025, 0.24, 0.26]);
    instance(&mut world, "Cascade", surface, water, Mat4::IDENTITY);
    world.validate()?;
    Ok(world)
}
fn storage(g: &mut FrameGraph, label: &str, stride: usize) -> Resource {
    g.local_buffer(label, stride * VERTICES)
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Channel {
    dimensions: [u32; 4],
    bed: [f32; 4],
    timing: [f32; 4],
}
pub(super) fn parcels(
    g: &mut FrameGraph,
    time: f64,
    rewind: f32,
    previous: Option<Mat4>,
) -> Resource {
    let r = storage(
        g,
        if previous.is_some() {
            "Previous water positions"
        } else {
            "Water parcels and aeration"
        },
        16,
    );
    let channel = Channel {
        dimensions: [LANES, ROWS, STEPS, u32::from(previous.is_some())],
        bed: [START, TREAD, RISE, FLOOR],
        timing: [(time % 256.) as f32, rewind, AGE, OUTLET],
    };
    let mut settings = bytemuck::bytes_of(&channel).to_vec();
    settings.extend_from_slice(bytemuck::bytes_of(
        &previous.unwrap_or(Mat4::IDENTITY).to_cols_array(),
    ));
    let uniform = g.input("Cascade solver parameters", settings);
    g.pass_in(
        if previous.is_some() {
            PassPhase::Motion
        } else {
            PassPhase::Animation
        },
        if previous.is_some() {
            "Track previous water parcels"
        } else {
            "Integrate water over stair colliders"
        },
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(spv!("water_integrate.comp")),
                bindings: vec![(uniform, Access::ComputeRead), (r, Access::ComputeWrite)],
                push: vec![],
            },
            groups: [LANES.div_ceil(8), ROWS.div_ceil(8), 1],
        },
    );
    r
}
pub(super) fn simulate(
    g: &mut FrameGraph,
    time: f64,
    previous: Option<(f64, Mat4)>,
) -> (Resource, Option<Resource>) {
    let points = parcels(g, time, 0., None);
    let vertices = storage(g, "Cascade surface vertices", 32);
    g.pass_in(
        PassPhase::Animation,
        "Reconstruct cascade surface",
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(spv!("water_surface.comp")),
                bindings: vec![
                    (points, Access::ComputeRead),
                    (vertices, Access::ComputeWrite),
                ],
                push: bytemuck::bytes_of(&[LANES, ROWS]).to_vec(),
            },
            groups: [LANES.div_ceil(8), ROWS.div_ceil(8), 1],
        },
    );
    let old = previous.map(|(old, model)| {
        parcels(
            g,
            time,
            (time - old).min(f64::from(AGE)) as f32,
            Some(model),
        )
    });
    (vertices, old)
}
