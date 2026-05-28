use std::collections::VecDeque;
use std::hash::Hash;
use std::sync::RwLock;

use ahash::AHashMap;

/// Inner state guarded by a single `RwLock`. The `map` holds the cached
/// values; the `queue` mirrors the insertion order so eviction can drop
/// the oldest key in O(1).
struct Inner<K, V> {
    map: AHashMap<K, V>,
    queue: VecDeque<K>,
}

/// A bounded thread-safe cache with FIFO eviction. `cap == 0` disables
/// caching entirely (every lookup recomputes). Values are cloned out on
/// hit, so callers should store cheap-to-clone values (e.g. `Arc<T>`).
pub struct FifoCache<K, V> {
    cap: usize,
    inner: RwLock<Inner<K, V>>,
}

impl<K, V> FifoCache<K, V> {
    /// Builds a cache holding at most `cap` entries. `cap == 0` turns the
    /// cache into a pass-through: nothing is ever stored.
    pub fn new(cap: usize) -> Self {
        let inner = Inner {
            map: AHashMap::new(),
            queue: VecDeque::new(),
        };
        Self {
            cap,
            inner: RwLock::new(inner),
        }
    }
}

impl<K, V> FifoCache<K, V>
where
    K: Clone + Hash + Eq,
    V: Clone,
{
    /// Returns the cached value for `key`, building and inserting it on a
    /// miss. The closure runs only on a miss; its error is propagated
    /// unchanged and nothing is stored. With `cap == 0` the closure runs
    /// on every call and the result is never cached.
    pub fn get_or_try_insert_with<E>(
        &self,
        key: K,
        build: impl FnOnce() -> Result<V, E>,
    ) -> Result<V, E> {
        if self.cap == 0 {
            return build();
        }

        // Fast path: shared read lock, clone out on hit.
        {
            let guard = self.inner.read().expect("FifoCache inner lock poisoned");
            if let Some(value) = guard.map.get(&key) {
                return Ok(value.clone());
            }
        }

        // Miss: build outside any lock so a slow build does not block readers.
        let value = build()?;

        let mut guard = self.inner.write().expect("FifoCache inner lock poisoned");

        // Another thread may have inserted the same key between the read
        // and write locks. Re-check and reuse its value; do not push a
        // duplicate key onto the FIFO queue.
        if let Some(existing) = guard.map.get(&key) {
            return Ok(existing.clone());
        }

        guard.map.insert(key.clone(), value.clone());
        guard.queue.push_back(key);

        if guard.map.len() > self.cap {
            if let Some(oldest) = guard.queue.pop_front() {
                guard.map.remove(&oldest);
            }
        }

        Ok(value)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::convert::Infallible;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use super::*;

    fn build_ok(value: i32) -> impl FnOnce() -> Result<i32, Infallible> {
        move || Ok(value)
    }

    #[test]
    fn hit_returns_cached_value_without_rebuilding() {
        let calls = AtomicUsize::new(0);
        let cache: FifoCache<&str, i32> = FifoCache::new(4);

        let first = cache
            .get_or_try_insert_with("k", || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, Infallible>(7)
            })
            .unwrap();
        let second = cache
            .get_or_try_insert_with("k", || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, Infallible>(99)
            })
            .unwrap();

        assert_eq!(first, 7);
        assert_eq!(second, 7, "second lookup must reuse the cached value");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "build runs only on the miss"
        );
    }

    #[test]
    fn zero_cap_never_caches_and_builds_every_time() {
        let calls = AtomicUsize::new(0);
        let cache: FifoCache<&str, i32> = FifoCache::new(0);

        for _ in 0..3 {
            let value = cache
                .get_or_try_insert_with("k", || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, Infallible>(1)
                })
                .unwrap();
            assert_eq!(value, 1);
        }

        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "cap==0 rebuilds on every call"
        );
    }

    #[test]
    fn error_is_propagated_and_nothing_is_stored() {
        let cache: FifoCache<&str, i32> = FifoCache::new(4);

        let result = cache.get_or_try_insert_with("k", || Err::<i32, &str>("boom"));
        assert_eq!(result, Err("boom"));

        // A later successful build must run (the failed attempt cached nothing).
        let calls = AtomicUsize::new(0);
        let value = cache
            .get_or_try_insert_with("k", || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, Infallible>(5)
            })
            .unwrap();
        assert_eq!(value, 5);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fifo_eviction_drops_the_oldest_after_exceeding_cap() {
        let cache: FifoCache<i32, i32> = FifoCache::new(2);

        cache.get_or_try_insert_with(1, build_ok(10)).unwrap();
        cache.get_or_try_insert_with(2, build_ok(20)).unwrap();
        // Inserting the third key evicts key 1 (oldest).
        cache.get_or_try_insert_with(3, build_ok(30)).unwrap();

        // Keys 2 and 3 survived the eviction — they are served from cache
        // (the build closure returning 999 must NOT run).
        let still_two = cache.get_or_try_insert_with(2, build_ok(999)).unwrap();
        let still_three = cache.get_or_try_insert_with(3, build_ok(999)).unwrap();
        assert_eq!(still_two, 20, "key 2 must still be cached");
        assert_eq!(still_three, 30, "key 3 must still be cached");

        // Key 1 was evicted: build runs again and we observe the fresh value.
        let calls = AtomicUsize::new(0);
        let reborn = cache
            .get_or_try_insert_with(1, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, Infallible>(11)
            })
            .unwrap();
        assert_eq!(reborn, 11);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "evicted key must rebuild");
    }

    #[test]
    fn reaccess_does_not_reset_fifo_order() {
        let cache: FifoCache<i32, i32> = FifoCache::new(2);

        cache.get_or_try_insert_with(1, build_ok(10)).unwrap();
        cache.get_or_try_insert_with(2, build_ok(20)).unwrap();
        // Re-access key 1 — FIFO (not LRU) means this does NOT protect it.
        cache.get_or_try_insert_with(1, build_ok(999)).unwrap();
        // Inserting key 3 still evicts key 1 (the oldest inserted).
        cache.get_or_try_insert_with(3, build_ok(30)).unwrap();

        let calls = AtomicUsize::new(0);
        let reborn = cache
            .get_or_try_insert_with(1, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, Infallible>(11)
            })
            .unwrap();
        assert_eq!(reborn, 11);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "re-access must not protect under FIFO"
        );
    }

    #[test]
    fn concurrent_access_is_consistent_and_panic_free() {
        const THREADS: usize = 8;
        const KEYS: i32 = 16;
        const ITERATIONS: usize = 500;

        let cache: Arc<FifoCache<i32, i32>> = Arc::new(FifoCache::new(KEYS as usize));

        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let cache = Arc::clone(&cache);
            let handle = thread::spawn(move || {
                for iteration in 0..ITERATIONS {
                    let key = (iteration as i32) % KEYS;
                    let value = cache
                        .get_or_try_insert_with(key, || Ok::<_, Infallible>(key * 100))
                        .unwrap();
                    // Whoever built the entry, the value is bound to the key.
                    assert_eq!(value, key * 100);
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().expect("worker thread panicked");
        }
    }
}
