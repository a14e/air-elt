//! MongoDB storage backend for cursor state and CDC resume tokens.
//!
//! Two collections, both keyed on `_id = flow_name`:
//! * `"air_elt_cursors"` — column-cursor state for pull-based
//!   sources (`Storage::{load,save}_cursor`). Payload is the JSON
//!   serialisation of `CursorState`, parked under field `cursor`.
//!   Why JSON instead of pure BSON: `CursorState` already has a
//!   stable JSON serde impl that round-trips every `Value` variant
//!   losslessly via the tagged-enum form
//!   (`{"type": "int64", "value": 7}`), and we re-use it here.
//!   Storing native BSON would require a parallel codec for every
//!   type variant.
//! * `"air_elt_resume_tokens"` — opaque CDC resume tokens
//!   (`Storage::{load,save}_resume_token`). Same JSON-as-string
//!   approach for the same reason.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bson::{Document, doc};
use mongodb::Client;
use mongodb::options::{FindOneOptions, ReplaceOptions};
use tracing::info;

use air_elt_commons_mongodb::MongoPoolStatsReader;
use air_elt_commons_mongodb::client::{PoolSettings, connect, database_from_url};
use air_elt_commons_mongodb::identifier;
use air_elt_commons_mongodb::task::detached;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::CursorState;
use air_elt_core::traits::Storage;
use air_elt_core::types::DataType;

use crate::config::MongoStorageConfig;

const DEFAULT_COLLECTION: &str = "air_elt_cursors";
/// Resume tokens live in their own collection — see the
/// `Storage::save_resume_token` rationale: the token is a distinct
/// concept (opaque BSON keyed by flow), kept apart from
/// column-cursor state.
const DEFAULT_RESUME_TOKENS_COLLECTION: &str = "air_elt_resume_tokens";

pub struct MongoStorage {
    client: Client,
    database: String,
    collection: String,
    resume_tokens_collection: String,
    /// Per-operation cap; applied as `max_time` / `maxTimeMS` on every
    /// server-side call. Bounds runaway server work after a detach.
    operation_timeout: Duration,
    pool_max_connections: u32,
}

impl MongoStorage {
    /// See `MongoSource::connect` for the reader lifecycle contract.
    pub async fn connect(
        config: MongoStorageConfig,
        reader: Arc<MongoPoolStatsReader>,
    ) -> RuntimeResult<Self> {
        let database = config
            .database
            .clone()
            .or_else(|| database_from_url(&config.url))
            .ok_or_else(|| {
                RuntimeError::Other(
                    "mongodb storage: `database` not set and url has no path component".into(),
                )
            })?;
        identifier::validate_name(&database).map_err(RuntimeError::from)?;
        let collection = config
            .collection
            .clone()
            .unwrap_or_else(|| DEFAULT_COLLECTION.to_string());
        identifier::validate_name(&collection).map_err(RuntimeError::from)?;

        let settings = PoolSettings::from_options(
            config.connect_timeout,
            config.acquire_timeout,
            config.idle_timeout,
            None,
            config.operation_timeout,
            config.max_connections,
            config.min_connections,
        )?;
        let operation_timeout = settings.statement;
        let pool_max_connections = settings.max_connections;
        let client = connect(&config.url, settings, reader).await?;
        Ok(Self {
            client,
            database,
            collection,
            resume_tokens_collection: DEFAULT_RESUME_TOKENS_COLLECTION.to_string(),
            operation_timeout,
            pool_max_connections,
        })
    }

    fn max_time_ms(&self) -> i64 {
        i64::try_from(self.operation_timeout.as_millis()).unwrap_or(i64::MAX)
    }

    fn coll(&self) -> mongodb::Collection<Document> {
        self.client
            .database(&self.database)
            .collection(&self.collection)
    }

    fn resume_tokens(&self) -> mongodb::Collection<Document> {
        self.client
            .database(&self.database)
            .collection(&self.resume_tokens_collection)
    }
}

#[async_trait]
impl Storage for MongoStorage {
    fn max_connections(&self) -> u32 {
        self.pool_max_connections
    }

    async fn validate_access(&self) -> RuntimeResult<()> {
        let database = self.database.clone();
        let client = self.client.clone();
        let coll = self.coll();
        let db_for_log = self.database.clone();
        let coll_for_log = self.collection.clone();
        let max_time_ms = self.max_time_ms();
        detached(async move {
            client
                .database(&database)
                .run_command(doc! { "ping": 1, "maxTimeMS": max_time_ms })
                .await
                .map_err(RuntimeError::backend)?;
            // Write probe: replace_one upsert + delete sentinel doc.
            // `ReplaceOptions`/`delete_one` don't expose `max_time` in
            // mongodb 3.6 typed builders; the spawned task ensures the
            // driver future is never dropped mid-await regardless.
            let id = "__air_elt_probe__";
            let opts = ReplaceOptions::builder().upsert(Some(true)).build();
            coll.replace_one(doc! { "_id": id }, doc! { "_id": id, "probe": true })
                .with_options(opts)
                .await
                .map_err(RuntimeError::backend)?;
            coll.delete_one(doc! { "_id": id })
                .await
                .map_err(RuntimeError::backend)?;
            info!(database = %db_for_log, collection = %coll_for_log, "mongodb storage access validated");
            Ok(())
        })
        .await
    }

    async fn migrate(&self) -> RuntimeResult<()> {
        // Mongo collections / indexes are implicit. Nothing to do.
        Ok(())
    }

    async fn load_cursor(
        &self,
        flow: &str,
        cursor_types: &[DataType],
    ) -> RuntimeResult<Option<CursorState>> {
        let coll = self.coll();
        let flow = flow.to_string();
        let max_time = self.operation_timeout;
        let cursor_types: Vec<DataType> = cursor_types.to_vec();
        detached(async move {
            let opts = FindOneOptions::builder().max_time(max_time).build();
            let opt = coll
                .find_one(doc! { "_id": &flow })
                .with_options(opts)
                .await
                .map_err(RuntimeError::backend)?;
            let Some(doc_) = opt else { return Ok(None) };
            let cursor_json = doc_.get_str("cursor").map_err(|_| {
                RuntimeError::Other(format!(
                    "mongodb storage: row for flow {flow:?} is missing string field `cursor`"
                ))
            })?;
            let raw: serde_json::Value =
                serde_json::from_str(cursor_json).map_err(RuntimeError::from)?;
            let parsed = CursorState::from_typed_json(raw, &cursor_types)?;
            Ok(Some(parsed))
        })
        .await
    }

    async fn save_cursor(
        &self,
        flow: &str,
        state: &CursorState,
        dry_run: bool,
    ) -> RuntimeResult<()> {
        let cursor_json = serde_json::to_string(state).map_err(RuntimeError::from)?;
        if dry_run {
            return Ok(());
        }
        let coll = self.coll();
        let flow = flow.to_string();
        detached(async move {
            let opts = ReplaceOptions::builder().upsert(Some(true)).build();
            coll.replace_one(
                doc! { "_id": &flow },
                doc! {
                    "_id": &flow,
                    "cursor": cursor_json,
                    "updated_at": bson::DateTime::now(),
                },
            )
            .with_options(opts)
            .await
            .map_err(RuntimeError::backend)?;
            Ok(())
        })
        .await
    }

    async fn load_resume_token(&self, flow: &str) -> RuntimeResult<Option<serde_json::Value>> {
        let coll = self.resume_tokens();
        let flow = flow.to_string();
        let max_time = self.operation_timeout;
        detached(async move {
            let opts = FindOneOptions::builder().max_time(max_time).build();
            let opt = coll
                .find_one(doc! { "_id": &flow })
                .with_options(opts)
                .await
                .map_err(RuntimeError::backend)?;
            let Some(doc_) = opt else { return Ok(None) };
            // `token` is stored as the JSON-string form (round-trips
            // through serde_json without a parallel BSON codec — same
            // rationale as `cursor` above).
            let token_json = doc_.get_str("token").map_err(|_| {
                RuntimeError::Other(format!(
                    "mongodb storage: resume-token row for flow {flow:?} is missing field `token`"
                ))
            })?;
            let parsed: serde_json::Value =
                serde_json::from_str(token_json).map_err(RuntimeError::from)?;
            Ok(Some(parsed))
        })
        .await
    }

    async fn save_resume_token(
        &self,
        flow: &str,
        token: &serde_json::Value,
        dry_run: bool,
    ) -> RuntimeResult<()> {
        let token_json = serde_json::to_string(token).map_err(RuntimeError::from)?;
        if dry_run {
            return Ok(());
        }
        let coll = self.resume_tokens();
        let flow = flow.to_string();
        detached(async move {
            let opts = ReplaceOptions::builder().upsert(Some(true)).build();
            coll.replace_one(
                doc! { "_id": &flow },
                doc! {
                    "_id": &flow,
                    "token": token_json,
                    "updated_at": bson::DateTime::now(),
                },
            )
            .with_options(opts)
            .await
            .map_err(RuntimeError::backend)?;
            Ok(())
        })
        .await
    }
}
