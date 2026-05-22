//! `PoolStatsReader` wrapper for the mongo driver. Mongo's 3.6 driver
//! doesn't expose pool counts directly; instead the CMAP event
//! handler writes into these atomics on every connection
//! check-in/check-out/close event. Reads are O(1) atomic loads.

use std::sync::atomic::{AtomicU32, Ordering};

use air_elt_monitoring::{PoolConnectionCounts, PoolStatsReader};

pub struct MongoPoolStatsReader {
    active: AtomicU32,
    idle: AtomicU32,
}

impl MongoPoolStatsReader {
    pub fn new() -> Self {
        Self {
            active: AtomicU32::new(0),
            idle: AtomicU32::new(0),
        }
    }

    pub fn on_pool_filled(&self) {
        self.idle.fetch_add(1, Ordering::Relaxed);
    }

    pub fn on_idle_acquired(&self) {
        self.idle
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_sub(1))
            .ok();
        self.active.fetch_add(1, Ordering::Relaxed);
    }

    pub fn on_released_to_idle(&self) {
        self.active
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_sub(1))
            .ok();
        self.idle.fetch_add(1, Ordering::Relaxed);
    }

    pub fn on_closed_from_idle(&self) {
        self.idle
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_sub(1))
            .ok();
    }

    pub fn on_closed_from_active(&self) {
        self.active
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_sub(1))
            .ok();
    }
}

impl Default for MongoPoolStatsReader {
    fn default() -> Self {
        Self::new()
    }
}

impl PoolStatsReader for MongoPoolStatsReader {
    fn read(&self) -> PoolConnectionCounts {
        PoolConnectionCounts {
            active: self.active.load(Ordering::Relaxed),
            idle: self.idle.load(Ordering::Relaxed),
        }
    }
}
