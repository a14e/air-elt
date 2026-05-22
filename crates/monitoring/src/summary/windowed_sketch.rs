use std::collections::VecDeque;
use std::time::{Duration, Instant};

use sketches_ddsketch::{Config, DDSketch};

/// Sliding-window DDSketch built as a ring of fixed-granularity
/// sub-sketches. Each bucket carries the events recorded during one
/// granularity tick (default 1s); buckets older than `window` are
/// evicted lazily on `record` and on `merge_live`.
///
/// Eviction is "older than `now - window`" — a `5s` window with a `1s`
/// granularity keeps at most 6 buckets in steady state (5 fully-live
/// buckets + the currently-filling one).
///
/// Quantiles only — cumulative `count` and `sum` for the Prometheus
/// Summary contract live on the owning `SummarySlotInner`. This struct
/// owns just the windowed quantile material.
pub struct WindowedSketch {
    window: Duration,
    granularity: Duration,
    /// Per-bucket sketches in chronological order: front = oldest,
    /// back = currently-filling.
    buckets: VecDeque<Bucket>,
}

struct Bucket {
    /// Wall-clock start of the bucket (aligned to `granularity`).
    start: Instant,
    sketch: DDSketch,
}

impl WindowedSketch {
    pub fn new(window: Duration, granularity: Duration) -> Self {
        debug_assert!(!window.is_zero());
        debug_assert!(!granularity.is_zero());
        debug_assert!(granularity <= window);
        Self {
            window,
            granularity,
            buckets: VecDeque::new(),
        }
    }

    pub fn record(&mut self, value: f64) {
        let now = Instant::now();
        self.evict(now);
        let bucket = self.current_bucket(now);
        bucket.sketch.add(value);
    }

    /// Merge every live bucket into a fresh sketch. Mutates only for
    /// eviction — does not consume the ring.
    pub fn merge_live(&mut self) -> DDSketch {
        let now = Instant::now();
        self.evict(now);
        let mut merged = DDSketch::new(Config::defaults());
        for bucket in &self.buckets {
            // `DDSketch::merge` only fails when configs differ. All
            // sketches in this ring share `Config::defaults()`, so this
            // path cannot fail; on the off chance the upstream invariant
            // changes we surface the panic locally rather than masking
            // the bug with a silent skip.
            merged
                .merge(&bucket.sketch)
                .expect("ring sketches share one Config");
        }
        merged
    }

    fn evict(&mut self, now: Instant) {
        let cutoff = now.checked_sub(self.window);
        while let Some(front) = self.buckets.front() {
            // A bucket is fully outside the sliding window only once its
            // entire span has aged out — i.e. `start + granularity` is
            // already at or before the cutoff. Dropping it earlier loses
            // tail samples that should still be live.
            if let Some(cutoff) = cutoff {
                if front.start + self.granularity <= cutoff {
                    self.buckets.pop_front();
                    continue;
                }
            }
            break;
        }
    }

    fn current_bucket(&mut self, now: Instant) -> &mut Bucket {
        let needs_new = match self.buckets.back() {
            None => true,
            Some(b) => now.duration_since(b.start) >= self.granularity,
        };
        if needs_new {
            self.buckets.push_back(Bucket {
                start: now,
                sketch: DDSketch::new(Config::defaults()),
            });
        }
        self.buckets
            .back_mut()
            .expect("just pushed a bucket above when missing")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn fresh_ring_is_empty() {
        let mut s = WindowedSketch::new(Duration::from_millis(200), Duration::from_millis(50));
        let sketch = s.merge_live();
        assert!(matches!(sketch.quantile(0.5), Ok(None)));
    }

    #[test]
    fn record_then_merge_reports_quantile() {
        let mut s = WindowedSketch::new(Duration::from_secs(60), Duration::from_secs(1));
        for v in [1.0, 2.0, 3.0, 4.0, 5.0] {
            s.record(v);
        }
        let sketch = s.merge_live();
        let q = sketch.quantile(0.5).unwrap().unwrap();
        assert!((2.0..=4.0).contains(&q), "median was {q}");
    }

    #[test]
    fn evicts_buckets_older_than_window() {
        // A bucket is evicted only after its entire `granularity` span
        // has aged past `window`. Sleep `window + granularity + slack`
        // so the seeded bucket is fully outside the sliding window.
        let window = Duration::from_millis(150);
        let gran = Duration::from_millis(30);
        let mut s = WindowedSketch::new(window, gran);
        s.record(100.0);
        sleep(window + gran + Duration::from_millis(40));
        s.record(1.0);
        let sketch = s.merge_live();
        let q = sketch.quantile(0.5).unwrap().unwrap();
        assert!(q < 50.0, "median dominated by stale value: {q}");
    }

    #[test]
    fn rolls_into_new_bucket_after_granularity() {
        let mut s = WindowedSketch::new(Duration::from_secs(60), Duration::from_millis(20));
        s.record(1.0);
        sleep(Duration::from_millis(50));
        s.record(2.0);
        sleep(Duration::from_millis(50));
        s.record(3.0);
        assert!(
            s.buckets.len() >= 3,
            "expected ≥3 buckets, got {}",
            s.buckets.len()
        );
    }

    #[test]
    fn bucket_tail_samples_survive_until_full_granularity_age_out() {
        // A sample recorded just before the bucket rolls must still be
        // live for the full `window` duration after its bucket starts.
        // With the old `<` predicate the bucket was evicted one
        // granularity early, dropping these tail samples.
        let window = Duration::from_millis(120);
        let gran = Duration::from_millis(40);
        let mut s = WindowedSketch::new(window, gran);
        s.record(42.0);
        // Sleep less than `window` — the sample must still be live even
        // though `record_start + window` has not yet elapsed.
        sleep(window - Duration::from_millis(20));
        let sketch = s.merge_live();
        let q = sketch
            .quantile(0.5)
            .unwrap()
            .expect("recorded sample should still be live");
        assert!((40.0..=44.0).contains(&q), "expected ~42, got {q}");
    }
}
