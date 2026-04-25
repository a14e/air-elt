use async_trait::async_trait;

use crate::error::RuntimeResult;
use crate::model::{
    Batch, CursorState, ReadSpec, Schema, SinkCtx, SourceCtx, WriteReport, WriteSpec,
};

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Source: Send + Sync {
    async fn validate_access(&self, spec: &ReadSpec) -> RuntimeResult<()>;
    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema>;
    async fn init_context(&self, spec: &ReadSpec) -> RuntimeResult<Box<dyn SourceCtx>>;
    async fn read_batch<'a>(
        &self,
        spec: &ReadSpec,
        ctx: Box<dyn SourceCtx>,
        cursor: Option<&'a CursorState>,
    ) -> RuntimeResult<(Batch, Box<dyn SourceCtx>)>;
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Sink: Send + Sync {
    async fn validate_access(&self, spec: &WriteSpec) -> RuntimeResult<()>;
    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema>;
    async fn init_context(&self, spec: &WriteSpec) -> RuntimeResult<Box<dyn SinkCtx>>;
    async fn write_batch(
        &self,
        spec: &WriteSpec,
        ctx: Box<dyn SinkCtx>,
        batch: &Batch,
    ) -> RuntimeResult<(WriteReport, Box<dyn SinkCtx>)>;
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Storage: Send + Sync {
    async fn validate_access(&self) -> RuntimeResult<()>;
    async fn migrate(&self) -> RuntimeResult<()>;
    async fn load_cursor(&self, flow: &str) -> RuntimeResult<Option<CursorState>>;
    async fn save_cursor(&self, flow: &str, state: &CursorState) -> RuntimeResult<()>;
}
