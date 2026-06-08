# MongoDB helpers (full reference)

Mongo has no SQL surface, so `commons-mongodb` ships its own helper set:

- **`commons-mongodb::client`** — `mongodb::Client` builder + project-wide pool/timeout settings, reusing `commons::pool_timeouts`. Mongo has no per-statement timeout; the runner's per-call `tokio::time::timeout` covers that.
- **`commons-mongodb::identifier`** — gates database / collection names on the same character class as SQL identifiers.
- **`commons-mongodb::path`** — read / write nested BSON via `core::mapping::FieldPath`. `set` creates missing intermediate documents.
- **`commons-mongodb::bson_value`** — bidirectional codec between BSON and the canonical `Value`/`DataType`. ObjectId → `Bytes(12)`; BSON Date → `Timestamp` (UTC, sub-ms truncation documented inline); Decimal128 → `Decimal`; Document/Array → `Json`; Binary(uuid subtype) → `Uuid`. Unrepresentable BSON variants (regex, JS code, MinKey/MaxKey, …) error rather than silently dropping data.
- **`commons-mongodb::infer`** — sample-based schema inference. Folds per-field types; widens `int32 + int64` → `Int64`, `int + float` → `Float64`.
- **`commons-mongodb::sampling`** — `sample_documents` / `describe_collection_schema` / `rows_from_documents`. Shared between the `mongodb` and `mongo-cdc` sources. New mongo-shaped sources should call these instead of duplicating `$sample` aggregation pipelines.
- **`commons-mongodb::key_bson::KeyBson`** — newtype around `bson::Bson` with total `Eq` + `Hash` (NaN==NaN, Null==Null; recursion through `Document`/`Array`). Used by the mongo-cdc source to dedup change-stream events by `_id` directly on the BSON value.
- **`commons-mongodb::task::detached`** — spawns a driver call on the runtime so dropping the outer future does not cancel it (the `mongodb` 3.x driver is not cancellation-safe).
