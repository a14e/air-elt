CREATE TABLE IF NOT EXISTS air_elt_cursors (
    flow       TEXT PRIMARY KEY,
    state      JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
