//! Shared ClickHouse helpers for sink and (future) source connectors.
//!
//! This crate sits between `air-elt-core` and the `clickhouse` sink in
//! the same way `air-elt-commons-mysql` does for the mysql connectors.
//! It owns:
//!
//! * the HTTP client builder ([`client`]),
//! * identifier quoting ([`identifier`]),
//! * the schema introspection (`SELECT … FROM system.columns`) and
//!   ClickHouse type-string parser ([`ch_type_parser`], [`schema`]),
//! * the RowBinary value encoder ([`row_binary`]),
//! * the ClickHouse-specific `DynType` / `DynValue` registry under
//!   [`types`] (`AggregateFunction` states, `IPv4` / `IPv6`,
//!   `FixedString`, `Enum8` / `Enum16`).
//!
//! Structural composite types (`Tuple`, `Array`, `Map`, `Nested`, geo
//! shapes) are mapped onto the canonical [`air_elt_core::types::DataType::Json`]
//! pivot at parse time — they are *structural*, so JSON is a faithful
//! lossless representation. `LowCardinality(T)` is unwrapped (the LC
//! modifier is a CH storage detail). `Nullable(T)` is unwrapped and
//! surfaces on the `Field.nullable` side of the schema.

pub mod ch_type_parser;
pub mod client;
pub mod identifier;
pub mod row_binary;
pub mod schema;
pub mod types;

pub use ch_type_parser::{ParseError, ParsedType, parse_type};
pub use client::{ChClient, ChClientConfig};
