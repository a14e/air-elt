---
name: Postgres row-constructor comparison & ORDER BY direction
description: Tuple compare (a,b)>(x,y) is lexicographic left-to-right with NULL short-circuiting; ORDER BY direction keywords apply per-column, not globally
type: reference
---

From https://www.postgresql.org/docs/16/functions-comparisons.html:
> For <, <=, > and >= cases, row elements are compared left-to-right,
> stopping as soon as an unequal or null pair of elements is found. If
> either of this pair of elements is null, the result of the row comparison
> is unknown (null); otherwise comparison of this pair determines the result.

Implication for cursor code:
- `(cursor_cols) > ($cursor_vals)` is the correct strictly-greater pagination predicate.
- If ANY cursor value is NULL, the row comparison returns NULL → WHERE drops the row. So nullable cursor columns are a functional hazard, not just a typing hazard.

From https://www.postgresql.org/docs/16/sql-select.html#SQL-ORDERBY:
- `ORDER BY a, b DESC` means `a ASC, b DESC` — the DESC binds only to `b`.
- For tuple cursor semantics to match ORDER BY, you MUST emit the direction
  on every column: `ORDER BY a DESC, b DESC`.

**How to apply:** When reviewing cursor SQL builders, check:
1. Every cursor column in the ORDER BY has an explicit direction keyword.
2. The row-compare operator (`>` vs `<`) flips with the direction.
3. The validator rejects nullable cursor columns, or the runtime explicitly
   handles NULL-terminated cursor states.

Bug site in this repo: `crates/sources/postgres/src/sql_statements.rs:74`
emits `ORDER BY {ordering_cols} {direction}` — direction only hits the last
column. Latent because the call site hard-codes `CursorOrder::Asc`.
