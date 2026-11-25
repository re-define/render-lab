//! Immutable pages shared by snapshots and by the two independently retired Vulkan frame slots.
use std::sync::Arc;
const PAGE: usize = 64;
#[derive(Clone, Debug)]
pub(crate) struct Rows {
    pages: Arc<Vec<Arc<[u8]>>>,
    stride: usize,
    records_per_page: usize,
    len: usize,
}
impl Rows {
    pub fn new<T: bytemuck::Pod>(records: &[T]) -> Self {
        Self {
            stride: size_of::<T>(),
            records_per_page: PAGE,
            len: records.len(),
            pages: Arc::new(
                records
                    .chunks(PAGE)
                    .map(|p| Arc::from(bytemuck::cast_slice(p)))
                    .collect(),
            ),
        }
    }
    /// Word-addressable payload with 4 KiB copy-on-write pages.
    pub fn words() -> Self {
        Self {
            pages: Default::default(),
            stride: 4,
            len: 0,
            records_per_page: 1024,
        }
    }
    pub fn version(&self) -> usize {
        Arc::as_ptr(&self.pages) as usize
    }
    pub fn resize_words(&mut self, words: usize) {
        assert_eq!(self.stride, 4);
        if self.len == words {
            return;
        }
        let pages = Arc::make_mut(&mut self.pages);
        let count = words.div_ceil(self.records_per_page);
        pages.truncate(count);
        let full = self.records_per_page * 4;
        let existing = pages.len();
        if let Some(last) = pages.last_mut() {
            let records =
                (words - (existing - 1) * self.records_per_page).min(self.records_per_page);
            if last.len() != records * 4 {
                let mut bytes = last.to_vec();
                bytes.resize(records * 4, 0);
                *last = bytes.into();
            }
        }
        while pages.len() < count {
            let n = (words * 4 - pages.len() * full).min(full);
            pages.push(vec![0; n].into());
        }
        self.len = words;
    }
    /// Replace a word-aligned range. Unchanged pages keep their identity across snapshots.
    pub fn write_words(&mut self, offset: usize, bytes: &[u8]) -> usize {
        assert_eq!(self.stride, 4);
        assert!(bytes.len().is_multiple_of(4) && offset * 4 + bytes.len() <= self.bytes());
        let mut source = bytes;
        let mut pos = offset * 4;
        let mut changed = 0;
        let page_bytes = self.records_per_page * self.stride;
        while !source.is_empty() {
            let page = pos / page_bytes;
            let start = pos % page_bytes;
            let n = source.len().min(page_bytes - start);
            if self.pages[page][start..start + n] != source[..n] {
                let pages = Arc::make_mut(&mut self.pages);
                Arc::make_mut(&mut pages[page])[start..start + n].copy_from_slice(&source[..n]);
                changed += n;
            }
            pos += n;
            source = &source[n..];
        }
        changed
    }
    pub fn bytes(&self) -> usize {
        self.stride * self.len
    }
    pub fn record(&self, i: usize) -> &[u8] {
        let off = (i % self.records_per_page) * self.stride;
        &self.pages[i / self.records_per_page][off..off + self.stride]
    }
    pub fn set<T: bytemuck::Pod>(&mut self, i: usize, record: &T) {
        let bytes = bytemuck::bytes_of(record);
        assert_eq!(bytes.len(), self.stride);
        let p = i / self.records_per_page;
        let off = i % self.records_per_page * self.stride;
        if &self.pages[p][off..off + self.stride] != bytes {
            let pages = Arc::make_mut(&mut self.pages);
            Arc::make_mut(&mut pages[p])[off..off + self.stride].copy_from_slice(bytes);
        }
    }
    pub fn identical(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.pages, &other.pages)
    }
    pub fn upload(&self, old: Option<&Self>, target: &mut [u8]) -> usize {
        let old = old.filter(|old| {
            old.stride == self.stride && old.records_per_page == self.records_per_page
        });
        let mut count = 0;
        for (p, page) in self.pages.iter().enumerate() {
            let prior = old.and_then(|o| o.pages.get(p));
            if prior.is_some_and(|o| Arc::ptr_eq(o, page)) {
                continue;
            }
            let base = p * self.records_per_page * self.stride;
            if let Some(prior) = prior {
                let common = page.len().min(prior.len());
                for (i, (a, b)) in page[..common]
                    .chunks_exact(self.stride)
                    .zip(prior[..common].chunks_exact(self.stride))
                    .enumerate()
                {
                    if a != b {
                        let offset = base + i * self.stride;
                        target[offset..offset + self.stride].copy_from_slice(a);
                        count += self.stride;
                    }
                }
                if common < page.len() {
                    target[base + common..base + page.len()].copy_from_slice(&page[common..]);
                    count += page.len() - common;
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
    fn paged_payload_preserves_prefix_and_tracks_sparse_replacements() {
        let mut a = Rows::words();
        a.resize_words(1250);
        a.write_words(1022, &[31; 28]);
        let mut b = a.clone();
        b.resize_words(1201);
        b.resize_words(1537);
        assert_eq!(b.record(1024), &[31; 4]);
        assert_eq!(b.record(1536), &[0; 4]);
        let mut last = b.clone();
        last.write_words(1024, &[19; 4]);
        let mut mapped = vec![0; last.bytes()];
        b.upload(None, &mut mapped);
        assert_eq!(last.upload(Some(&b), &mut mapped), 4);
        assert_eq!(b.record(1024), &[31; 4]);
        assert_eq!(&mapped[4096..4100], &[19; 4]);
        b.resize_words(0);
        assert_eq!(b.bytes(), 0);
    }
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
