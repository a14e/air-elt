//! ClickHouse `DynType`/`DynValue` registry.
//!
//! Each submodule owns one CH-specific kind. The kinds live behind
//! [`DataType::Custom`] / [`Value::Custom`] so the canonical pivot stays
//! unchanged.

pub mod aggregate_state;
pub mod array_;
pub mod enum_;
pub mod fixed_string;
pub mod int128;
pub mod int256;
pub mod ip;
pub mod map_;
pub mod tuple_;
