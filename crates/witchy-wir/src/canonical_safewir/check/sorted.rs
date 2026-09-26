//! Ordered maps and sets stored as one sorted run, for the checker's
//! per-program-point facts.
//!
//! [`super::memory::MemoryState`] holds every fact behind `CowFacts`, an
//! `Arc` the checker clones at each edge and then writes through with
//! `Arc::make_mut`. With a `BTreeMap` that write clones a tree: one heap
//! allocation per node, every time. A profile of one admission (BUG-640) put
//! 37.7% of samples inside mimalloc and 25.7% inside `BTreeMap`, and these
//! facts are the bulk of both.
//!
//! The keys are `ValueId`s and small tuples of them, so the runs are short and
//! contiguous. Stored sorted, a clone is one allocation and a memcpy, a lookup
//! is a binary search over cache-resident memory, iteration is a pointer bump,
//! and equality — which the fixpoint asks for at every join — is a single
//! slice comparison instead of a tree walk.
//!
//! Iteration order is ascending by key, exactly as the trees were, because the
//! meet operations and the diagnostics they feed depend on it.

use std::fmt;

/// Ascending iteration over a [`SortedMap`]. Named rather than opaque so the
/// borrow checker knows it needs no drop and can end its borrow early.
pub(crate) type MapIter<'a, K, V> =
    std::iter::Map<std::slice::Iter<'a, (K, V)>, fn(&'a (K, V)) -> (&'a K, &'a V)>;

/// Invariant: `entries` is sorted by key and holds no duplicate key.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SortedMap<K, V> {
    entries: Vec<(K, V)>,
}

impl<K, V> Default for SortedMap<K, V> {
    fn default() -> Self {
        Self { entries: Vec::new() }
    }
}

impl<K: fmt::Debug, V: fmt::Debug> fmt::Debug for SortedMap<K, V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_map().entries(self.entries.iter().map(|(k, v)| (k, v))).finish()
    }
}

#[allow(clippy::len_without_is_empty, reason = "is_empty exists for fixtures only")]
impl<K: Ord, V> SortedMap<K, V> {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Fixture setup only: production facts are dropped with their state.
    #[cfg(feature = "raw-encoder-test-api")]
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    fn locate<Q>(&self, key: &Q) -> Result<usize, usize>
    where
        K: std::borrow::Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.entries.binary_search_by(|(existing, _)| existing.borrow().cmp(key))
    }

    pub(crate) fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.locate(key).ok().map(|index| &self.entries[index].1)
    }

    pub(crate) fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match self.locate(key) {
            Ok(index) => Some(&mut self.entries[index].1),
            Err(_) => None,
        }
    }

    pub(crate) fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.locate(key).is_ok()
    }

    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<V> {
        match self.locate(&key) {
            Ok(index) => Some(std::mem::replace(&mut self.entries[index].1, value)),
            Err(index) => {
                self.entries.insert(index, (key, value));
                None
            }
        }
    }

    pub(crate) fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match self.locate(key) {
            Ok(index) => Some(self.entries.remove(index).1),
            Err(_) => None,
        }
    }

    pub(crate) fn entry(&mut self, key: K) -> Entry<'_, K, V> {
        let found = self.locate(&key);
        Entry { map: self, key, found }
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = &K> {
        self.entries.iter().map(|(key, _)| key)
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.iter().map(|(_, value)| value)
    }

    pub(crate) fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.entries.iter_mut().map(|(_, value)| value)
    }

    pub(crate) fn iter(&self) -> MapIter<'_, K, V> {
        fn split<K, V>(entry: &(K, V)) -> (&K, &V) {
            (&entry.0, &entry.1)
        }
        self.entries.iter().map(split as fn(&(K, V)) -> (&K, &V))
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&K, &mut V) -> bool) {
        self.entries.retain_mut(|(key, value)| keep(key, value));
    }

}

pub(crate) struct Entry<'a, K, V> {
    map: &'a mut SortedMap<K, V>,
    key: K,
    found: Result<usize, usize>,
}

impl<'a, K: Ord, V> Entry<'a, K, V> {
    pub(crate) fn or_insert_with(self, make: impl FnOnce() -> V) -> &'a mut V {
        let index = match self.found {
            Ok(index) => index,
            Err(index) => {
                self.map.entries.insert(index, (self.key, make()));
                index
            }
        };
        &mut self.map.entries[index].1
    }

    pub(crate) fn or_default(self) -> &'a mut V
    where
        V: Default,
    {
        self.or_insert_with(V::default)
    }
}

impl<K: Ord, V> FromIterator<(K, V)> for SortedMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(items: I) -> Self {
        let mut entries: Vec<(K, V)> = items.into_iter().collect();
        // Later duplicates win. Stable sorting preserves their arrival order;
        // dedup_by passes the later entry first and removes it on true, so swap
        // just duplicates to retain the complete last pair. Unique runs need
        // no movement, unlike reversing the whole run before and after dedup.
        entries.sort_by(|(left, _), (right, _)| left.cmp(right));
        entries.dedup_by(|later, retained| {
            if later.0 == retained.0 {
                std::mem::swap(later, retained);
                true
            } else {
                false
            }
        });
        Self { entries }
    }
}

impl<K: Ord, V, const N: usize> From<[(K, V); N]> for SortedMap<K, V> {
    fn from(entries: [(K, V); N]) -> Self {
        entries.into_iter().collect()
    }
}

impl<K: Ord, V> std::ops::Index<&K> for SortedMap<K, V> {
    type Output = V;
    fn index(&self, key: &K) -> &V {
        self.get(key).expect("no entry found for key")
    }
}

impl<K: Ord, V> Extend<(K, V)> for SortedMap<K, V> {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, items: I) {
        for (key, value) in items {
            self.insert(key, value);
        }
    }
}

impl<'a, K, V> IntoIterator for &'a mut SortedMap<K, V> {
    type Item = (&'a K, &'a mut V);
    type IntoIter =
        std::iter::Map<std::slice::IterMut<'a, (K, V)>, fn(&'a mut (K, V)) -> (&'a K, &'a mut V)>;
    fn into_iter(self) -> Self::IntoIter {
        fn split<K, V>(entry: &mut (K, V)) -> (&K, &mut V) {
            (&entry.0, &mut entry.1)
        }
        self.entries.iter_mut().map(split as fn(&mut (K, V)) -> (&K, &mut V))
    }
}

impl<K, V> IntoIterator for SortedMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::vec::IntoIter<(K, V)>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<'a, K, V> IntoIterator for &'a SortedMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = std::iter::Map<std::slice::Iter<'a, (K, V)>, fn(&'a (K, V)) -> (&'a K, &'a V)>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter().map(|(key, value)| (key, value))
    }
}

/// Invariant: `items` is sorted and holds no duplicate.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SortedSet<T> {
    items: Vec<T>,
}

impl<T> Default for SortedSet<T> {
    fn default() -> Self {
        Self { items: Vec::new() }
    }
}

impl<T: fmt::Debug> fmt::Debug for SortedSet<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_set().entries(self.items.iter()).finish()
    }
}

impl<T: Ord> SortedSet<T> {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(crate) fn contains<Q>(&self, item: &Q) -> bool
    where
        T: std::borrow::Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.items.binary_search_by(|existing| existing.borrow().cmp(item)).is_ok()
    }

    pub(crate) fn insert(&mut self, item: T) -> bool {
        match self.items.binary_search(&item) {
            Ok(_) => false,
            Err(index) => {
                self.items.insert(index, item);
                true
            }
        }
    }

    pub(crate) fn remove<Q>(&mut self, item: &Q) -> bool
    where
        T: std::borrow::Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match self.items.binary_search_by(|existing| existing.borrow().cmp(item)) {
            Ok(index) => {
                self.items.remove(index);
                true
            }
            Err(_) => false,
        }
    }

    pub(crate) fn iter(&self) -> std::slice::Iter<'_, T> {
        self.items.iter()
    }

    pub(crate) fn retain(&mut self, keep: impl FnMut(&T) -> bool) {
        self.items.retain(keep);
    }

    pub(crate) fn first(&self) -> Option<&T> {
        self.items.first()
    }

    /// Both sides are sorted runs, so the answer is one merge, not a lookup
    /// per element.
    pub(crate) fn intersection<'a>(&'a self, other: &'a Self) -> impl Iterator<Item = &'a T> {
        let mut left = 0;
        let mut right = 0;
        std::iter::from_fn(move || {
            while left < self.items.len() && right < other.items.len() {
                match self.items[left].cmp(&other.items[right]) {
                    std::cmp::Ordering::Less => left += 1,
                    std::cmp::Ordering::Greater => right += 1,
                    std::cmp::Ordering::Equal => {
                        left += 1;
                        right += 1;
                        return Some(&self.items[left - 1]);
                    }
                }
            }
            None
        })
    }

}

impl<T: Ord> FromIterator<T> for SortedSet<T> {
    fn from_iter<I: IntoIterator<Item = T>>(items: I) -> Self {
        let mut items: Vec<T> = items.into_iter().collect();
        items.sort();
        items.dedup();
        Self { items }
    }
}

impl<T: Ord, const N: usize> From<[T; N]> for SortedSet<T> {
    fn from(items: [T; N]) -> Self {
        items.into_iter().collect()
    }
}

/// Mirrors `BTreeSet`'s borrowing `Extend`, so a set of `Copy` keys can absorb
/// another set without cloning it first.
impl<'a, T: Ord + Copy + 'a> Extend<&'a T> for SortedSet<T> {
    fn extend<I: IntoIterator<Item = &'a T>>(&mut self, items: I) {
        for item in items {
            self.insert(*item);
        }
    }
}

impl<T: Ord> Extend<T> for SortedSet<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, items: I) {
        for item in items {
            self.insert(item);
        }
    }
}

impl<T> IntoIterator for SortedSet<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a SortedSet<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}
