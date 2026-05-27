//! `AggregateFunction(fn, args)` and `SimpleAggregateFunction(fn, args)`
//! states. Opaque binary payloads — ClickHouse serialises every
//! aggregate-function state via its own internal format (different per
//! `fn`), and we never inspect them.
//!
//! Identity-only: copy CH↔CH (the only credible use case is replicating
//! a `quantilesTDigestState` between two CH clusters without
//! materialising values). The matrix rejects any cross-canonical
//! mapping; no `truncate` softens this.
//!
//! `kind()` is derived from the aggregate function name via
//! [`kind_for_fn`] so that telemetry can distinguish TDigest from
//! DDSketch from Uniq states without parsing the type-string twice.
//! The function name component is snake_case'd from the CH camelCase.
//! The kind string is stored on the struct — no allocations at
//! trait-method time, no `Box::leak`.

use std::any::Any;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

use air_elt_core::error::JsonEncodeError;
use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

/// Schema descriptor for `AggregateFunction(<fn>(...params), <arg_types>...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChAggregateStateType {
    /// Original ClickHouse function name in camelCase, e.g.
    /// `quantilesTDigest`. Parameters (`(0.5, 0.99)`) are not retained
    /// because they do not affect the binary state format.
    pub fn_name: String,
    /// Stringified argument types — kept verbatim from CH for
    /// diagnostics.
    pub arg_types: Vec<String>,
    /// `true` for `SimpleAggregateFunction(fn, T)` (state == T value);
    /// `false` for the standard `AggregateFunction`.
    pub simple: bool,
    /// Pre-computed `kind()` string — `"clickhouse.aggregate.<snake_fn>"`.
    pub kind: String,
}

impl ChAggregateStateType {
    /// Shared prefix for every aggregate-state `kind()` string. Every
    /// recognition site (encoder, validator, schema introspection) must
    /// match against `KIND_PREFIX` rather than spelling the literal
    /// twice — a rename has to flow through the compiler.
    pub const KIND_PREFIX: &'static str = "clickhouse.aggregate.";

    /// Build the kind string from a CH function name.
    pub fn kind_for_fn(fn_name: &str) -> String {
        format!("{}{}", Self::KIND_PREFIX, camel_to_snake(fn_name))
    }
}

fn camel_to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

impl DynType for ChAggregateStateType {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn kind(&self) -> &str {
        &self.kind
    }

    fn display(&self) -> String {
        if self.arg_types.is_empty() {
            format!("AggregateFunction({})", self.fn_name)
        } else {
            format!(
                "AggregateFunction({}, {})",
                self.fn_name,
                self.arg_types.join(", ")
            )
        }
    }

    fn can_convert_to(&self, target: &DataType, _truncate: bool) -> bool {
        matches!(target, DataType::Custom(t) if t.kind() == self.kind())
    }

    fn can_construct_from(&self, src: &DataType, _truncate: bool) -> bool {
        matches!(src, DataType::Custom(t) if t.kind() == self.kind())
    }

    fn convert(
        &self,
        value: Value,
        target: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        if matches!(target, DataType::Custom(t) if t.kind() == self.kind()) {
            return Ok(value);
        }
        Err(ConvertError::Unsupported {
            src: DataType::Custom(Box::new(self.clone())),
            dst: target.clone(),
        })
    }

    fn construct(
        &self,
        value: Value,
        src: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        if matches!(src, DataType::Custom(t) if t.kind() == self.kind()) {
            return Ok(value);
        }
        Err(ConvertError::Unsupported {
            src: src.clone(),
            dst: DataType::Custom(Box::new(self.clone())),
        })
    }

    fn parse_default(&self, _literal: &toml::Value) -> Result<Option<Value>, String> {
        // No sane TOML literal for an aggregate state; operator must
        // populate via CH-side functions.
        Ok(None)
    }

    // eq_dyn uses the default kind()-equality. Two aggregate descriptors
    // with different `arg_types` but the same `fn_name` share the same
    // kind() — that's fine for matrix purposes because the encoder
    // validates byte-shape at write time anyway.
    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(self.clone())
    }
}

/// Runtime carrier for an aggregate state — raw bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChAggregateStateValue {
    pub bytes: Vec<u8>,
    pub fn_name: String,
}

impl DynValue for ChAggregateStateValue {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(ChAggregateStateType {
            fn_name: self.fn_name.clone(),
            arg_types: Vec::new(),
            simple: false,
            kind: ChAggregateStateType::kind_for_fn(&self.fn_name),
        })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }

    fn is_equal(&self, other: &dyn DynValue) -> bool {
        match other.as_any().downcast_ref::<ChAggregateStateValue>() {
            Some(o) => self == o,
            None => false,
        }
    }

    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(self.clone())
    }

    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        Ok(serde_json::Value::String(
            BASE64_STANDARD.encode(&self.bytes),
        ))
    }
}
