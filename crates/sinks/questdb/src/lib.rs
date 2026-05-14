//! QuestDB sink — pg-wire writer.
//!
//! All writes go through QuestDB's Postgres-wire surface. INSERTs are
//! chunked to stay under QuestDB 8.2.3's `parameterCount=-2` bug at
//! ~9_300 bound params; see [`pg_writer::QDB_PG_MAX_BIND_PARAMS`].
//!
//! ## WAL-apply visibility
//!
//! QuestDB applies WAL writes asynchronously. Even after `write_batch`
//! returns `Ok`, a subsequent pg-wire `SELECT` may not see the freshly
//! ingested rows for ~hundreds of milliseconds — read-your-write is
//! NOT guaranteed across calls. Operators querying QuestDB right after
//! a flow tick should expect a brief lag.
//! See: <https://community.questdb.com/t/how-to-await-for-rows-ingested-to-wal-table-to-become-visible-in-questdb/48>
//!
//! Public surface kept narrow on purpose — `QuestDbSink`, the factory,
//! and the config struct are the only consumers of this crate.

pub mod config;
pub mod factory;
pub mod pg_writer;
pub mod questdb_sink;

mod sql_statements;

pub use config::QuestDbSinkConfig;
pub use factory::QuestDbSinkFactory;
pub use questdb_sink::{QuestDbSink, QuestDbSinkCtx, type_supported};
