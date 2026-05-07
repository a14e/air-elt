//! Library entrypoint for the air-elt binary.
//!
//! Exposes `App` (config + registry + run/migrate/validate) so the
//! `air-elt` bin and integration tests share a single codegen point.

pub mod app;
pub mod registry;

pub use app::{App, ListedKinds};
