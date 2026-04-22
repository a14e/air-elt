---
name: sqlx 0.8 typed-null binding quirk
description: Option<T>::None in sqlx carries T's type OID; binding a typed null to a column of a different type errors on the server
type: reference
---

From `sqlx-postgres/src/arguments.rs` at v0.8.6: `IsNull::Yes` writes -1 for
the parameter length, and the type OID sent in the Bind message comes from
`T::type_info()`. This means `push_bind::<Option<i64>>(None)` advertises the
parameter as `int8` NULL — not "untyped NULL".

For a bulk INSERT via `QueryBuilder::push_values`, each typed-null must
match the target column. The sink code at
`crates/sinks/postgres/src/pg_sink.rs:147-185` does this correctly with a
match over `DataType`. The source's `bind_cursor_value` in
`crates/sources/postgres/src/model/mapping.rs:70-72` does NOT — it always
binds `Option<i64>::None`, which is only safe when the cursor column is
`int8`.

**How to apply:** Any time you see a NULL bound via sqlx, check that the
Rust type inside the `Option` matches the DB column type. If the code maps
an internal `Value::Null` to `push_bind`, the match arm must branch on the
column's declared `DataType`, not just bind `Option<i64>::None`. A single
bigint-fallback is a silent type-mismatch bug waiting for a non-int cursor.

Reference: https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-postgres/src/arguments.rs
