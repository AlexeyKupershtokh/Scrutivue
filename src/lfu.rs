//! Small bounded LFU cache with deterministic least-recently-used tie breaking.
use std::{collections::HashMap, hash::Hash};

struct Entry<V> {
    value: V,
    uses: u64,
    touched: u64,
}

pub struct Lfu<K, V> {
    entries: HashMap<K, Entry<V>>,
    capacity: usize,
    clock: u64,
}
impl<K: Eq + Hash + Clone, V> Lfu<K, V> {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            entries: HashMap::new(),
            capacity,
            clock: 0,
        }
    }
    pub fn get(&mut self, key: &K) -> Option<&V> {
        let entry = self.entries.get_mut(key)?;
        self.clock = self.clock.saturating_add(1);
        entry.uses = entry.uses.saturating_add(1);
        entry.touched = self.clock;
        Some(&entry.value)
    }
    /// Inspect a snapshot without counting a second hit for the same request.
    pub fn peek(&self, key: &K) -> Option<&V> {
        self.entries.get(key).map(|entry| &entry.value)
    }
    /// Updating progress is not another cache access: frequent publications must
    /// not make a rarely requested pair artificially popular.
    pub fn insert(&mut self, key: K, value: V) {
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.value = value;
            return;
        }
        if self.entries.len() == self.capacity {
            let victim = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| (entry.uses, entry.touched))
                .map(|(key, _)| key.clone())
                .unwrap();
            self.entries.remove(&victim);
        }
        self.clock = self.clock.saturating_add(1);
        self.entries.insert(
            key,
            Entry {
                value,
                uses: 1,
                touched: self.clock,
            },
        );
    }
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frequency_wins_and_oldest_breaks_ties() {
        let mut cache = Lfu::new(2);
        cache.insert(1, "a");
        cache.insert(2, "b");
        cache.get(&1);
        cache.insert(3, "c");
        assert!(cache.get(&2).is_none());
        assert_eq!(cache.get(&1), Some(&"a"));
        assert_eq!(cache.len(), 2);
        let mut cache = Lfu::new(2);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.insert(1, 11); // progress is not a hit
        cache.insert(3, 30);
        assert!(cache.get(&1).is_none());
    }
}
