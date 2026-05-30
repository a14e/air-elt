//! Second-pass, whole-program rewrite rules and their driver.
//!
//! See [`engine`] for the [`ProgramPass`](engine::ProgramPass) trait and the
//! driver; the individual modules hold each pass.

mod constant_inliner;
mod engine;
mod field_hoist;
mod register_pruner;

pub(crate) use engine::{SecondPassDriver, SecondPassSet};
