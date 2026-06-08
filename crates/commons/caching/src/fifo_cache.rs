use std::collections::VecDeque;
use std::hash::Hash;
use std::sync::{Arc, RwLock};

use ahash::AHashMap;

/// Inner state guarded by a single `RwLock`. The `map` holds the cached
/// values; the `queue` mirrors the insertion order so eviction can drop
/// the oldest key in O(1).
struct Inner<K, V> {
    map: AHashMap<K, V>,
    queue: VecDeque<K>,
}

/// A bounded thread-safe cache with FIFO eviction. `cap == 0` disables
/// caching entirely (every lookup recomputes). Two access modes:
/// [`get_or_try_insert_with`](Self::get_or_try_insert_with) clones the value
/// out on a hit (store cheap-to-clone values, e.g. `Arc<T>`);
/// [`with_or_try_insert_with`](Self::with_or_try_insert_with) borrows the
/// stored value in place and never clones it (store the value directly, no
/// `Clone` bound — at the cost of holding the read lock across the caller's
/// closure on a hit).
///
/// The shared state lives behind an `Arc`, so the cache is a cheap-to-clone
/// handle: every clone refers to the same underlying store (the idiomatic
/// shared-cache shape — no need to wrap it in an `Arc` at the call site).
///
/// FIFO is deliberate (not LRU): a hit takes only a shared read lock, so
/// concurrent reads of hot entries never contend. LRU would need a write
/// lock on every read to bump recency, serialising the read path. For the
/// expected workload — compiled regex / JSON-path that are almost always
/// constant (folded at compile time), with dynamic source/sink patterns
/// being rare — simple overflow protection is plenty.
pub struct FifoCache<K, V> {
    cap: usize,
    inner: Arc<RwLock<Inner<K, V>>>,
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
            inner: Arc::new(RwLock::new(inner)),
        }
    }
}

// Hand-written so cloning the handle never requires `K: Clone` / `V: Clone`
// (a `#[derive(Clone)]` would impose those bounds). Clones share one store.
impl<K, V> Clone for FifoCache<K, V> {
    fn clone(&self) -> Self {
        Self {
            cap: self.cap,
            inner: Arc::clone(&self.inner),
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

impl<K, V> FifoCache<K, V>
where
    K: Clone + Hash + Eq,
{
    /// Builds the value for `key` on a miss, then runs `use_value` against a
    /// shared reference to the stored value and returns its result. Unlike
    /// [`get_or_try_insert_with`](Self::get_or_try_insert_with) the value is
    /// never cloned out — callers that only read the cached artifact (match a
    /// regex, query a JSON path) avoid both the clone and the `V: Clone` bound,
    /// so the value can be stored directly (no `Arc` wrapper). A hit runs
    /// `use_value` under a shared read lock; with `cap == 0` the value is built,
    /// used, and dropped without being stored.
    ///
    /// Contract for `use_value`: it runs while a lock is held on a hit, so it
    /// must not re-enter this cache (re-entrant read can starve a queued
    /// writer) and should be cheap and non-blocking. A panic inside it on a hit
    /// poisons the lock — acceptable as a fail-fast, matching the rest of this
    /// type. On a race (a concurrent insert of the same key landed first) the
    /// freshly-built value is dropped and the result computed from it is
    /// returned; this is sound only because `build` is a pure function of the
    /// key, so an equal value yields an equal result.
    pub fn with_or_try_insert_with<R, E>(
        &self,
        key: K,
        build: impl FnOnce() -> Result<V, E>,
        use_value: impl FnOnce(&V) -> R,
    ) -> Result<R, E> {
        if self.cap == 0 {
            let value = build()?;
            return Ok(use_value(&value));
        }

        // Fast path: shared read lock, run `use_value` against the reference.
        {
            let guard = self.inner.read().expect("FifoCache inner lock poisoned");
            if let Some(value) = guard.map.get(&key) {
                return Ok(use_value(value));
            }
        }

        // Miss: build and use outside any lock so neither blocks readers. Only
        // the insert below takes the write lock.
        let value = build()?;
        let result = use_value(&value);

        let mut guard = self.inner.write().expect("FifoCache inner lock poisoned");

        // Another thread may have inserted the same key between the read and
        // write locks. Keep its entry and drop ours; the result computed from
        // an equal freshly-built value is still correct.
        if guard.map.contains_key(&key) {
            return Ok(result);
        }

        guard.map.insert(key.clone(), value);
        guard.queue.push_back(key);

        if guard.map.len() > self.cap {
            if let Some(oldest) = guard.queue.pop_front() {
                guard.map.remove(&oldest);
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::convert::Infallible;
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
    fn clone_shares_the_same_store() {
        let cache: FifoCache<&str, i32> = FifoCache::new(4);
        let clone = cache.clone();

        // Insert through one handle.
        cache.get_or_try_insert_with("k", build_ok(7)).unwrap();

        // The clone observes it (shared store): the build returning 999 must
        // not run.
        let via_clone = clone.get_or_try_insert_with("k", build_ok(999)).unwrap();
        assert_eq!(via_clone, 7, "clone must share the underlying cache");
    }

    #[test]
    fn with_or_try_insert_runs_use_value_against_the_reference_and_caches() {
        let calls = AtomicUsize::new(0);
        // Value type is intentionally non-`Clone` to prove no clone happens.
        struct NotClone(i32);
        let cache: FifoCache<&str, NotClone> = FifoCache::new(4);

        let build = |seed: i32| {
            let calls = &calls;
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, Infallible>(NotClone(seed))
            }
        };

        let first = cache
            .with_or_try_insert_with("k", build(7), |value| value.0 + 1)
            .unwrap();
        let second = cache
            .with_or_try_insert_with("k", build(99), |value| value.0 + 1)
            .unwrap();

        assert_eq!(first, 8);
        assert_eq!(second, 8, "second lookup must reuse the cached value");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "build runs only on the miss"
        );
    }

    #[test]
    fn with_or_try_insert_zero_cap_builds_every_time_without_storing() {
        let calls = AtomicUsize::new(0);
        let cache: FifoCache<&str, i32> = FifoCache::new(0);

        for _ in 0..3 {
            let doubled = cache
                .with_or_try_insert_with(
                    "k",
                    || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok::<_, Infallible>(21)
                    },
                    |value| value * 2,
                )
                .unwrap();
            assert_eq!(doubled, 42);
        }

        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "cap==0 rebuilds every call"
        );
    }

    #[test]
    fn with_or_try_insert_propagates_build_error_and_stores_nothing() {
        let cache: FifoCache<&str, i32> = FifoCache::new(4);

        let result =
            cache.with_or_try_insert_with("k", || Err::<i32, &str>("boom"), |value| *value);
        assert_eq!(result, Err("boom"));

        let calls = AtomicUsize::new(0);
        let value = cache
            .with_or_try_insert_with(
                "k",
                || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, Infallible>(5)
                },
                |value| *value,
            )
            .unwrap();
        assert_eq!(value, 5);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn with_or_try_insert_evicts_oldest_after_exceeding_cap() {
        let cache: FifoCache<i32, i32> = FifoCache::new(2);
        let insert = |key: i32, seed: i32| {
            cache
                .with_or_try_insert_with(key, || Ok::<_, Infallible>(seed), |value| *value)
                .unwrap()
        };

        assert_eq!(insert(1, 10), 10);
        assert_eq!(insert(2, 20), 20);
        // Inserting the third key evicts key 1 (oldest).
        assert_eq!(insert(3, 30), 30);

        // Keys 2 and 3 survived — served from cache (build returning 999 must not run).
        assert_eq!(insert(2, 999), 20, "key 2 must still be cached");
        assert_eq!(insert(3, 999), 30, "key 3 must still be cached");

        // Key 1 was evicted: build runs again.
        let calls = AtomicUsize::new(0);
        let reborn = cache
            .with_or_try_insert_with(
                1,
                || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, Infallible>(11)
                },
                |value| *value,
            )
            .unwrap();
        assert_eq!(reborn, 11);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "evicted key must rebuild");
    }

    #[test]
    fn with_or_try_insert_concurrent_access_is_consistent_and_panic_free() {
        const THREADS: usize = 8;
        const KEYS: i32 = 16;
        const ITERATIONS: usize = 500;

        // Hammer the borrow-in-place path on hot keys so the miss-race discard
        // branch (build a value, find a racing insert, return the local result)
        // is exercised under contention.
        let cache: FifoCache<i32, i32> = FifoCache::new(KEYS as usize);

        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let cache = cache.clone();
            let handle = thread::spawn(move || {
                for iteration in 0..ITERATIONS {
                    let key = (iteration as i32) % KEYS;
                    let value = cache
                        .with_or_try_insert_with(
                            key,
                            || Ok::<_, Infallible>(key * 100),
                            |value| *value,
                        )
                        .unwrap();
                    assert_eq!(value, key * 100);
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().expect("worker thread panicked");
        }
    }

    #[test]
    fn concurrent_access_is_consistent_and_panic_free() {
        const THREADS: usize = 8;
        const KEYS: i32 = 16;
        const ITERATIONS: usize = 500;

        // The cache clones into each thread directly — no outer `Arc` needed.
        let cache: FifoCache<i32, i32> = FifoCache::new(KEYS as usize);

        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let cache = cache.clone();
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
