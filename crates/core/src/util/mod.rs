//! Cross-cutting utilities that don't belong to a specific subsystem.
//!
//! Helpers here describe behaviour, not the shape of data — anything
//! that models the flow (Schema, Spec, Cursor, FlowState, Context)
//! lives under [`crate::model`] instead.

pub mod concurrency;
pub mod retry;

pub use concurrency::{ConcurrencyManager, FlowLockHandle, log_concurrency_budgets};
pub use retry::retry_transient;
