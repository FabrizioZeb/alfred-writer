//! Exact-text -> issues memoization so retyping/undoing/refocusing back to text we've
//! already checked never spawns another `claude` subprocess. See the cache row in the
//! "Performance/cost architecture" table in ARCHITECTURE.md — this is one of four gates
//! that exist together on purpose; don't remove it in isolation.

use crate::providers::Issue;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

const CACHE_CAP: usize = 40;

pub(super) type IssueCache = Arc<Mutex<(HashMap<String, Vec<Issue>>, VecDeque<String>)>>;

/// Creates an empty, shareable cache (map + FIFO eviction queue behind one `Mutex`).
pub(super) fn new_cache() -> IssueCache {
    Arc::new(Mutex::new((HashMap::new(), VecDeque::new())))
}

/// Looks up previously-cached issues for the exact text `key`.
///
/// Returns:
/// `Some(issues)` on a cache hit (clones the stored `Vec`), `None` on a miss.
pub(super) fn cache_get(cache: &IssueCache, key: &str) -> Option<Vec<Issue>> {
    cache.lock().unwrap().0.get(key).cloned()
}

/// Stores `issues` for exact text `key`. Re-inserting an existing `key` overwrites its
/// value without duplicating its eviction-queue slot. Once the cache holds more than
/// [`CACHE_CAP`] distinct keys, the oldest (FIFO, not LRU) is evicted.
pub(super) fn cache_insert(cache: &IssueCache, key: String, issues: Vec<Issue>) {
    let mut guard = cache.lock().unwrap();
    if !guard.0.contains_key(&key) {
        guard.1.push_back(key.clone());
        while guard.1.len() > CACHE_CAP {
            if let Some(old) = guard.1.pop_front() {
                guard.0.remove(&old);
            }
        }
    }
    guard.0.insert(key, issues);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(original: &str) -> Issue {
        Issue {
            original: original.to_string(),
            suggestion: format!("{original}-fixed"),
            explanation: "because".to_string(),
        }
    }

    #[test]
    fn miss_on_empty_cache() {
        let cache = new_cache();
        assert!(cache_get(&cache, "hello").is_none());
    }

    #[test]
    fn insert_then_get_round_trips() {
        let cache = new_cache();
        cache_insert(&cache, "hello".to_string(), vec![issue("hello")]);
        let got = cache_get(&cache, "hello").expect("should hit");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].original, "hello");
    }

    #[test]
    fn distinct_keys_do_not_collide() {
        let cache = new_cache();
        cache_insert(&cache, "a".to_string(), vec![issue("a")]);
        cache_insert(&cache, "b".to_string(), vec![issue("b")]);
        assert_eq!(cache_get(&cache, "a").unwrap()[0].original, "a");
        assert_eq!(cache_get(&cache, "b").unwrap()[0].original, "b");
    }

    #[test]
    fn reinserting_same_key_overwrites_value_without_growing_queue() {
        let cache = new_cache();
        cache_insert(&cache, "a".to_string(), vec![issue("a")]);
        cache_insert(&cache, "a".to_string(), vec![]);
        let got = cache_get(&cache, "a").unwrap();
        assert!(got.is_empty(), "second insert should have replaced the value");

        // The key should only occupy one eviction-queue slot: filling the cache to
        // capacity with distinct keys after repeatedly reinserting "a" should not evict
        // "a" early because of phantom duplicate queue entries.
        for i in 0..CACHE_CAP - 1 {
            cache_insert(&cache, format!("k{i}"), vec![]);
        }
        assert!(cache_get(&cache, "a").is_some(), "\"a\" should not have been evicted");
    }

    #[test]
    fn evicts_oldest_entry_once_over_capacity() {
        let cache = new_cache();
        for i in 0..CACHE_CAP {
            cache_insert(&cache, format!("k{i}"), vec![issue(&format!("k{i}"))]);
        }
        assert!(cache_get(&cache, "k0").is_some());

        // One more insert pushes us over capacity; the oldest key ("k0") should be evicted.
        cache_insert(&cache, "new".to_string(), vec![issue("new")]);
        assert!(cache_get(&cache, "k0").is_none(), "oldest entry should have been evicted");
        assert!(cache_get(&cache, "new").is_some());
        assert!(cache_get(&cache, "k1").is_some(), "second-oldest entry should survive");
    }
}
