//! QuestDB identifier quoting — SQL-92 standard double-quote convention,
//! shared with Postgres. The implementation lives in
//! `air_elt_commons::pg_identifier`; this module is a thin re-export so
//! existing callers keep their `air_elt_commons_questdb::identifier::…`
//! import paths.
//!
//! QuestDB does not surface a separate database/catalog tier (every table
//! lives in a single namespace), but the parser accepts up to two dotted
//! segments for cosmetic parity with the other SQL backends — a
//! single-segment name remains the common case. `split_qualified` is not
//! re-exported because QuestDB has no `public` schema fallback.

pub use air_elt_commons::pg_identifier::{quote_columns, quote_ident, quote_qualified};
