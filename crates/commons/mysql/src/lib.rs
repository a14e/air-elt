//! MySQL-specific SQL helpers shared by all mysql connectors.
//!
//! See `air-elt-commons-pg` for the postgres counterpart. Db-agnostic
//! identifier validation and pool tunables live in `air-elt-commons`.

pub mod identifier;
pub mod mysql_type;
pub mod null_bind;
pub mod pool;
pub mod pool_stats_reader;
pub mod schema;
pub mod sink_bind;

pub use pool_stats_reader::MySqlPoolStatsReader;
