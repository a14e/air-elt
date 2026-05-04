-- CDC resume-token storage. See storage-postgres/0002_resume_tokens.sql
-- for the rationale (separate semantics from column-based cursors).
CREATE TABLE IF NOT EXISTS air_elt_resume_tokens (
    flow       VARCHAR(255) NOT NULL PRIMARY KEY,
    token      JSON NOT NULL,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP
) ENGINE = InnoDB;
