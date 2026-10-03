//! Structural sharing keeps proposals independent without copying old values.
use std::{borrow::Borrow, sync::Arc};

#[derive(Clone, Debug)]
pub(crate) struct Map<K: Ord + Clone, V: Clone>(im::OrdMap<K, Arc<V>>);

impl<K: Ord + Clone, V: Clone> Map<K, V> {
    pub(crate) fn new() -> Self {
        Self(im::OrdMap::new())
    }
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }
    pub(crate) fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.0.get(key).map(AsRef::as_ref)
    }
    pub(crate) fn insert(&mut self, key: K, value: V) {
        self.0.insert(key, Arc::new(value));
    }
    pub(crate) fn remove<Q: Ord + ?Sized>(&mut self, key: &Q)
    where
        K: Borrow<Q>,
    {
        self.0.remove(key);
    }
    pub(crate) fn values(&self) -> impl Iterator<Item = &V> {
        self.0.values().map(AsRef::as_ref)
    }
}
