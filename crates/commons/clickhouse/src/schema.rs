//! Schema introspection: read `system.columns` and build a canonical
//! [`Schema`] for one ClickHouse table.

use serde::Deserialize;
use thiserror::Error;

use air_elt_core::model::{Field, Schema};

use crate::ch_type_parser::{ParseError, parse_type};
use crate::client::{ChClient, ChClientError};
use crate::identifier::split_qualified;

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("clickhouse client: {0}")]
    Client(#[from] ChClientError),
    #[error("type parse for column {column:?}: {source}")]
    TypeParse {
        column: String,
        #[source]
        source: ParseError,
    },
    #[error("identifier: {0}")]
    Identifier(#[from] air_elt_commons::identifier::IdentifierError),
    #[error("table {table:?} not found or empty schema")]
    TableNotFound { table: String },
    #[error("invalid system.columns response: {0}")]
    Response(String),
}

#[derive(Debug, Deserialize)]
struct SystemColumnsRow {
    name: String,
    #[serde(rename = "type")]
    type_str: String,
}

#[derive(Debug, Deserialize)]
struct JsonResponse {
    data: Vec<SystemColumnsRow>,
}

pub async fn fetch_schema(client: &ChClient, table: &str) -> Result<Schema, SchemaError> {
    let (db_opt, table_name) = split_qualified(table)?;
    let db = db_opt.unwrap_or_else(|| client.database().to_string());
    // Single-quote-escape (CH SQL: '' is the escape for ').
    let db_escaped = db.replace('\'', "''");
    let table_escaped = table_name.replace('\'', "''");
    let sql = format!(
        "SELECT name, type FROM system.columns \
         WHERE database = '{db_escaped}' AND table = '{table_escaped}' \
         ORDER BY position FORMAT JSON"
    );
    let body = client.query_text(&sql).await?;
    let resp: JsonResponse = serde_json::from_str(&body)
        .map_err(|e| SchemaError::Response(format!("{e}; body: {}", truncate(&body, 256))))?;
    if resp.data.is_empty() {
        return Err(SchemaError::TableNotFound {
            table: table.to_string(),
        });
    }
    let mut fields: Vec<Field> = Vec::with_capacity(resp.data.len());
    for row in resp.data {
        let parsed = parse_type(&row.type_str).map_err(|source| SchemaError::TypeParse {
            column: row.name.clone(),
            source,
        })?;
        fields.push(Field {
            name: row.name,
            data_type: parsed.data_type,
            nullable: parsed.nullable,
        });
    }
    Ok(Schema::new(fields))
}

fn truncate(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    // Walk back to the nearest UTF-8 char boundary so we don't split a
    // multi-byte sequence — CH error bodies may contain Cyrillic / emoji
    // identifiers, and byte slicing inside such a sequence panics.
    let mut end = n;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
