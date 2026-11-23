use super::*;
use std::sync::OnceLock;
const PAGE: usize = 64;
/// Immutable snapshots share pages. The contiguous public view is materialized only for callers
/// of the slice API. Internal traversal, lookup and delta extraction stay paged.
#[derive(Default)]
pub(crate) struct InstancePages {
    layout: Arc<()>,
    pages: Vec<Arc<Vec<Arc<RenderInstance>>>>,
    len: usize,
    flat: OnceLock<Vec<Arc<RenderInstance>>>,
}
impl Clone for InstancePages {
    fn clone(&self) -> Self {
        Self {
            layout: self.layout.clone(),
            pages: self.pages.clone(),
            len: self.len,
            flat: OnceLock::new(),
        }
    }
}
impl InstancePages {
    pub(super) fn new(instances: Vec<Arc<RenderInstance>>) -> Self {
        Self {
            len: instances.len(),
            pages: instances
                .chunks(PAGE)
                .map(|p| Arc::new(p.to_vec()))
                .collect(),
            ..Self::default()
        }
    }
    pub(crate) fn same_layout(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.layout, &other.layout)
    }
    pub(crate) fn len(&self) -> usize {
        self.len
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Arc<RenderInstance>> {
        self.pages.iter().flat_map(|p| p.iter())
    }
    pub(super) fn flat(&self) -> &[Arc<RenderInstance>] {
        self.flat.get_or_init(|| self.iter().cloned().collect())
    }
    pub(super) fn edit(&mut self, index: usize) -> &mut Arc<RenderInstance> {
        self.flat.take();
        &mut Arc::make_mut(&mut self.pages[index / PAGE])[index % PAGE]
    }
    pub(crate) fn changed(&self, old: &Self) -> Vec<usize> {
        if !self.same_layout(old) {
            return (0..self.len).collect();
        }
        let mut indices = Vec::new();
        for (n, (a, b)) in self.pages.iter().zip(&old.pages).enumerate() {
            if !Arc::ptr_eq(a, b) {
                indices.extend(
                    a.iter()
                        .zip(b.iter())
                        .enumerate()
                        .filter_map(|(i, (x, y))| (!Arc::ptr_eq(x, y)).then_some(n * PAGE + i)),
                );
            }
        }
        indices
    }
    pub(crate) fn binary_search_by_key<K: Ord>(
        &self,
        k: &K,
        mut key: impl FnMut(&Arc<RenderInstance>) -> K,
    ) -> std::result::Result<usize, usize> {
        let mut lo = 0;
        let mut hi = self.len;
        while lo < hi {
            let mid = (hi + lo) / 2;
            match key(&self[mid]).cmp(k) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Ok(mid),
            }
        }
        Err(lo)
    }
}
impl std::ops::Index<usize> for InstancePages {
    type Output = Arc<RenderInstance>;
    fn index(&self, i: usize) -> &Self::Output {
        &self.pages[i / PAGE][i % PAGE]
    }
}
