use crate::{
    experiments::effects::river,
    scene::{Mesh, Texture},
};
use glam::{Mat4, Vec2, Vec3};

pub(super) struct Terrain {
    low: Vec2,
    high: Vec2,
    segments: [u32; 2],
    size: u32,
    river: bool,
}

impl Terrain {
    pub fn meadow() -> Self {
        Self {
            low: Vec2::new(-74., -132.),
            high: Vec2::new(74., 37.),
            segments: [118, 136],
            size: 512,
            river: false,
        }
    }

    pub fn forest() -> Self {
        Self {
            low: Vec2::new(-535., -816.),
            high: Vec2::new(535., 170.),
            segments: [278, 256],
            size: 1024,
            river: false,
        }
    }

    pub fn river() -> Self {
        Self {
            low: Vec2::new(-292., river::START),
            high: Vec2::new(292., river::END),
            segments: [320, 440],
            size: 1024,
            river: true,
        }
    }
    pub fn light(&self, texture: &Texture, p: Vec2) -> f32 {
        let px = (((p - self.low) / (self.high - self.low)) * self.size as f32)
            .as_uvec2()
            .min(glam::UVec2::splat(self.size - 1));
        (f32::from(texture.rgba[((px.y * self.size + px.x) * 4) as usize]) / 255.).powf(2.2)
    }
    /// Intersect a vertical with the heightfield's actual render triangle at this XZ.
    pub fn surface_at(&self, mesh: &Mesh, p: Vec2) -> (f32, Vec3) {
        let [nx, nz] = self.segments;
        let rowf =
            ((p.y - self.low.y) / (self.high.y - self.low.y) * nz as f32).clamp(0., nz as f32);
        let row = (rowf as u32).min(nz - 1);
        let t = rowf - row as f32;
        let vertex = |r, c| Vec3::from(mesh.positions[(r * (nx + 1) + c) as usize]);
        let mut lo = 0;
        let mut hi = nx;
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            let x = vertex(row, mid).lerp(vertex(row + 1, mid), t).x;
            if p.x < x {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        let a = vertex(row, lo);
        let b = vertex(row + 1, lo);
        let c = vertex(row, hi);
        let d = vertex(row + 1, hi);
        let edge = c.lerp(b, t);
        let (h, n) = if p.x <= edge.x {
            let across = (p.x - a.lerp(b, t).x) / (c.x - a.x);
            (
                a.y + (b.y - a.y) * t + (c.y - a.y) * across,
                (b - a).cross(c - a),
            )
        } else {
            let across = (p.x - edge.x) / (d.x - b.x);
            (edge.y + (d.y - b.y) * across, (b - c).cross(d - c))
        };
        (h, n.normalize())
    }

    pub fn colour_riverbed(&self, texture: &mut Texture) {
        for (i, rgba) in texture.rgba.chunks_exact_mut(4).enumerate() {
            let p = self.low
                + (Vec2::new((i as u32 % self.size) as f32, (i as u32 / self.size) as f32) + 0.5)
                    / self.size as f32
                    * (self.high - self.low);
            let d = river::shore(p.x, p.y);
            let shade = (f32::from(rgba[0]) / 255.).powf(2.2);
            let mottling = (p.x * 0.24 + (p.y * 0.13).sin() * 2.).sin() * (p.y * 0.31).cos();
            let moss = Vec3::new(0.117, 0.172, 0.057)
                .lerp(Vec3::new(0.085, 0.110, 0.050), (1. - shade) * 0.6);
            let gravel = Vec3::new(0.255, 0.229, 0.164);
            let color = gravel.lerp(moss, ((d + 0.22 * mottling) / 2.8).clamp(0., 1.))
                * (0.85 + 0.13 * mottling)
                * shade;
            for (dst, v) in rgba[..3].iter_mut().zip(color.to_array()) {
                *dst = (v.powf(1. / 2.2) * 255.).round() as u8;
            }
        }
    }

    pub fn build(&self, crowns: &[(Mat4, usize)]) -> (Mesh, Texture) {
        let [columns, rows] = self.segments;
        let mut mesh = super::generate::empty();
        for row in 0..=rows {
            for col in 0..=columns {
                let mut uv = Vec2::new(col as f32 / columns as f32, row as f32 / rows as f32);
                let z = self.low.y + uv.y * (self.high.y - self.low.y);
                let x = if self.river {
                    // Reserve a strip of terrain columns for the moving banks at every row.
                    let c = river::center(z);
                    let w = river::width(z) + 1.7;
                    let a = columns / 2 - 12;
                    let b = columns / 2 + 12;
                    if col < a {
                        self.low.x + (c - w - self.low.x) * col as f32 / a as f32
                    } else if col <= b {
                        c - w + 2. * w * (col - a) as f32 / (b - a) as f32
                    } else {
                        c + w + (self.high.x - c - w) * (col - b) as f32 / (columns - b) as f32
                    }
                } else {
                    self.low.x + uv.x * (self.high.x - self.low.x)
                };
                uv.x = (x - self.low.x) / (self.high.x - self.low.x);
                let elevation = if self.river { river::ground } else { ground };
                let h = elevation(x, z);
                let n = Vec3::new(h - elevation(x + 0.1, z), 0.1, h - elevation(x, z + 0.1))
                    .normalize();
                mesh.positions.push([x, h, z]);
                mesh.normals.push(n.into());
                mesh.uv.push(uv.min(Vec2::splat(0.99999)).into());
                if row < rows && col < columns {
                    let i = row * (columns + 1) + col;
                    let next = i + columns + 1;
                    mesh.indices.extend([i, next, i + 1, i + 1, next, next + 1]);
                }
            }
        }
        (mesh, self.occlusion(crowns))
    }

    fn occlusion(&self, crowns: &[(Mat4, usize)]) -> Texture {
        let size = self.size as usize;
        let mut light = vec![1f32; size * size];
        let extent = self.high - self.low;
        let raster = Vec2::splat(self.size as f32) / extent;
        for &(model, variant) in crowns {
            let root = model.w_axis.truncate();
            let scale = model.x_axis.truncate().length();
            let center = Vec2::new(root.x, root.z);
            let radius = scale * if variant < 2 { 2.7 } else { 3.0 };
            let center_px = (center - self.low) * raster;
            let radius_px = 2.1 * radius * raster;
            let start = (center_px - radius_px).floor().max(Vec2::ZERO);
            let end = (center_px + radius_px)
                .ceil()
                .min(Vec2::splat((size - 1) as f32));
            if start.x > end.x || start.y > end.y {
                continue;
            }
            for y in start.y as usize..=end.y as usize {
                for x in start.x as usize..=end.x as usize {
                    let p = (Vec2::new(x as f32, y as f32) + 0.5 - center_px) / raster;
                    let distance = p.length_squared() / radius.powi(2);
                    let canopy = 0.43 * (-distance * 1.27).exp();
                    let root = 0.29 * (-distance * 12.).exp();
                    light[y * size + x] = (light[y * size + x] * (1. - canopy - root)).max(0.30);
                }
            }
        }
        let rgba = light
            .into_iter()
            .flat_map(|v| {
                let k = (v.powf(1. / 2.2) * 255.).round() as u8;
                [k, k, k, 255]
            })
            .collect();
        Texture {
            width: self.size,
            height: self.size,
            rgba,
        }
    }
}

pub(super) fn trail(z: f32) -> f32 {
    1.9 * (z * 0.055).sin() + 0.75 * (z * 0.029).sin()
}

pub(super) fn ground(x: f32, z: f32) -> f32 {
    let broad = 1.18 * (0.037 * x + 0.021 * z).sin() * (0.026 * z).cos()
        + 0.68 * (0.059 * z - 0.035 * x).cos();
    let ridge = 1.7 * (-((z + 59.) / 39.).powi(2)).exp() * (0.035 * x + 0.25).sin();
    broad + ridge + 0.24 * (0.11 * x + 0.13 * z).sin()
}
