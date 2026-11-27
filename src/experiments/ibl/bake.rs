use anyhow::{Result, ensure};
use glam::{Vec2, Vec3};
use half::f16;
use rayon::prelude::*;
use std::{
    f32::consts::{PI, TAU},
    io::Cursor,
    sync::Arc,
};

pub(super) const HDR: &[u8] = include_bytes!("../../../assets/environments/studio_small_02_1k.hdr");
const SAMPLES: u32 = 256;
const LEVELS: usize = 7;
const LUT_EDGE: u32 = 128;

struct Map {
    width: u32,
    pixels: Vec<Vec3>,
}

fn direction(width: u32, i: u32) -> Vec3 {
    let theta = TAU * ((i / width) as f32 + 0.5) / width as f32;
    let phi = TAU * ((i % width) as f32 + 0.5) / width as f32 - PI;
    let (s, y) = theta.sin_cos();
    let (z, x) = phi.sin_cos();
    Vec3::new(x * s, y, z * s)
}

impl Map {
    fn from_fn(width: u32, sample: impl Fn(Vec3) -> Vec3 + Sync) -> Self {
        let pixels = (0..width * (width / 2))
            .into_par_iter()
            .map(|i| sample(direction(width, i)))
            .collect();
        Self { width, pixels }
    }

    fn sample(&self, v: Vec3) -> Vec3 {
        let u = (v.z.atan2(v.x) / TAU + 0.5) * self.width as f32 - 0.5;
        let h = (self.width / 2) as i32;
        let y = v.y.clamp(-1., 1.).acos() / PI * h as f32 - 0.5;
        let xy = |x: i32, y: i32| {
            self.pixels[(y.clamp(0, h - 1) as u32 * self.width
                + x.rem_euclid(self.width as i32) as u32) as usize]
        };
        let (x0, y0) = (u.floor() as i32, y.floor() as i32);
        let (fx, fy) = (u - u.floor(), y - y.floor());
        xy(x0, y0)
            .lerp(xy(x0 + 1, y0), fx)
            .lerp(xy(x0, y0 + 1).lerp(xy(x0 + 1, y0 + 1), fx), fy)
    }

    fn encode(&self, words: &mut Vec<u32>) -> [u32; 2] {
        let offset = words.len() as u32;
        for rgb in &self.pixels {
            words.extend([pair(rgb.x, rgb.y), pair(rgb.z, 0.)]);
        }
        [offset, self.width]
    }
}

fn pair(x: f32, y: f32) -> u32 {
    let encode = |v: f32| u32::from(f16::from_f32(v.clamp(0., 65504.)).to_bits());
    encode(x) | encode(y) << 16
}

fn sequence(i: u32) -> Vec2 {
    Vec2::new(
        (i as f32 + 0.5) / SAMPLES as f32,
        i.reverse_bits() as f32 / 4294967296.,
    )
}

fn orient(local: Vec3, n: Vec3) -> Vec3 {
    let up = if n.y.abs() < 0.995 { Vec3::Y } else { Vec3::X };
    let t = up.cross(n).normalize();
    t * local.x + n.cross(t) * local.y + n * local.z
}

fn ggx(u: Vec2, alpha: f32) -> Vec3 {
    let (sin, cos) = (TAU * u.x).sin_cos();
    let z = ((1. - u.y) / (1. + (alpha * alpha - 1.) * u.y)).sqrt();
    let radius = (1. - z * z).max(0.).sqrt();
    Vec3::new(radius * cos, radius * sin, z)
}

fn prefilter(mips: &[Map], roughness: f32, n: Vec3) -> Vec3 {
    let alpha = roughness * roughness;
    let a2 = alpha * alpha;
    let mut sum = Vec3::ZERO;
    let mut weight = 0.;
    for i in 0..SAMPLES {
        let h = ggx(sequence(i), alpha);
        let l = 2. * h.z * h - Vec3::Z;
        if l.z > 0. {
            let world = orient(l, n);
            let d = (h.z * h.z * (a2 - 1.) + 1.).max(1e-6);
            let pdf = a2 / (4. * PI * d * d);
            let row = (1. - world.y * world.y).max(0.0001).sqrt();
            let texel = TAU * PI * row / (mips[0].pixels.len() as f32);
            let mip = (0.5 * (1. / (pdf * SAMPLES as f32 * texel)).log2())
                .clamp(0., (mips.len() - 1) as f32);
            let lower = mip.floor() as usize;
            let upper = mip.ceil() as usize;
            sum += mips[lower]
                .sample(world)
                .lerp(mips[upper].sample(world), mip.fract())
                * l.z;
            weight += l.z;
        }
    }
    sum / weight.max(1e-6)
}

fn integrated_brdf(nv: f32, roughness: f32) -> Vec2 {
    let view = Vec3::new((1. - nv * nv).sqrt(), 0., nv);
    let alpha = roughness * roughness;
    let k = alpha * 0.5;
    let g = |cos: f32| cos / (cos * (1. - k) + k);
    let mut sum = Vec2::ZERO;
    for i in 0..SAMPLES {
        let h = ggx(sequence(i), alpha.max(0.0001));
        let vh = view.dot(h).max(0.);
        let l = 2. * vh * h - view;
        if l.z > 0. {
            let visibility = g(nv) * g(l.z) * vh / (h.z * nv).max(1e-6);
            let fresnel = (1. - vh).powi(5);
            sum += Vec2::new(1. - fresnel, fresnel) * visibility;
        }
    }
    sum / SAMPLES as f32
}

pub(super) fn tables() -> Result<Arc<[u8]>> {
    let src =
        image::DynamicImage::from_decoder(image::codecs::hdr::HdrDecoder::new(Cursor::new(HDR))?)?
            .into_rgb32f();
    ensure!(
        src.width() == src.height() * 2,
        "expected a 2:1 HDR environment"
    );
    let mut mips = vec![Map {
        width: src.width(),
        pixels: src.pixels().map(|p| p.0.into()).collect(),
    }];
    while mips.last().unwrap().width > 8 {
        let previous = mips.last().unwrap();
        let width = previous.width / 2;
        let pixels = (0..width * (width / 2))
            .map(|i| {
                let top = ((i / width) * 2 * previous.width + (i % width) * 2) as usize;
                let bottom = top + previous.width as usize;
                let latitude = PI * ((i / width * 2) as f32 + 0.5) / (previous.width / 2) as f32;
                let a = latitude.sin();
                let b = (latitude + TAU / previous.width as f32).sin();
                ((previous.pixels[top] + previous.pixels[top + 1]) * a
                    + (previous.pixels[bottom] + previous.pixels[bottom + 1]) * b)
                    / (2. * (a + b))
            })
            .collect();
        mips.push(Map { width, pixels });
    }
    let mut words = vec![0; 4 + LEVELS * 2];
    // Diffuse preintegration uses solid-angle quadrature to preserve energy
    // from small bright sources even when the HDR image has a dark background.
    let source = mips.iter().find(|m| m.width <= 128).unwrap();
    let angular_step = TAU / source.width as f32;
    let diffuse_sources = source
        .pixels
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let i = i as u32;
            let theta = (i / source.width) as f32 * angular_step;
            let omega = angular_step * (theta.cos() - (theta + angular_step).cos());
            (direction(source.width, i), *p * (omega / PI))
        })
        .collect::<Vec<_>>();
    let diffuse = Map::from_fn(64, |n| {
        diffuse_sources
            .iter()
            .map(|&(ray, energy)| energy * n.dot(ray).max(0.))
            .sum()
    });
    let [start, width] = diffuse.encode(&mut words);
    words[0..2].copy_from_slice(&[start, width]);
    let header = mips[0].encode(&mut words);
    words[4..6].copy_from_slice(&header);
    for level in 1..LEVELS {
        let width = (256 >> (level - 1)).max(16);
        let roughness = level as f32 / (LEVELS - 1) as f32;
        let header = Map::from_fn(width, |n| prefilter(&mips, roughness, n)).encode(&mut words);
        words[4 + 2 * level..6 + 2 * level].copy_from_slice(&header);
    }
    let offset = words.len() as u32;
    words[2..4].copy_from_slice(&[offset, LUT_EDGE]);
    words.par_extend((0..LUT_EDGE * LUT_EDGE).into_par_iter().map(|i| {
        let nv = ((i % LUT_EDGE) as f32 + 0.5) / LUT_EDGE as f32;
        let r = ((i / LUT_EDGE) as f32 + 0.5) / LUT_EDGE as f32;
        let p = integrated_brdf(nv, r);
        pair(p.x, p.y)
    }));
    Ok(bytemuck::cast_slice(&words).into())
}
