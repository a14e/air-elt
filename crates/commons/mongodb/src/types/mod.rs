//! MongoDB-specific custom types plugged into the canonical
//! [`air_elt_core::types::DataType`] / [`air_elt_core::types::Value`]
//! enums via the [`air_elt_core::types::dynamic::DynType`] /
//! [`air_elt_core::types::dynamic::DynValue`] traits.
//!
//! - [`object_id`] — `Bson::ObjectId` round-trip preserving the BSON
//!   variant (instead of being flattened to a 12-byte `Bytes`).
//! - [`js`] — `Bson::JavaScriptCode` round-trip.
//! - [`bson_object`] — whole-document raw passthrough used by the
//!   wildcard mapping (`mapping = ["*"]`) when both source and sink
//!   are schemaless (Mongo→Mongo).
//!
//! All of these live here rather than in `core::types` because they
//! are Mongo-only — bloating the canonical enum with vendor-specific
//! variants is exactly what the dynamic-types extension point exists
//! to avoid.

pub mod bson_object;
pub mod js;
pub mod object_id;

pub use bson_object::{BsonObjectType, BsonObjectValue};
pub use js::{MongoJsType, MongoJsValue};
pub use object_id::{MongoObjectIdType, MongoObjectIdValue};
