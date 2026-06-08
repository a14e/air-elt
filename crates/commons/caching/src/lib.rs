//! Thread-safe FIFO cache for compiled artifacts (regex, JSON-path, …).
pub mod fifo_cache;

pub use fifo_cache::FifoCache;
