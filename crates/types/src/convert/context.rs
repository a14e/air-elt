//! Per-mapping context passed into [`super::convert`].
//!
//! `truncate=true` opts the column into otherwise-rejected narrowing
//! conversions (text/bytes shrink, integer/float saturate, decimal scale
//! drop, json→text serialize, etc.). `default` substitutes for `Value::Null`
//! before any conversion runs — enabling nullable-source → NOT-NULL-sink
//! flows. The default value is parsed against the *sink* `DataType` at
//! validation time, so the runner sees a ready-to-bind value.

use crate::Value;

#[derive(Debug, Clone, Default)]
pub struct ConversionContext {
    pub truncate: bool,
    pub default: Option<Value>,
}

impl ConversionContext {
    pub fn passthrough() -> Self {
        Self::default()
    }

    pub fn with_truncate(mut self) -> Self {
        self.truncate = true;
        self
    }

    pub fn with_default(mut self, v: Value) -> Self {
        self.default = Some(v);
        self
    }
}
