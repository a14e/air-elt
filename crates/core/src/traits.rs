use std::sync::Arc;

use async_trait::async_trait;

use crate::error::RuntimeResult;
use crate::model::{
    Batch, CursorState, ReadSpec, Row, Schema, SinkCtx, SourceCtx, WriteReport, WriteSpec,
};

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Source: Send + Sync {
    /// The connector instance's config-given name (`[[sources]] name = ...`).
    /// Used by the validation pipeline to group flows that share a
    /// source pool: each source name becomes one async worker so the
    /// shared pool isn't fanned out under contention.
    fn name(&self) -> &str;

    async fn validate_access(&self, spec: &ReadSpec) -> RuntimeResult<()>;
    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema>;
    async fn build_context(&self, spec: &ReadSpec) -> RuntimeResult<Arc<dyn SourceCtx>>;
    /// Read a batch. `ctx` is shared via `Arc`: the runner keeps its own
    /// clone, the future holds another. Async cancellation (timeout /
    /// shutdown) drops only the future's clone — the runner's ctx survives,
    /// preserving any cached state inside ctx across ticks.
    async fn read_batch<'a>(
        &self,
        spec: &ReadSpec,
        ctx: Arc<dyn SourceCtx>,
        cursor: Option<&'a CursorState>,
    ) -> RuntimeResult<Batch>;

    /// Pull up to `n` rows for sampling-validation by driving the
    /// cursor query — same code path the runner uses, so a wedged
    /// cursor query surfaces here too. The default implementation
    /// issues a single cursor-less `read_batch` with `spec.limit`
    /// overridden by `n`, discards the returned cursor state, and
    /// returns the rows. Connectors do not override this; they
    /// override `sample_fresh` to add a complementary random sample.
    async fn sample(&self, spec: &ReadSpec, n: usize) -> RuntimeResult<Vec<Row>> {
        let mut sample_spec = spec.clone();
        sample_spec.limit = n;
        let ctx = self.build_context(&sample_spec).await?;
        let batch = self.read_batch(&sample_spec, ctx, None).await?;
        Ok(batch.rows)
    }

    /// Optional companion to `sample` — pulls a *random* slice instead
    /// of the cursor-ordered head. Default returns an empty vector for
    /// backends without a cheap random-access primitive (Postgres,
    /// MySQL/MariaDB). Mongo overrides this with
    /// `aggregate([{ $sample: { size: n } }])`. Sampling-validation
    /// runs both `sample` (for the cursor path) and `sample_fresh`
    /// (for representativeness) and unions the rows before exercising
    /// the conversion plan.
    async fn sample_fresh(&self, _spec: &ReadSpec, _n: usize) -> RuntimeResult<Vec<Row>> {
        Ok(Vec::new())
    }

    /// `true` when this connector's in-flight futures are safe to drop
    /// mid-await (i.e. the underlying driver supports cooperative
    /// cancellation without leaving internal state inconsistent).
    /// `sqlx` cleanly cancels in-flight queries on drop, so its
    /// connectors stay `true`. The `mongodb` 3.x Rust driver is **not**
    /// cancellation-safe — dropping its futures can leave driver
    /// internals inconsistent. Such connectors must return `false` so
    /// the runner skips its client-side `tokio::time::timeout` wrap and
    /// relies on `ClientOptions::default_timeout` (server-enforced)
    /// instead.
    fn cancel_safe(&self) -> bool {
        true
    }

    /// `true` when this source can emit `Row { op: Delete }`. Pull-based
    /// connectors (postgres / mysql / mongodb cursor) only ever emit
    /// `Upsert`, so they keep the default `false`. CDC connectors
    /// (`mongo-cdc`) override to `true`. The validation pipeline uses
    /// this flag to decide whether to also pre-flight `Sink::validate_delete_access`
    /// — without it, a missing DELETE privilege on the sink only
    /// surfaces at runtime on the first delete batch.
    fn emits_deletes(&self) -> bool {
        false
    }
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Sink: Send + Sync {
    async fn validate_access(&self, spec: &WriteSpec) -> RuntimeResult<()>;
    /// Pre-flight the DELETE path. Default returns `Ok(())` so backends
    /// with no DELETE distinction (or that already cover it inside
    /// `validate_access`) opt out for free. Called by the validation
    /// pipeline only when the source `emits_deletes()` *and* the flow
    /// declares a `[flow.x.conflict]` block — both are required for
    /// the runner to ever invoke the DELETE path.
    async fn validate_delete_access(&self, _spec: &WriteSpec) -> RuntimeResult<()> {
        Ok(())
    }
    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema>;
    async fn build_context(&self, spec: &WriteSpec) -> RuntimeResult<Arc<dyn SinkCtx>>;
    async fn write_batch(
        &self,
        spec: &WriteSpec,
        ctx: Arc<dyn SinkCtx>,
        batch: &Batch,
    ) -> RuntimeResult<WriteReport>;

    /// `true` for sinks that have no authoritative column schema —
    /// notably MongoDB, where collections accept any BSON shape.
    /// When true, validation skips the type-compatibility matrix
    /// check for this sink (the matrix has nothing to compare
    /// against) and instead treats every mapped column as accepting
    /// the source's declared type.
    fn schemaless(&self) -> bool {
        false
    }

    /// See `Source::cancel_safe`. Same contract — Mongo sinks return
    /// `false`; sqlx-backed sinks keep the default `true`.
    fn cancel_safe(&self) -> bool {
        true
    }
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Storage: Send + Sync {
    async fn validate_access(&self) -> RuntimeResult<()>;
    async fn migrate(&self) -> RuntimeResult<()>;
    async fn load_cursor(&self, flow: &str) -> RuntimeResult<Option<CursorState>>;
    async fn save_cursor(&self, flow: &str, state: &CursorState) -> RuntimeResult<()>;

    /// CDC resume tokens are conceptually distinct from column-based
    /// cursors — they are opaque per-stream blobs (BSON for Mongo)
    /// keyed by flow name. Stored in their own table / collection.
    /// Default impls error so a misrouted call surfaces clearly.
    async fn load_resume_token(&self, _flow: &str) -> RuntimeResult<Option<serde_json::Value>> {
        Err(crate::error::RuntimeError::Other(
            "this storage backend does not implement CDC resume-token \
             persistence — switch to a backend that does (postgres / mysql / mongodb)"
                .into(),
        ))
    }
    async fn save_resume_token(
        &self,
        _flow: &str,
        _token: &serde_json::Value,
    ) -> RuntimeResult<()> {
        Err(crate::error::RuntimeError::Other(
            "this storage backend does not implement CDC resume-token \
             persistence — switch to a backend that does (postgres / mysql / mongodb)"
                .into(),
        ))
    }

    /// See `Source::cancel_safe`. Same contract — Mongo storages
    /// return `false`; sqlx-backed storages keep the default `true`.
    fn cancel_safe(&self) -> bool {
        true
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // A trivial connector that opts into every default — used to lock
    // down the `cancel_safe()` default-true contract. If somebody flips
    // the default to `false` accidentally, this test catches it before
    // it silently turns every sqlx connector into the spawn-detach path.
    struct Defaults;

    #[async_trait]
    impl Source for Defaults {
        fn name(&self) -> &str {
            "defaults"
        }
        async fn validate_access(&self, _spec: &ReadSpec) -> RuntimeResult<()> {
            Ok(())
        }
        async fn describe_schema(&self, _table: &str) -> RuntimeResult<Schema> {
            Ok(Schema::default())
        }
        async fn build_context(&self, _spec: &ReadSpec) -> RuntimeResult<Arc<dyn SourceCtx>> {
            unreachable!("not exercised in this test")
        }
        async fn read_batch<'a>(
            &self,
            _spec: &ReadSpec,
            _ctx: Arc<dyn SourceCtx>,
            _cursor: Option<&'a CursorState>,
        ) -> RuntimeResult<Batch> {
            Ok(Batch::default())
        }
    }

    #[async_trait]
    impl Sink for Defaults {
        async fn validate_access(&self, _spec: &WriteSpec) -> RuntimeResult<()> {
            Ok(())
        }
        async fn describe_schema(&self, _table: &str) -> RuntimeResult<Schema> {
            Ok(Schema::default())
        }
        async fn build_context(&self, _spec: &WriteSpec) -> RuntimeResult<Arc<dyn SinkCtx>> {
            unreachable!("not exercised in this test")
        }
        async fn write_batch(
            &self,
            _spec: &WriteSpec,
            _ctx: Arc<dyn SinkCtx>,
            _batch: &Batch,
        ) -> RuntimeResult<WriteReport> {
            Ok(WriteReport::default())
        }
    }

    #[async_trait]
    impl Storage for Defaults {
        async fn validate_access(&self) -> RuntimeResult<()> {
            Ok(())
        }
        async fn migrate(&self) -> RuntimeResult<()> {
            Ok(())
        }
        async fn load_cursor(&self, _flow: &str) -> RuntimeResult<Option<CursorState>> {
            Ok(None)
        }
        async fn save_cursor(&self, _flow: &str, _state: &CursorState) -> RuntimeResult<()> {
            Ok(())
        }
    }

    #[test]
    fn cancel_safe_default_is_true() {
        let d = Defaults;
        assert!(<Defaults as Source>::cancel_safe(&d));
        assert!(<Defaults as Sink>::cancel_safe(&d));
        assert!(<Defaults as Storage>::cancel_safe(&d));
    }

    // A connector that opts out of cancellation safety — exercises the
    // override path without any real Mongo dependency.
    struct Unsafe;

    #[async_trait]
    impl Source for Unsafe {
        fn name(&self) -> &str {
            "unsafe"
        }
        async fn validate_access(&self, _spec: &ReadSpec) -> RuntimeResult<()> {
            Ok(())
        }
        async fn describe_schema(&self, _table: &str) -> RuntimeResult<Schema> {
            Ok(Schema::default())
        }
        async fn build_context(&self, _spec: &ReadSpec) -> RuntimeResult<Arc<dyn SourceCtx>> {
            unreachable!()
        }
        async fn read_batch<'a>(
            &self,
            _spec: &ReadSpec,
            _ctx: Arc<dyn SourceCtx>,
            _cursor: Option<&'a CursorState>,
        ) -> RuntimeResult<Batch> {
            Ok(Batch::default())
        }
        fn cancel_safe(&self) -> bool {
            false
        }
    }

    #[test]
    fn cancel_safe_can_be_overridden() {
        let u = Unsafe;
        assert!(!<Unsafe as Source>::cancel_safe(&u));
    }
}
