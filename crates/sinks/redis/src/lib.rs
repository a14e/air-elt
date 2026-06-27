//! Redis / Valkey sink (AIR-5).
//!
//! Sink-only connector that writes batches to redis in one of five
//! per-flow modes — `kv`, `kv-delete`, `list`, `stream`, `pubsub` —
//! over a `deadpool-redis` connection pool (`air-elt-commons-redis`).
//!
//! The write mode is declared per flow via the developed sink form
//! `sink = { name = "...", mode = "..." }`; the connection URL and pool
//! tunables live on the connector `config = { url, pool = { ... } }`.
//!
//! See [`redis_sink`] for the delivery-semantics and conflict-block
//! contract, and [`flow_options`] for the per-mode column contract.

pub mod commands;
pub mod config;
pub mod factory;
pub mod flow_options;
pub mod redis_sink;

pub use config::RedisSinkConfig;
pub use factory::RedisSinkFactory;
pub use flow_options::{RedisFlowOptions, RedisMode};
pub use redis_sink::RedisSink;
