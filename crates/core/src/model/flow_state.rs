use std::sync::Arc;
use std::time::Duration;

use crate::mapping::ColumnMapping;
use crate::model::{ReadSpec, WriteSpec};
use crate::traits::{Sink, Source, Storage};

pub struct FlowState {
    pub name: String,
    /// Shared via `Arc` so multiple flows referencing the same source by
    /// name reuse a single instance (and its pool).
    pub source: Arc<dyn Source>,
    pub sink: Arc<dyn Sink>,
    pub storage: Arc<dyn Storage>,
    pub mappings: Vec<ColumnMapping>,
    pub read_spec: ReadSpec,
    pub write_spec: WriteSpec,
    pub interval: Duration,
    pub query_timeout: Duration,
}
