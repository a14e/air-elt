//! Source-side wrapper. The canonical PgType table + `to_internal` live in
//! `commons::sql::pg::pg_type` — imported here as `PgType` and `to_internal`
//! so call sites keep short paths.

pub use air_elt_commons::sql::pg::pg_type::{PgType, to_internal};
