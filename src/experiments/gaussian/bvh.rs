//! Stackless point queries over the texel rectangles used by the Gaussian forward pass.
use super::Layout;
use anyhow::{Result, ensure};

const LEAF_SIZE: usize = 8;
const NODE_BYTES: usize = 16;

#[derive(Clone, Copy)]
struct Bounds {
    lo: [u32; 2],
    hi: [u32; 2], // exclusive
}

impl Bounds {
    fn join(self, b: Self) -> Self {
        Self {
            lo: std::array::from_fn(|i| self.lo[i].min(b.lo[i])),
            hi: std::array::from_fn(|i| self.hi[i].max(b.hi[i])),
        }
    }
    fn area(self) -> u64 {
        u64::from(self.hi[0] - self.lo[0]) * u64::from(self.hi[1] - self.lo[1])
    }
}

#[derive(Clone, Copy)]
struct Primitive {
    bounds: Bounds,
    source: usize,
}
impl Primitive {
    fn key(self, axis: usize) -> (u64, usize) {
        // A size split also separates low-frequency Gaussians from fine detail.
        let key = if axis == 2 {
            self.bounds.area()
        } else {
            u64::from(self.bounds.lo[axis] + self.bounds.hi[axis])
        };
        (key, self.source)
    }
}

fn split(items: &mut [Primitive]) -> usize {
    let n = items.len();
    let mut suffix = vec![items[0].bounds; n];
    let mut best = (u64::MAX, 0, 0);
    // For point queries, expected work is proportional to covered area × count.
    // Limit unbalanced splits to bound build depth on adversarial inputs.
    let margin = n.div_ceil(8);
    for axis in 0..3 {
        items.sort_unstable_by_key(|p| p.key(axis));
        suffix[n - 1] = items[n - 1].bounds;
        for i in (1..n - 1).rev() {
            suffix[i] = items[i].bounds.join(suffix[i + 1]);
        }
        let mut left = items[0].bounds;
        for i in 1..=n - margin {
            if i >= margin {
                let cost = left.area() * i as u64 + suffix[i].area() * (n - i) as u64;
                best = best.min((cost, axis, i));
            }
            left = left.join(items[i].bounds);
        }
    }
    items.sort_unstable_by_key(|p| p.key(best.1));
    best.2
}

fn emit(items: &mut [Primitive], nodes: &mut Vec<[u32; 4]>, first: u32) {
    let index = nodes.len();
    let bounds = items.iter().map(|p| p.bounds).reduce(Bounds::join).unwrap();
    let xy = |v: [u32; 2]| v[0] | (v[1] << 16);
    nodes.push([xy(bounds.lo), xy(bounds.hi), first, items.len() as u32]);
    if items.len() > LEAF_SIZE {
        let mid = split(items);
        let (left, right) = items.split_at_mut(mid);
        emit(left, nodes, first);
        emit(right, nodes, first + mid as u32);
        nodes[index][2] = nodes.len() as u32; // escape to the next sibling or end
        nodes[index][3] = 0;
    }
}

pub(super) struct Tree {
    pub bytes: Vec<u8>,
    pub nodes: usize,
    pub records: usize,
}
impl Tree {
    pub fn build(source: &[u8], layout: &Layout) -> Result<Self> {
        let records = &source[32..32 + 40 * layout.count as usize];
        let extent = [layout.width, layout.height];
        let mut items = Vec::with_capacity(layout.count as usize);
        for (source, rec) in records.chunks_exact(40).enumerate() {
            // The shader rejects all samples outside the original CUDA coverage.
            let unpack = |i| {
                let xy = u32::from_le_bytes(rec[i..i + 4].try_into().unwrap());
                [(xy & 65535) * 16, (xy >> 16) * 16]
            };
            let lo = unpack(32);
            let hi = unpack(36);
            let hi = std::array::from_fn(|i| hi[i].min(extent[i]));
            if lo[0] < hi[0] && lo[1] < hi[1] {
                items.push(Primitive {
                    bounds: Bounds { lo, hi },
                    source,
                });
            }
        }
        let mut nodes = Vec::new();
        if !items.is_empty() {
            emit(&mut items, &mut nodes, 0);
        }
        let node_start = 8 + 10 * items.len();
        let byte_size = 4 * node_start + NODE_BYTES * nodes.len();
        ensure!(
            byte_size / 4 <= u32::MAX as usize,
            "Gaussian BVH exceeds shader address range"
        );
        let mut bytes = Vec::with_capacity(byte_size);
        for word in [
            u32::from_le_bytes(*b"G2B1"),
            layout.width,
            layout.height,
            0,
            items.len() as u32,
            nodes.len() as u32,
            node_start as u32,
            0,
        ] {
            bytes.extend(word.to_le_bytes());
        }
        for p in &items {
            bytes.extend_from_slice(&records[40 * p.source..40 * (p.source + 1)]);
        }
        for node in &nodes {
            for word in node {
                bytes.extend(word.to_le_bytes());
            }
        }
        Ok(Self {
            bytes,
            nodes: nodes.len(),
            records: items.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn word(data: &[u8], i: usize) -> u32 {
        u32::from_le_bytes(data[4 * i..4 * i + 4].try_into().unwrap())
    }

    #[test]
    fn traversal_finds_each_covering_record_once_in_shuffled_input() -> Result<()> {
        let dims = [71, 53];
        let mut rects = Vec::new();
        for i in 0..83 {
            let lo = [(31 * i + 3) % 7, (17 * i + 1) % 5];
            let hi = [lo[0] + i % 4, lo[1] + 1 + i % 3];
            rects.push((lo, hi));
        }
        let mut data = vec![0; 32];
        for (i, &(lo, hi)) in rects.iter().enumerate() {
            let mut record = [0; 10];
            record[0] = i as u32;
            record[8] = lo[0] | lo[1] << 16;
            record[9] = hi[0] | hi[1] << 16;
            for w in record {
                data.extend(w.to_le_bytes());
            }
        }
        let mut layout = Layout {
            width: dims[0],
            height: dims[1],
            size: 16,
            count: rects.len() as u32,
            refs: 0,
            tiles: 0,
        };
        let tree = Tree::build(&data, &layout)?;
        assert!(tree.nodes > 1 && tree.records < rects.len());
        let bytes = &tree.bytes;
        let first_node = word(bytes, 6) as usize;
        assert_eq!(
            bytes.len(),
            32 + 40 * tree.records + NODE_BYTES * tree.nodes
        );
        let contains = |x, y, lo: u32, hi: u32| {
            x >= (lo & 65535) && x < (hi & 65535) && y >= (lo >> 16) && y < (hi >> 16)
        };
        for y in 0..dims[1] {
            for x in 0..dims[0] {
                let mut found = vec![];
                let mut node = 0;
                while node < tree.nodes {
                    let at = first_node + 4 * node;
                    let link = word(bytes, at + 2) as usize;
                    let count = word(bytes, at + 3) as usize;
                    if contains(x, y, word(bytes, at), word(bytes, at + 1)) {
                        assert!(count <= LEAF_SIZE);
                        for j in 0..count {
                            let r = 8 + 10 * (link + j);
                            if contains(x / 16, y / 16, word(bytes, r + 8), word(bytes, r + 9)) {
                                found.push(word(bytes, r) as usize);
                            }
                        }
                    } else if count == 0 {
                        assert!(link > node && link <= tree.nodes);
                        node = link;
                        continue;
                    }
                    node += 1;
                }
                let expected = rects
                    .iter()
                    .enumerate()
                    .filter_map(|(i, &(lo, hi))| {
                        (x / 16 >= lo[0] && x / 16 < hi[0] && y / 16 >= lo[1] && y / 16 < hi[1])
                            .then_some(i)
                    })
                    .collect::<Vec<_>>();
                found.sort_unstable();
                assert_eq!(found, expected);
            }
        }
        layout.count = 0;
        let empty = Tree::build(&data, &layout)?;
        assert_eq!((empty.bytes.len(), empty.nodes), (32, 0));
        Ok(())
    }
}
