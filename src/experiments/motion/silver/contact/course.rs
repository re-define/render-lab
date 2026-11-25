use super::*;
use crate::experiments::lighting::scenes;

// Course coordinates are metres. Both animation alignment and camera shots
// reference the same layout as the walkable surfaces and obstacle meshes.
pub(super) const GAP_Z: f32 = -1.85;
pub(super) const RAIL_Z: f32 = 0.36;
pub(super) const GATE_Z: f32 = 2.42;
pub(super) const TURN: Vec3 = Vec3::new(0., 0., 3.50);
pub(super) const RAIL_TOP: f32 = 0.50;

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(super) struct Fixtures {
    pub landing: Id,
    #[serde(default)]
    pub service: Option<Id>,
    pub station: Id,
    pub ramp: Id,
    pub rail: Id,
    pub gate: Id,
}

pub(super) fn build(w: &mut World) -> (Id, Id, Id, Fixtures) {
    let cube = w.spawn(Entity {
        name: "Course box mesh".into(),
        mesh: Some(Arc::new(scenes::cube())),
        ..Default::default()
    });
    let sphere = w.spawn(Entity {
        name: "Console lamp mesh".into(),
        mesh: Some(Arc::new(scenes::sphere(24, 16))),
        ..Default::default()
    });
    let materials = [
        ("Deck", [0.17, 0.24, 0.30]),
        ("Column", [0.07, 0.12, 0.17]),
        ("Edge", [0.90, 0.53, 0.09]),
        ("Support", [0.08, 0.42, 0.46]),
        ("Panel", [0.37, 0.49, 0.55]),
        ("Indicator", [0.98, 0.30, 0.04]),
    ]
    .map(|(name, rgb)| {
        w.spawn(Entity {
            name: format!("{name} finish"),
            material: Some(Material {
                base_color: Vec3::from_array(rgb).extend(1.).to_array(),
                ..Default::default()
            }),
            ..Default::default()
        })
    });
    let mut place = |name: &str, mesh, material: usize, at: [f32; 3], size: [f32; 3], rot: Quat| {
        w.spawn(Entity {
            name: name.into(),
            instance: Some(Instance::new(mesh, Some(materials[material]))),
            local: Mat4::from_scale_rotation_translation(size.into(), rot, at.into()),
            ..Default::default()
        })
    };
    let flat = Quat::IDENTITY;
    let floor = place(
        "Launch deck",
        cube,
        0,
        [0., -0.24, -3.205],
        [1.9, 0.48, 2.21],
        flat,
    );
    let landing = place(
        "Receiving deck",
        cube,
        0,
        [0., -0.24, 1.38],
        [1.9, 0.48, 5.96],
        flat,
    );
    let station = place(
        "Inspection station foundation",
        cube,
        1,
        [1.64, -0.24, 3.50],
        [1.40, 0.48, 1.72],
        flat,
    );
    // The lower maintenance floor leaves a real 0.50 m break between deck ends.
    let service = place(
        "Service floor",
        cube,
        1,
        [0.45, -0.91, 0.10],
        [4.9, 0.12, 9.2],
        flat,
    );
    for z in [GAP_Z - 0.28, GAP_Z + 0.28] {
        place(
            "Gap edge marking",
            cube,
            2,
            [0., 0.001, z],
            [1.88, 0.002, 0.045],
            flat,
        );
    }
    let rail = place(
        "Vault rail",
        cube,
        2,
        [0., RAIL_TOP - 0.035, RAIL_Z],
        [1.38, 0.07, 0.10],
        flat,
    );
    for x in [-0.65, 0.65] {
        place(
            "Rail bracket",
            cube,
            1,
            [x, (RAIL_TOP - 0.07) * 0.5, RAIL_Z],
            [0.07, RAIL_TOP - 0.07, 0.10],
            flat,
        );
        place(
            "Rail mounting plate",
            cube,
            4,
            [x, 0.025, RAIL_Z],
            [0.20, 0.05, 0.27],
            flat,
        );
    }
    for x in [-0.86, 0.86] {
        place(
            "Service arch upright",
            cube,
            4,
            [x, 0.66, GATE_Z],
            [0.11, 1.32, 0.21],
            flat,
        );
        place(
            "Arch foot",
            cube,
            1,
            [x, 0.06, GATE_Z],
            [0.21, 0.12, 0.34],
            flat,
        );
    }
    let gate = place(
        "Low inspection beam",
        cube,
        3,
        [0., 1.26, GATE_Z],
        [1.83, 0.14, 0.20],
        flat,
    );
    for x in [-0.64, 0., 0.64] {
        place(
            "Beam clearance marking",
            cube,
            2,
            [x, 1.245, GATE_Z - 0.102],
            [0.18, 0.055, 0.006],
            flat,
        );
    }
    let ramp = place(
        "Station approach ramp",
        cube,
        4,
        [0.85, 0.015, 3.50],
        [0.76, 0.045, 1.32],
        Quat::from_rotation_z(0.11),
    );
    let plate = place(
        "Spring plate",
        cube,
        3,
        [1.75, 0.10, 3.50],
        [1.21, 0.08, 1.32],
        Quat::from_rotation_z(0.055),
    );
    place(
        "Console riser",
        cube,
        1,
        [2.17, 0.57, 3.87],
        [0.15, 1.14, 0.17],
        flat,
    );
    place(
        "Console bezel",
        cube,
        4,
        [2.13, 1.29, 3.87],
        [0.13, 0.32, 0.34],
        flat,
    );
    let target = place(
        "Console contact lamp",
        sphere,
        5,
        [2.055, 1.29, 3.87],
        [0.07; 3],
        Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
    );
    (
        floor,
        plate,
        target,
        Fixtures {
            service: Some(service),
            landing,
            station,
            ramp,
            rail,
            gate,
        },
    )
}

pub(super) fn camera(shot: &str) -> Option<Camera> {
    let (target, distance, yaw, pitch) = match shot {
        "wide" => (Vec3::new(0.30, 0.48, 0.13), 6.1, -1.28, 0.30),
        "jump" => (Vec3::new(0., 0.68, GAP_Z), 3.08, -1.24, 0.15),
        "vault" => (Vec3::new(0., 0.72, RAIL_Z), 2.53, -1.19, 0.17),
        "duck" => (Vec3::new(0., 0.74, GATE_Z), 2.97, -1.13, 0.13),
        "finish" => (Vec3::new(1.0, 0.74, 3.56), 3.05, 0.36, 0.21),
        _ => return None,
    };
    Some(Camera {
        target,
        distance,
        yaw,
        pitch,
    })
}
