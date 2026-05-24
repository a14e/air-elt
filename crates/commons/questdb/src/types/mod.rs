//! QuestDB `DynType` / `DynValue` registry.
//!
//! Each submodule owns one QuestDB-specific kind. The kinds live behind
//! [`air_elt_core::types::data_type::DataType::Custom`] and
//! [`air_elt_core::types::value::Value::Custom`] so the canonical pivot
//! stays unchanged.

pub mod geohash;
pub mod long256;
pub mod native_kind;
pub mod symbol;

pub use geohash::{QuestDbGeohashType, QuestDbGeohashValue};
pub use long256::{QuestDbLong256Type, QuestDbLong256Value};
pub use native_kind::is_questdb_native_kind;
pub use symbol::{QuestDbSymbolType, QuestDbSymbolValue};
