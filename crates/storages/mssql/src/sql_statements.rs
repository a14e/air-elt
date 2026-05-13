//! SQL emitted by the MS SQL storage crate.
//!
//! Two system tables: `air_elt_cursors` and `air_elt_resume_tokens`.
//! Uses MERGE for upsert. Values inlined via `simple_query`.

pub const CURSORS_TABLE: &str = "air_elt_cursors";
pub const RESUME_TOKENS_TABLE: &str = "air_elt_resume_tokens";

pub const PING: &str = "SELECT 1";

pub const TABLE_EXISTS: &str = "\
    SELECT CASE WHEN EXISTS (\
        SELECT 1 FROM INFORMATION_SCHEMA.TABLES \
        WHERE TABLE_NAME = @P1 AND TABLE_SCHEMA = SCHEMA_NAME()\
    ) THEN CAST(1 AS BIT) ELSE CAST(0 AS BIT) END AS exists_flag";

// Probe INSERT for access validation.
pub const PROBE_INSERT_CURSORS_WHERE_FALSE: &str = "\
    INSERT INTO air_elt_cursors (flow, state) \
    SELECT flow, state FROM air_elt_cursors WHERE 1=0";

pub const PROBE_INSERT_TOKENS_WHERE_FALSE: &str = "\
    INSERT INTO air_elt_resume_tokens (flow, token) \
    SELECT flow, token FROM air_elt_resume_tokens WHERE 1=0";

pub const SELECT_CURSOR: &str = "\
    SELECT state FROM air_elt_cursors WHERE flow = @P1";

pub const SELECT_RESUME_TOKEN: &str = "\
    SELECT token FROM air_elt_resume_tokens WHERE flow = @P1";

// MERGE for cursor UPSERT. `WITH (HOLDLOCK)` is the documented workaround
// for the MSSQL MERGE upsert race — without it, two concurrent flow
// workers can both observe NOT MATCHED and produce a duplicate-key error.
// `updated_at` is set via DEFAULT GETUTCDATE() in the DDL — the MERGE
// does not explicitly touch it (matches PG/MySQL pattern).
pub const UPSERT_CURSOR: &str = "\
    MERGE air_elt_cursors WITH (HOLDLOCK) AS target \
    USING (VALUES (@P1, @P2)) AS source(flow, state) \
    ON target.flow = source.flow \
    WHEN MATCHED THEN UPDATE SET state = source.state \
    WHEN NOT MATCHED THEN INSERT (flow, state) VALUES (source.flow, source.state);";

pub const UPSERT_RESUME_TOKEN: &str = "\
    MERGE air_elt_resume_tokens WITH (HOLDLOCK) AS target \
    USING (VALUES (@P1, @P2)) AS source(flow, token) \
    ON target.flow = source.flow \
    WHEN MATCHED THEN UPDATE SET token = source.token \
    WHEN NOT MATCHED THEN INSERT (flow, token) VALUES (source.flow, source.token);";

/// Wrap a write statement in a single-batch try/rollback for dry-run
/// validation. The whole batch is sent as one command so a failure on the
/// inner statement cannot leave a transaction open on the connection.
pub fn dry_run_wrap(inner: &str) -> String {
    format!(
        "BEGIN TRY \
            BEGIN TRANSACTION; \
            {inner} \
            IF @@TRANCOUNT > 0 ROLLBACK; \
         END TRY \
         BEGIN CATCH \
            IF @@TRANCOUNT > 0 ROLLBACK; \
            THROW; \
         END CATCH;"
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_wrap_contains_try_catch_and_rollback() {
        let s = dry_run_wrap("INSERT INTO foo VALUES (1);");
        assert!(s.contains("BEGIN TRY"));
        assert!(s.contains("BEGIN CATCH"));
        assert!(s.contains("ROLLBACK"));
        assert!(s.contains("THROW;"));
    }
}
