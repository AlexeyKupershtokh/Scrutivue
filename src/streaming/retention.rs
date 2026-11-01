//! Worker-owned LRU for inactive, incomplete sources. Exact frames use Services'
//! separate revision cache. Mappings/native allocations are not a process-RSS cap.
use super::{PhotoId, State};
use std::collections::HashMap;

pub(super) struct Retention {
    clock: u64,
    used: HashMap<PhotoId, u64>,
    bytes: usize,
    entries: usize,
}
impl Default for Retention {
    fn default() -> Self {
        Self {
            clock: 0,
            used: HashMap::new(),
            bytes: 768 * 1024 * 1024,
            entries: 6,
        }
    }
}
impl Retention {
    pub(super) fn touch(&mut self, ids: &[PhotoId]) {
        self.clock += 1;
        for &id in ids {
            self.used.insert(id, self.clock);
        }
    }
    pub(super) fn clear(&mut self) {
        self.used.clear();
    }
    pub(super) fn trim(&mut self, states: &mut HashMap<PhotoId, State>, selected: &[PhotoId]) {
        states.retain(|id, state| {
            if selected.contains(id) {
                return true;
            }
            // Do not pin a second full-frame cache, or retain opaque native
            // decoder workspaces for each historical selection. Completed tiles
            // survive; at most an interrupted/coarse tile may need restarting.
            state.pending = None;
            state.full.is_none() && state.source.tiles.iter().any(Option::is_some)
        });
        self.used.retain(|id, _| states.contains_key(id));
        let mut inactive: Vec<_> = states
            .iter()
            .filter(|(id, _)| !selected.contains(id))
            .map(|(&id, state)| {
                (
                    id,
                    self.used.get(&id).copied().unwrap_or(0),
                    state.tile_bytes(),
                )
            })
            .collect();
        inactive.sort_by_key(|entry| (entry.1, entry.0));
        let mut bytes = inactive.iter().map(|entry| entry.2).sum::<usize>();
        let mut count = inactive.len();
        for (id, _, size) in inactive {
            if bytes <= self.bytes && count <= self.entries {
                break;
            }
            states.remove(&id);
            self.used.remove(&id);
            bytes -= size;
            count -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::tests::image_state;

    #[test]
    fn incomplete_sources_survive_selection_changes_and_pause() {
        let (a, _) = image_state("retained-a", [64, 48], |_, _| [123; 3]);
        let (b, _) = image_state("retained-b", [64, 48], |_, _| [456; 3]);
        let before = a.source.tiles[0].as_ref().unwrap().display.clone();
        let mut states = HashMap::from([(1, a), (2, b)]);
        let mut cache = Retention::default();
        cache.touch(&[1]);
        cache.touch(&[2]);
        cache.trim(&mut states, &[2]);
        cache.trim(&mut states, &[]);
        assert!(std::sync::Arc::ptr_eq(
            &before,
            &states[&1].source.tiles[0].as_ref().unwrap().display
        ));
        assert_eq!(states.len(), 2);
    }

    #[test]
    fn least_recent_inactive_source_is_evicted_without_evicting_selected_pair() {
        let mut states: HashMap<_, _> = (1..=4)
            .map(|id| {
                (
                    id,
                    image_state(&format!("retention-{id}"), [64, 48], |_, _| [0; 3]).0,
                )
            })
            .collect();
        let mut cache = Retention {
            entries: 1,
            bytes: usize::MAX,
            ..Retention::default()
        };
        cache.touch(&[1]);
        cache.touch(&[2]);
        cache.touch(&[3, 4]);
        cache.trim(&mut states, &[3, 4]);
        assert!(!states.contains_key(&1));
        assert!(states.contains_key(&2) && states.contains_key(&3) && states.contains_key(&4));
        cache.bytes = 0;
        cache.trim(&mut states, &[3, 4]);
        assert_eq!(states.len(), 2);
    }

    #[test]
    fn byte_limit_evicts_only_enough_old_inactive_tile_payloads() {
        let mut states: HashMap<_, _> = (1..=3)
            .map(|id| {
                (
                    id,
                    image_state(&format!("retention-bytes-{id}"), [64, 48], |_, _| [0; 3]).0,
                )
            })
            .collect();
        let bytes = states[&1].tile_bytes();
        let mut cache = Retention {
            entries: 6,
            bytes,
            ..Retention::default()
        };
        cache.touch(&[1]);
        cache.touch(&[2]);
        cache.touch(&[3]);
        cache.trim(&mut states, &[3]);
        assert!(!states.contains_key(&1));
        assert!(states.contains_key(&2) && states.contains_key(&3));
        assert_eq!(states[&2].tile_bytes(), bytes);
    }
    #[test]
    fn completed_frames_are_not_pinned_by_the_partial_cache() {
        let (mut a, frame) = image_state("retained-full", [64, 48], |_, _| [0; 3]);
        a.full = Some(frame);
        let mut states = HashMap::from([(1, a)]);
        Retention::default().trim(&mut states, &[]);
        assert!(states.is_empty());
    }
}
