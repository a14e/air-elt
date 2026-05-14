//! Postgres identifier quoting. The implementation lives in
//! `air_elt_commons::pg_identifier` (SQL-92 standard double-quote
//! quoting, shared with QuestDB); this module is a thin re-export so
//! existing callers keep their `air_elt_commons_pg::identifier::…`
//! import paths.

pub use air_elt_commons::pg_identifier::{
    quote_columns, quote_ident, quote_qualified, split_qualified,
};
