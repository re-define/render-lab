//! Quantized Gaussian records and compact indices, built once per material load.
use super::{Layout, bvh::Tree};
use anyhow::{Result, ensure};
use half::f16;

const HEADER_WORDS: usize = 12;
type Record = [u32; 4];

fn word(bytes: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(bytes[4 * i..4 * i + 4].try_into().unwrap())
}

fn half_pair(x: f32, y: f32) -> Result<(u32, [f32; 2])> {
    let pair = [f16::from_f32(x), f16::from_f32(y)];
    ensure!(
        pair.iter().all(|v| v.is_finite()),
        "Gaussian parameters exceed FP16 range"
    );
    Ok((
        u32::from(pair[0].to_bits()) | (u32::from(pair[1].to_bits()) << 16),
        pair.map(f16::to_f32),
    ))
}

fn box16(lo: u32, hi: u32) -> Result<u32> {
    let (lx, ly, hx, hy) = (lo & 65535, lo >> 16, hi & 65535, hi >> 16);
    if lx >= hx || ly >= hy {
        return Ok(u32::from_le_bytes([255, 255, 0, 0]));
    }
    // Store the last covered tile inclusively so a 4096px axis fits in 8 bits.
    let coords = [lx, ly, hx - 1, hy - 1];
    ensure!(
        coords.iter().all(|&v| v <= 255),
        "compact Gaussian bounds exceed 256 tiles per axis"
    );
    Ok(u32::from_le_bytes(coords.map(|v| v as u8)))
}

struct Quantized {
    record: Record,
    // Conservative support of the *decoded* conic, for rebuilding tile membership.
    lo: [u32; 2],
    hi: [u32; 2],
}

impl Quantized {
    fn new(src: &[u8], scale: [f32; 3], extent: [u32; 2]) -> Result<Self> {
        let f = |i| f32::from_bits(word(src, i));
        let bounds = box16(word(src, 8), word(src, 9))?;
        let [lx, ly, last_x, last_y] = bounds.to_le_bytes().map(|v| u32::from(v) * 16);
        let (center, delta) = half_pair(f(0) - lx as f32, f(1) - ly as f32)?;
        let (sx, sy) = (f(2).sqrt(), f(4).sqrt());
        let (diagonal, root) = half_pair(sx, sy)?;
        ensure!(
            root.iter().all(|&s| s > 0.),
            "Gaussian conic underflows FP16"
        );
        let corr = (128. * f(3) / (sx * sy))
            .round_ties_even()
            .clamp(-127., 127.) as i8;
        let color = std::array::from_fn::<_, 3, _>(|i| {
            if scale[i] == 0. {
                0
            } else {
                (f(5 + i) / scale[i]).round_ties_even().clamp(-127., 127.) as i8 as u8
            }
        });
        let attr = u32::from_le_bytes([color[0], color[1], color[2], corr as u8]);
        let rho = f64::from(corr) / 128.;
        let radius = (2. * 255f64.ln() / (1. - rho * rho)).sqrt();
        let mut lo = [lx, ly];
        let mut hi = [last_x + 16, last_y + 16];
        for i in 0..2 {
            let pos = f64::from(delta[i] + lo[i] as f32);
            let reach = radius / f64::from(root[i]) + 1.;
            lo[i] = lo[i].max((pos - reach).floor() as u32);
            hi[i] = hi[i].min(extent[i]).min(((pos + reach).ceil() + 1.) as u32);
        }
        Ok(Self {
            record: [center, diagonal, attr, bounds],
            lo,
            hi,
        })
    }
}

fn words(bytes: &mut Vec<u8>, data: impl IntoIterator<Item = u32>) {
    for v in data {
        bytes.extend(v.to_le_bytes());
    }
}

fn integers(bytes: &mut Vec<u8>, values: impl IntoIterator<Item = u32>, narrow: bool) {
    if narrow {
        for v in values {
            bytes.extend((v as u16).to_le_bytes());
        }
        bytes.resize(bytes.len().next_multiple_of(4), 0);
    } else {
        words(bytes, values);
    }
}

fn append_run(runs: &mut Vec<u32>, id: u32, bits: u32, limit: u32) {
    let mask = (1 << bits) - 1;
    if let Some(last) = runs.last_mut() {
        let length = (*last >> bits) + 1;
        if length < limit && (*last & mask) + length == id {
            *last += 1 << bits;
            return;
        }
    }
    runs.push(id);
}

pub(super) fn build(source: &[u8], layout: &Layout, bvh: bool) -> Result<Vec<u8>> {
    ensure!(
        layout.width <= 4096 && layout.height <= 4096,
        "compact indices support up to 4096 texels per axis; use tile or bvh for larger assets"
    );
    let tree = bvh.then(|| Tree::build(source, layout)).transpose()?;
    let src = tree.as_ref().map_or(source, |t| t.bytes.as_slice());
    let count = word(src, 4) as usize;
    let records = &src[32..32 + 40 * count];
    let mut scale = [0f32; 3];
    for row in source[32..32 + 40 * layout.count as usize].chunks_exact(40) {
        for (i, peak) in scale.iter_mut().enumerate() {
            *peak = peak.max(f32::from_bits(word(row, 5 + i)).abs());
        }
    }
    scale = scale.map(|v| v / 127.);
    let mut bytes = vec![0; HEADER_WORDS * 4];
    let mut tiles = if bvh {
        vec![]
    } else {
        vec![Vec::new(); layout.tiles as usize]
    };
    let nx = layout.width.div_ceil(layout.size);
    let bits = u32::BITS - (count as u32).saturating_sub(1).leading_zeros();
    let bits = bits.max(1);
    let limit = 1 << (if count <= 65536 { 16 } else { 32 } - bits);
    for (id, src) in records.chunks_exact(40).enumerate() {
        let q = Quantized::new(src, scale, [layout.width, layout.height])?;
        words(&mut bytes, q.record);
        if !bvh && q.lo[0] < q.hi[0] && q.lo[1] < q.hi[1] {
            for y in q.lo[1] / layout.size..q.hi[1].div_ceil(layout.size) {
                for x in q.lo[0] / layout.size..q.hi[0].div_ceil(layout.size) {
                    append_run(&mut tiles[(y * nx + x) as usize], id as u32, bits, limit);
                }
            }
        }
    }
    let directory = u32::try_from(bytes.len() / 4)?;
    let (entries, ids) = if let Some(tree) = tree.as_ref() {
        for node in tree.bytes[32 + 40 * count..].chunks_exact(16) {
            // Nodes bound whole legacy tiles. The last tile is implicitly clipped by the texture.
            let tile = |v: u32, upper| {
                let xy = [v & 65535, v >> 16].map(|s| if upper { s.div_ceil(16) } else { s / 16 });
                xy[0] | (xy[1] << 16)
            };
            let bounds = box16(tile(word(node, 0), false), tile(word(node, 1), true))?;
            let link = word(node, 2);
            ensure!(link < 1 << 28, "compact BVH link exceeds 28 bits");
            words(&mut bytes, [bounds, link | word(node, 3) << 28]);
        }
        (tree.nodes as u32, 0)
    } else {
        let mut offsets = vec![0u32];
        for t in &tiles {
            offsets.push(
                offsets
                    .last()
                    .unwrap()
                    .checked_add(t.len().try_into()?)
                    .ok_or_else(|| anyhow::anyhow!("compact tile reference overflow"))?,
            );
        }
        let runs = *offsets.last().unwrap();
        integers(&mut bytes, offsets, runs <= 65535);
        let ids = u32::try_from(bytes.len() / 4)?;
        integers(&mut bytes, tiles.into_iter().flatten(), count <= 65536);
        (runs, ids)
    };
    let magic = if bvh { *b"GQB1" } else { *b"GQT1" };
    let header = [
        u32::from_le_bytes(magic),
        layout.width,
        layout.height,
        layout.size,
        count as u32,
        entries,
        directory,
        ids,
        scale[0].to_bits(),
        scale[1].to_bits(),
        scale[2].to_bits(),
        bits,
    ];
    for (dst, value) in bytes.chunks_exact_mut(4).zip(header) {
        dst.copy_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_split_at_gaps_and_length_limit() {
        for (bits, limit) in [(12, 16), (16, 1), (17, 32768)] {
            let ids: Vec<_> = (0..39).chain(42..49).chain(4090..4096).collect();
            let mut runs = Vec::new();
            for &id in &ids {
                append_run(&mut runs, id, bits, limit);
            }
            let decoded: Vec<_> = runs
                .into_iter()
                .flat_map(|v| {
                    let first = v & ((1 << bits) - 1);
                    first..first + (v >> bits) + 1
                })
                .collect();
            assert_eq!(decoded, ids);
        }
    }

    #[test]
    fn compact_node_bounds_round_out_and_empty_models_have_valid_tables() -> Result<()> {
        let mut layout = Layout {
            width: 17,
            height: 19,
            size: 32,
            count: 1,
            refs: 1,
            tiles: 1,
        };
        let mut source = vec![0; 32];
        source[16..20].copy_from_slice(&1u32.to_le_bytes());
        words(
            &mut source,
            [
                1f32.to_bits(),
                2f32.to_bits(),
                0.5f32.to_bits(),
                0,
                0.5f32.to_bits(),
                0,
                0.2f32.to_bits(),
                (-0.2f32).to_bits(),
                0,
                2 | (2 << 16),
            ],
        );
        let bvh = build(&source, &layout, true)?;
        let node = word(&bvh, 6) as usize;
        assert_eq!(word(&bvh, node).to_le_bytes(), [0, 0, 1, 1]);
        assert_eq!(
            box16(255, 256 | 256 << 16)?.to_le_bytes(),
            [255, 0, 255, 255]
        );
        assert_eq!(word(&bvh, node + 1), 1 << 28);
        assert_eq!(
            word(&bvh, 12 + 2).to_le_bytes(),
            [0, 127, (-127i8) as u8, 0]
        );
        layout.count = 0;
        source[16..20].fill(0);
        let empty = build(&source, &layout, false)?;
        assert_eq!((word(&empty, 5), empty.len()), (0, 52));
        assert_eq!(build(&source, &layout, true)?.len(), 48);
        layout.width = 4097;
        assert!(build(&source, &layout, true).is_err());
        Ok(())
    }
}
