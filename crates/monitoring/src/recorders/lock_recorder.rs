use std::sync::Arc;

use crate::integrating_gauge::IntegratingGaugeSlot;

/// Component classification — labels each lock metric so dashboards can
/// split source / sink / storage independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ComponentKind {
    Source,
    Sink,
    Storage,
}

impl ComponentKind {
    pub fn as_label(self) -> &'static str {
        match self {
            ComponentKind::Source => "source",
            ComponentKind::Sink => "sink",
            ComponentKind::Storage => "storage",
        }
    }
}

/// Per-component lock recorder. Tracks runtime semaphore stats as
/// time-integrating gauges: `rate(queue_seconds_integral[window])`
/// gives the time-averaged queue depth, and likewise for active
/// permits. Plain "current value" gauges are not emitted — operators
/// who want a snapshot read the integral at two adjacent scrapes.
/// Configuration (lock max) lives on `MonitoringManager` directly;
/// driver-level pool stats (active/idle/max/min) live on
/// `PoolStatsCollector`.
// No `Default` impl — see `PoolStatsRecorder`'s rationale: a silent
// `Default::default()` would hand back a disabled recorder and hide the
// intent at the call site. Callers use [`Self::disabled`] explicitly.
#[derive(Clone)]
pub struct LockRecorder {
    inner: Option<Arc<LockRecorderInner>>,
}

pub(crate) struct LockRecorderInner {
    pub(crate) lock_queue: IntegratingGaugeSlot,
    pub(crate) lock_active: IntegratingGaugeSlot,
}

impl LockRecorder {
    pub(crate) fn enabled(inner: LockRecorderInner) -> Self {
        Self {
            inner: Some(Arc::new(inner)),
        }
    }

    pub fn disabled() -> Self {
        Self { inner: None }
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    pub fn queue_guard(&self) -> QueueGuard<'_> {
        match &self.inner {
            None => QueueGuard { inner: None },
            Some(arc) => {
                arc.lock_queue.add(1.0);
                QueueGuard {
                    inner: Some(arc.as_ref()),
                }
            }
        }
    }

    pub fn active_guard(&self) -> ActiveGuard<'_> {
        match &self.inner {
            None => ActiveGuard { inner: None },
            Some(arc) => {
                arc.lock_active.add(1.0);
                ActiveGuard {
                    inner: Some(arc.as_ref()),
                }
            }
        }
    }
}

/// RAII queue-depth guard. Borrows the recorder — the compiler ties
/// the guard's lifetime to its parent `LockRecorder` so the slot it
/// decrements on drop cannot dangle.
pub struct QueueGuard<'a> {
    inner: Option<&'a LockRecorderInner>,
}

impl Drop for QueueGuard<'_> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.lock_queue.add(-1.0);
        }
    }
}

pub struct ActiveGuard<'a> {
    inner: Option<&'a LockRecorderInner>,
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.lock_active.add(-1.0);
        }
    }
}
