//! SQL emitted by the MySQL storage crate.
//!
//! The cursor table lives in the database selected by the connection URL.
//! No schema plumbing — operators encode the database name in the URL.

pub const CURSORS_TABLE: &str = "air_elt_cursors";

pub const PING: &str = "SELECT 1";

pub const TABLE_EXISTS: &str = "SELECT EXISTS (
    SELECT 1 FROM information_schema.tables
    WHERE table_name = ?
      AND table_schema = DATABASE()
) AS exists_flag";

pub const PROBE_INSERT_WHERE_FALSE: &str = "INSERT INTO air_elt_cursors (flow, state) \
    SELECT flow, state FROM air_elt_cursors WHERE FALSE";

pub const SELECT_CURSOR: &str = "SELECT state FROM air_elt_cursors WHERE flow = ?";

/// MySQL upsert: ON DUPLICATE KEY UPDATE. The trigger on `updated_at`
/// (`ON UPDATE CURRENT_TIMESTAMP`, set in migration 0001) keeps the column
/// fresh without us touching it explicitly.
///
/// MySQL 8.0.20 deprecated the `VALUES(col)` form in favour of a row alias
/// (`AS new ... new.col`); the row-alias syntax is supported since 8.0.19.
/// MariaDB does not (yet) support row aliases in `ON DUPLICATE KEY UPDATE`,
/// so we keep the legacy form for non-MySQL servers and pre-8.0.19 MySQL.
pub const UPSERT_CURSOR_LEGACY: &str = "INSERT INTO air_elt_cursors (flow, state) \
    VALUES (?, ?) \
    ON DUPLICATE KEY UPDATE state = VALUES(state)";

pub const UPSERT_CURSOR_ROW_ALIAS: &str = "INSERT INTO air_elt_cursors (flow, state) \
    VALUES (?, ?) AS new \
    ON DUPLICATE KEY UPDATE state = new.state";

/// Pick the upsert dialect based on a `SELECT VERSION()` string.
///
/// The version string looks like `8.0.36` for MySQL or
/// `10.11.6-MariaDB-1:10.11.6+maria~ubu2204` for MariaDB. MariaDB always
/// uses the legacy form. MySQL ≥ 8.0.19 uses the row-alias form to avoid
/// the 8.0.20 deprecation warning; older MySQL falls back to legacy.
pub fn pick_upsert_cursor(version: &str) -> &'static str {
    if version.to_ascii_lowercase().contains("mariadb") {
        return UPSERT_CURSOR_LEGACY;
    }
    if mysql_version_at_least(version, (8, 0, 19)) {
        UPSERT_CURSOR_ROW_ALIAS
    } else {
        UPSERT_CURSOR_LEGACY
    }
}

fn mysql_version_at_least(version: &str, target: (u32, u32, u32)) -> bool {
    let head = version
        .split(|c: char| c == '-' || c == '+' || c.is_whitespace())
        .next()
        .unwrap_or(version);
    let mut parts = head.split('.');
    let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let patch: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor, patch) >= target
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_row_alias_on_modern_mysql() {
        assert_eq!(pick_upsert_cursor("8.0.36"), UPSERT_CURSOR_ROW_ALIAS);
        assert_eq!(pick_upsert_cursor("8.4.0"), UPSERT_CURSOR_ROW_ALIAS);
        assert_eq!(pick_upsert_cursor("8.0.19"), UPSERT_CURSOR_ROW_ALIAS);
    }

    #[test]
    fn picks_legacy_on_old_mysql() {
        assert_eq!(pick_upsert_cursor("8.0.18"), UPSERT_CURSOR_LEGACY);
        assert_eq!(pick_upsert_cursor("5.7.44"), UPSERT_CURSOR_LEGACY);
    }

    #[test]
    fn picks_legacy_on_mariadb() {
        assert_eq!(
            pick_upsert_cursor("10.11.6-MariaDB-1:10.11.6+maria~ubu2204"),
            UPSERT_CURSOR_LEGACY
        );
        assert_eq!(pick_upsert_cursor("11.4.2-MariaDB"), UPSERT_CURSOR_LEGACY);
    }
}
