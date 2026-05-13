-- Migration zero: bootstrap the ledger table itself.
-- This migration is special — it runs unconditionally on every migrate()
-- call because the ledger doesn't yet exist to record its own application.
-- The IF OBJECT_ID guard keeps it idempotent on subsequent runs.
IF OBJECT_ID(N'_air_elt_migrations', N'U') IS NULL
BEGIN
    CREATE TABLE _air_elt_migrations (
        version    INT NOT NULL PRIMARY KEY,
        applied_at DATETIME2 NOT NULL DEFAULT GETUTCDATE()
    );
END;
