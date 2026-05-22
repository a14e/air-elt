//! MongoDB-specific helpers shared by the mongodb source / sink /
//! storage connectors.
//!
//! Mongo is schemaless, so unlike the SQL commons crates we do not
//! ship a `schema::fetch_schema` helper that reads
//! `information_schema`. Instead, callers infer schemas by sampling N
//! documents — `infer::infer_schema` walks a slice of docs and merges
//! per-field types. The `bson_value` module owns the BSON ↔ canonical
//! `Value` codec, and `path` walks nested documents using
//! `core::mapping::FieldPath`.

pub mod bson_value;
pub mod client;
pub mod identifier;
pub mod infer;
pub mod key_bson;
pub mod path;
pub mod pool_stats_reader;
pub mod sampling;
pub mod task;
pub mod types;
pub mod version;

pub use pool_stats_reader::MongoPoolStatsReader;
pub use types::{
    BsonObjectType, BsonObjectValue, MongoJsType, MongoJsValue, MongoObjectIdType,
    MongoObjectIdValue,
};
