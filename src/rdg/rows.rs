//! Immutable pages shared by snapshots and by the two independently retired Vulkan frame slots.
use std::sync::Arc;
const PAGE: usize = 64;
#[derive(Clone, Debug)]
pub(crate) struct Rows {
    pages: Arc<Vec<Arc<[u8]>>>,
    stride: usize,
    len: usize,
}
impl Rows {
    pub fn new<T: bytemuck::Pod>(records: &[T]) -> Self {
        Self {
            stride: size_of::<T>(),
            len: records.len(),
            pages: Arc::new(
                records
                    .chunks(PAGE)
                    .map(|p| Arc::from(bytemuck::cast_slice(p)))
                    .collect(),
            ),
        }
    }
    pub fn bytes(&self) -> usize {
        self.stride * self.len
    }
    pub fn record(&self, i: usize) -> &[u8] {
        let off = (i % PAGE) * self.stride;
        &self.pages[i / PAGE][off..off + self.stride]
    }
    pub fn set<T: bytemuck::Pod>(&mut self, i: usize, record: &T) {
        let bytes = bytemuck::bytes_of(record);
        assert_eq!(bytes.len(), self.stride);
        let p = i / PAGE;
        let off = i % PAGE * self.stride;
        if &self.pages[p][off..off + self.stride] != bytes {
            let pages = Arc::make_mut(&mut self.pages);
            Arc::make_mut(&mut pages[p])[off..off + self.stride].copy_from_slice(bytes);
        }
    }
    pub fn identical(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.pages, &other.pages)
    }
    pub fn upload(&self, old: Option<&Self>, target: &mut [u8]) -> usize {
        let old = old.filter(|old| old.stride == self.stride && old.len == self.len);
        let mut count = 0;
        for (p, page) in self.pages.iter().enumerate() {
            let prior = old.map(|o| &o.pages[p]);
            if prior.is_some_and(|o| Arc::ptr_eq(o, page)) {
                continue;
            }
            let base = p * PAGE * self.stride;
            if let Some(prior) = prior {
                for (i, (a, b)) in page
                    .chunks_exact(self.stride)
                    .zip(prior.chunks_exact(self.stride))
                    .enumerate()
                {
                    if a != b {
                        let offset = base + i * self.stride;
                        target[offset..offset + self.stride].copy_from_slice(a);
                        count += self.stride;
                    }
                }
            } else {
                target[base..base + page.len()].copy_from_slice(page);
                count += page.len();
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn each_slot_catches_up_from_its_last_retired_version() {
        let a = Rows::new(&[0u32; 130]);
        let mut b = a.clone();
        b.set(0, &19u32);
        b.set(65, &23u32);
        let mut c = b.clone();
        c.set(0, &0u32);
        c.set(129, &41u32);
        let mut first = vec![255; a.bytes()];
        assert_eq!(a.upload(None, &mut first), 520);
        let mut second = first.clone();
        assert_eq!(b.upload(Some(&a), &mut second), 8);
        assert_eq!(c.upload(Some(&a), &mut first), 8);
        assert_eq!(c.upload(Some(&b), &mut second), 8);
        assert_eq!(first, second);
        assert_eq!(c.upload(Some(&c), &mut first), 0);
        let smaller = Rows::new(&[9u32; 2]);
        assert_eq!(smaller.upload(Some(&c), &mut first), 8);
        assert_eq!(&first[..8], bytemuck::cast_slice::<u32, u8>(&[9u32; 2]));
    }
}
