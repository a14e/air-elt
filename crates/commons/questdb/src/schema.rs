//! Schema introspection over pg-wire.
//!
//! QuestDB exposes column metadata through `SHOW COLUMNS FROM "<table>"`,
//! which returns one row per column with the following columns:
//! `column`, `type`, `indexed`, `indexBlockCapacity`, `symbolCached`,
//! `symbolCapacity`, `designated`, `upsertKey`. We consume the `column`,
//! `type`, and `designated` triplet — everything else is sink-relevant
//! only when introspecting an existing DDL, which is out of scope.

use sqlx::{PgPool, Row};
use thiserror::Error;

use air_elt_commons::identifier::IdentifierError;
use air_elt_core::model::{Field, Schema};

use crate::identifier::quote_qualified;
use crate::qd_type_parser::{ParseError, parse_type};

#[derive(Debug)]
pub struct SchemaWithDesignated {
    pub schema: Schema,
    /// `Some(column_name)` when the table declares a designated timestamp
    /// column. Required by the sink and validated up-front at
    /// `validate_access` time.
    pub designated_column: Option<String>,
}

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("identifier: {0}")]
    Identifier(#[from] IdentifierError),
    #[error("type parse for column {column:?}: {source}")]
    TypeParse {
        column: String,
        #[source]
        source: ParseError,
    },
    #[error("table {table:?} returned no columns")]
    TableNotFound { table: String },
}

/// Issue `SHOW COLUMNS FROM "<table>"` and fold the rows into a canonical
/// schema. Columns are kept in the server-reported order — QuestDB
/// preserves declaration order, which matches the layout the sink uses
/// when binding INSERT statements.
pub async fn fetch_schema(pool: &PgPool, table: &str) -> Result<SchemaWithDesignated, SchemaError> {
    let quoted = quote_qualified(table)?;
    let sql = format!("SHOW COLUMNS FROM {quoted}");
    let rows = match sqlx::query(&sql).fetch_all(pool).await {
        Ok(rows) => rows,
        Err(error) => {
            // QuestDB pg-wire reports a missing table as a Database error
            // rather than as an empty result set. The message shape drifts
            // across versions — 8.2.x has surfaced at least:
            //   * `table does not exist [table=...]`
            //   * `'<name>' is not a valid table`
            // Match a small case-insensitive substring set so a future
            // string tweak does not silently mis-route the typed variant.
            if matches!(&error, sqlx::Error::Database(db) if is_missing_table_message(db.message()))
            {
                return Err(SchemaError::TableNotFound {
                    table: table.to_string(),
                });
            }
            return Err(SchemaError::Sqlx(error));
        }
    };
    if rows.is_empty() {
        return Err(SchemaError::TableNotFound {
            table: table.to_string(),
        });
    }

    let mut fields: Vec<Field> = Vec::with_capacity(rows.len());
    let mut designated_column: Option<String> = None;
    for row in rows {
        let name: String = row.try_get("column")?;
        let type_str: String = row.try_get("type")?;
        // `designated` is a `BOOLEAN`. QuestDB renders booleans on pg-wire
        // as the standard t/f representation; sqlx maps that to `bool`.
        let designated: bool = row.try_get("designated")?;

        let data_type = parse_type(&type_str).map_err(|source| SchemaError::TypeParse {
            column: name.clone(),
            source,
        })?;

        // Nullability: every QuestDB column is nullable from the
        // canonical model's perspective except the designated timestamp,
        // which the engine enforces server-side as NOT NULL on insert.
        let nullable = !designated;
        if designated {
            designated_column = Some(name.clone());
        }
        fields.push(Field {
            name,
            data_type,
            nullable,
        });
    }
    Ok(SchemaWithDesignated {
        schema: Schema::new(fields),
        designated_column,
    })
}

/// `true` when a pg-wire Database error message indicates the requested
/// table is missing. QuestDB has shipped multiple wordings over recent
/// versions; we match a case-insensitive substring set so the next drift
/// (e.g. `unknown table ...`) does not silently fall through to the
/// generic Backend mapping.
fn is_missing_table_message(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    const PATTERNS: &[&str] = &[
        "table does not exist",
        "is not a valid table",
        "does not exist",
        "unknown table",
    ];
    PATTERNS.iter().any(|p| lowered.contains(p))
}

impl From<SchemaError> for air_elt_core::error::RuntimeError {
    fn from(value: SchemaError) -> Self {
        use air_elt_core::error::{RuntimeError, ValidationError};
        match value {
            SchemaError::Identifier(e) => e.into(),
            SchemaError::TableNotFound { table } => {
                RuntimeError::Validation(ValidationError::SinkTableNotFound {
                    sink: "questdb".into(),
                    table,
                })
            }
            other => RuntimeError::backend(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_legacy_and_modern_table_messages() {
        assert!(is_missing_table_message("table does not exist [table=foo]"));
        assert!(is_missing_table_message("'foo' is not a valid table"));
        assert!(is_missing_table_message("Table 'foo' does not exist"));
        assert!(is_missing_table_message("unknown table foo"));
        assert!(is_missing_table_message("TABLE DOES NOT EXIST"));
        assert!(!is_missing_table_message("permission denied"));
        assert!(!is_missing_table_message("syntax error"));
    }
}
