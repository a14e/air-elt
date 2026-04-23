use async_trait::async_trait;

use crate::error::RuntimeResult;
use crate::model::{Batch, CursorState, ReadSpec, Schema, WriteReport, WriteSpec};

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Source: Send + Sync {
    async fn validate_access(&self, spec: &ReadSpec) -> RuntimeResult<()>;
    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema>;
    async fn read_batch<'a>(
        &self,
        spec: &ReadSpec,
        cursor: Option<&'a CursorState>,
    ) -> RuntimeResult<Batch>;
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Sink: Send + Sync {
    async fn validate_access(&self, spec: &WriteSpec) -> RuntimeResult<()>;
    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema>;
    async fn write_batch(&self, spec: &WriteSpec, batch: &Batch) -> RuntimeResult<WriteReport>;
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Storage: Send + Sync {
    async fn validate_access(&self) -> RuntimeResult<()>;
    async fn migrate(&self) -> RuntimeResult<()>;
    async fn load_cursor(&self, flow: &str) -> RuntimeResult<Option<CursorState>>;
    async fn save_cursor(&self, flow: &str, state: &CursorState) -> RuntimeResult<()>;
}
