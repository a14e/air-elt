-- CDC resume-token storage.
--
-- Why a separate table: resume tokens are conceptually distinct from
-- column-based cursors (an opaque BSON/JSON blob keyed by flow, not a
-- list of mapped column values). Mixing the two in one row would
-- require a polymorphic schema and a discriminator column. A second
-- table keeps the model honest and the routing trivial:
-- `Storage::{load,save}_resume_token` write here; the existing
-- column-cursor methods stay on `air_elt_cursors`.
CREATE TABLE IF NOT EXISTS air_elt_resume_tokens (
    flow       TEXT PRIMARY KEY,
    token      JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
