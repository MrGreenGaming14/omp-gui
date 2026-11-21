//! A string-keyed registry of shared strategy objects, usually trait objects.

use std::collections::BTreeMap;
use std::sync::Arc;

/// Strategies keyed by a stable id, iterated in id order.
pub struct Registry<A: ?Sized> {
    by_id: BTreeMap<String, Arc<A>>,
}

impl<A: ?Sized> Default for Registry<A> {
    fn default() -> Self { Self { by_id: BTreeMap::new() } }
}

impl<A: ?Sized> Clone for Registry<A> {
    fn clone(&self) -> Self { Self { by_id: self.by_id.clone() } }
}

impl<A: ?Sized> Registry<A> {
    pub fn new() -> Self { Self::default() }
    /// Adds `a` under `id`, replacing any earlier entry with that id.
    pub fn register(&mut self, id: impl Into<String>, a: Arc<A>) { self.by_id.insert(id.into(), a); }
    pub fn get(&self, id: &str) -> Option<Arc<A>> { self.by_id.get(id).cloned() }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Arc<A>)> { self.by_id.iter().map(|(id, a)| (id.as_str(), a)) }
    pub fn len(&self) -> usize { self.by_id.len() }
    pub fn is_empty(&self) -> bool { self.by_id.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_looks_up_and_replaces_by_id() {
        let mut r: Registry<dyn Fn() -> u8> = Registry::new();
        for (id, v) in [("b", 2), ("a", 1), ("b", 3)] {
            r.register(id, Arc::new(move || v));
        }
        let seen: Vec<_> = r.iter().map(|(id, f)| (id, f())).collect();
        assert_eq!(seen, [("a", 1), ("b", 3)]);
        assert!(r.get("c").is_none() && r.len() == 2);
    }
}
