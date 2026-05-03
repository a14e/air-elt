//! MongoDB storage backend for cursor state.
//!
//! State is stored in a single collection (default
//! `"air_elt_cursors"`), one document per flow, keyed on `_id =
//! flow_name`. The cursor payload is the JSON serialisation of
//! `CursorState`, parked under field `cursor`. Why JSON instead of
//! pure BSON: `CursorState` already has a stable JSON serde impl
//! that round-trips every `Value` variant losslessly via the
//! tagged-enum form (`{"type": "int64", "value": 7}`), and we can
//! re-use it as-is here. Storing native BSON would require a
//! parallel codec for every type variant.

use async_trait::async_trait;
use bson::{Document, doc};
use mongodb::Client;
use mongodb::options::ReplaceOptions;
use tracing::info;

use air_elt_commons_mongodb::client::{PoolSettings, connect, database_from_url};
use air_elt_commons_mongodb::identifier;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::CursorState;
use air_elt_core::traits::Storage;

use crate::config::MongoStorageConfig;

const DEFAULT_COLLECTION: &str = "air_elt_cursors";

pub struct MongoStorage {
    client: Client,
    database: String,
    collection: String,
}

impl MongoStorage {
    pub async fn connect(config: MongoStorageConfig) -> RuntimeResult<Self> {
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
            None,
            config.max_connections,
            config.min_connections,
        );
        let client = connect(&config.url, settings).await?;
        Ok(Self {
            client,
            database,
            collection,
        })
    }

    fn coll(&self) -> mongodb::Collection<Document> {
        self.client
            .database(&self.database)
            .collection(&self.collection)
    }
}

#[async_trait]
impl Storage for MongoStorage {
    async fn validate_access(&self) -> RuntimeResult<()> {
        self.client
            .database(&self.database)
            .run_command(doc! { "ping": 1 })
            .await
            .map_err(RuntimeError::backend)?;
        // Write probe: replace_one upsert + delete sentinel doc.
        let coll = self.coll();
        let id = "__air_elt_probe__";
        let opts = ReplaceOptions::builder().upsert(Some(true)).build();
        coll.replace_one(doc! { "_id": id }, doc! { "_id": id, "probe": true })
            .with_options(opts)
            .await
            .map_err(RuntimeError::backend)?;
        let _ = coll
            .delete_one(doc! { "_id": id })
            .await
            .map_err(RuntimeError::backend)?;
        info!(database = %self.database, collection = %self.collection, "mongodb storage access validated");
        Ok(())
    }

    async fn migrate(&self) -> RuntimeResult<()> {
        // Mongo collections / indexes are implicit. Nothing to do.
        Ok(())
    }

    async fn load_cursor(&self, flow: &str) -> RuntimeResult<Option<CursorState>> {
        let coll = self.coll();
        let opt = coll
            .find_one(doc! { "_id": flow })
            .await
            .map_err(RuntimeError::backend)?;
        let Some(doc_) = opt else { return Ok(None) };
        let cursor_json = doc_.get_str("cursor").map_err(|_| {
            RuntimeError::Other(format!(
                "mongodb storage: row for flow {flow:?} is missing string field `cursor`"
            ))
        })?;
        let parsed: CursorState = serde_json::from_str(cursor_json).map_err(RuntimeError::from)?;
        Ok(Some(parsed))
    }

    fn cancel_safe(&self) -> bool {
        // The `mongodb` 3.x Rust driver is not cancellation-safe —
        // see `MongoSource::cancel_safe` for the full rationale.
        false
    }

    async fn save_cursor(&self, flow: &str, state: &CursorState) -> RuntimeResult<()> {
        let cursor_json = serde_json::to_string(state).map_err(RuntimeError::from)?;
        let coll = self.coll();
        let opts = ReplaceOptions::builder().upsert(Some(true)).build();
        coll.replace_one(
            doc! { "_id": flow },
            doc! {
                "_id": flow,
                "cursor": cursor_json,
                "updated_at": bson::DateTime::now(),
            },
        )
        .with_options(opts)
        .await
        .map_err(RuntimeError::backend)?;
        Ok(())
    }
}
