//! QuestDB `DynType` / `DynValue` registry.
//!
//! Each submodule owns one QuestDB-specific kind. The kinds live behind
//! [`air_elt_core::types::data_type::DataType::Custom`] and
//! [`air_elt_core::types::value::Value::Custom`] so the canonical pivot
//! stays unchanged.

pub mod geohash;
pub mod ipv4;
pub mod long256;
pub mod symbol;

pub use geohash::{QuestDbGeohashType, QuestDbGeohashValue};
pub use ipv4::{QuestDbIpv4Type, QuestDbIpv4Value};
pub use long256::{QuestDbLong256Type, QuestDbLong256Value};
pub use symbol::{QuestDbSymbolType, QuestDbSymbolValue};

/// `true` when `kind` matches one of QuestDB's native custom kinds
/// (`questdb.symbol`, `questdb.long256`, `questdb.ipv4`, `questdb.geohash`).
///
/// Shared by sink type-gate and the pg-wire NULL bind path so the four
/// recognised kinds stay enumerated in exactly one place.
pub fn is_questdb_native_kind(kind: &str) -> bool {
    kind == QuestDbSymbolType::KIND
        || kind == QuestDbLong256Type::KIND
        || kind == QuestDbIpv4Type::KIND
        || kind == QuestDbGeohashType::KIND
}
