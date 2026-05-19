//! Canonical type model for Air Elt: `DataType`, `Value`, the N+N
//! conversion matrix, default-literal parsing, JSON encoding, and the
//! `DynType` / `DynValue` extension traits.
//!
//! Foundational crate. MUST NOT depend on any other `air-elt-*` crate.
//! `air-elt-core` and the connector commons crates depend on this one,
//! never the other way around. Backend-specific custom-type impls
//! (`mongodb.object_id`, `postgresql.hll`, …) live in
//! `commons-{backend}`, not here.

pub mod convert;
pub mod data_type;
pub mod default_value;
pub mod dynamic;
pub mod error;
pub mod json_encode;
pub mod matrix;
pub mod union_types;
pub mod value;

pub use convert::{ConversionContext, ConvertError, convert};
pub use data_type::DataType;
pub use dynamic::{DynType, DynValue};
pub use json_encode::value_to_json;
pub use union_types::collapse_union;
pub use value::Value;
