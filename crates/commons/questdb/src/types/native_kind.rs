use super::{QuestDbGeohashType, QuestDbLong256Type, QuestDbSymbolType};

/// `true` when `kind` matches one of QuestDB's native custom kinds
/// (`questdb.symbol`, `questdb.long256`, `questdb.geohash`).
/// IPv4 was promoted to canonical `DataType::Ipv4` and no longer
/// appears in this list.
///
/// Shared by sink type-gate and the pg-wire NULL bind path so the
/// recognised kinds stay enumerated in exactly one place.
pub fn is_questdb_native_kind(kind: &str) -> bool {
    kind == QuestDbSymbolType::KIND
        || kind == QuestDbLong256Type::KIND
        || kind == QuestDbGeohashType::KIND
}
