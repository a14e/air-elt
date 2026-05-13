//! MS SQL-specific helpers shared by all mssql connectors.
//!
//! Uses tiberius (TDS driver) with bb8 connection pooling.
//! sqlx 0.8 does not have an MSSQL backend — it was removed after 0.6
//! and is pending a full rewrite. The MSSQL crates therefore use
//! tiberius directly rather than going through sqlx.
//!
//! See `air-elt-commons-pg` for the postgres counterpart.

pub mod identifier;
pub mod mssql_type;
pub mod pool;
pub mod schema;
pub mod types;
pub mod value_bind;
