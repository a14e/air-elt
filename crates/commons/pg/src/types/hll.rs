//! Postgres `hll` extension type — a HyperLogLog cardinality sketch.
//!
//! `hll` is provided by the `postgresql-hll` extension (and by Citus
//! out-of-the-box). On the wire it is an opaque `bytea`-shaped binary
//! payload; the database side computes cardinality / merges sketches
//! via SQL functions. Air Elt treats it as **identity-only** —
//! we copy bytes byte-for-byte between source and sink, no conversion
//! to/from canonical types is supported.
//!
//! Why identity-only: HLL sketches have no meaningful encoding into
//! `Bytes` / `Text` / numeric types from a user's standpoint. Anyone
//! who wants the cardinality should compute it server-side via
//! `hll_cardinality(col)`. Re-shaping the opaque payload through a
//! lossy text/bytes lens would invite silent corruption — refusing
//! the conversion is the honest answer.

use std::any::Any;

use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::default_value::DefaultParseError;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

/// The schema-side descriptor for a Postgres `hll` column.
#[derive(Debug, Clone, Copy, Default)]
pub struct PgHllType;

impl PgHllType {
    /// Single source of truth for the kind string. Every site that
    /// needs to recognise an HLL `DataType::Custom(t)` should compare
    /// against this constant rather than re-spelling the literal.
    pub const KIND: &'static str = "postgresql.hll";
}

impl DynType for PgHllType {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn can_be_cursor(&self) -> bool {
        false
    }

    /// Identity only: `hll → hll` is allowed. Every other target is a
    /// matrix-time rejection — no truncation softens this.
    fn can_convert_to(&self, target: &DataType, _truncate: bool) -> bool {
        matches!(target, DataType::Custom(t) if t.kind() == self.kind())
    }

    /// Identity only: `hll ← hll` is allowed. Every other source is
    /// a matrix-time rejection.
    fn can_construct_from(&self, src: &DataType, _truncate: bool) -> bool {
        matches!(src, DataType::Custom(t) if t.kind() == self.kind())
    }

    fn convert(
        &self,
        value: Value,
        target: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        // Identity passes through untouched. Anything else surfaces as
        // `Unsupported`; the matrix should already have rejected the
        // mapping at validation time, so reaching this arm is a bug.
        if matches!(target, DataType::Custom(t) if t.kind() == self.kind()) {
            return Ok(value);
        }
        Err(ConvertError::Unsupported {
            src: DataType::Custom(Box::new(*self)),
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
            dst: DataType::Custom(Box::new(*self)),
        })
    }

    /// HLL columns do not accept TOML defaults. There is no sensible
    /// literal grammar for an opaque cardinality sketch; the operator
    /// must populate them via SQL (`hll_empty()`, `hll_add_agg(...)`).
    fn parse_default(&self, _literal: &toml::Value) -> Result<Option<Value>, DefaultParseError> {
        Ok(None)
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(*self)
    }
}

/// Runtime carrier for a Postgres `hll` value. Bytes are the raw on-wire
/// payload; the connector's sink path appends an `::hll` cast at SQL
/// template time so the server interprets the bytea as an HLL sketch.
#[derive(Debug, Clone)]
pub struct PgHllValue(pub Vec<u8>);

impl DynValue for PgHllValue {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(PgHllType)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn eq_dyn(&self, other: &dyn DynValue) -> bool {
        match other.as_any().downcast_ref::<PgHllValue>() {
            Some(o) => self.0 == o.0,
            None => false,
        }
    }

    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn ctx() -> ConversionContext {
        ConversionContext::passthrough()
    }

    #[test]
    fn kind_is_stable() {
        assert_eq!(PgHllType.kind(), "postgresql.hll");
    }

    #[test]
    fn cannot_be_cursor() {
        assert!(!PgHllType.can_be_cursor());
    }

    #[test]
    fn identity_conversion_is_accepted() {
        let same = DataType::Custom(Box::new(PgHllType));
        assert!(PgHllType.can_convert_to(&same, false));
        assert!(PgHllType.can_convert_to(&same, true));
        assert!(PgHllType.can_construct_from(&same, false));
        assert!(PgHllType.can_construct_from(&same, true));
    }

    #[test]
    fn rejects_non_identity_targets_even_with_truncate() {
        let hll = PgHllType;
        for target in [
            DataType::Bool,
            DataType::Int64,
            DataType::Text { size: None },
            DataType::Bytes { size: None },
            DataType::Json,
            DataType::Xml,
            DataType::Uuid,
        ] {
            assert!(
                !hll.can_convert_to(&target, false),
                "must reject convert_to {target:?}"
            );
            assert!(
                !hll.can_convert_to(&target, true),
                "truncate must not soften convert_to {target:?}"
            );
            assert!(
                !hll.can_construct_from(&target, false),
                "must reject construct_from {target:?}"
            );
            assert!(
                !hll.can_construct_from(&target, true),
                "truncate must not soften construct_from {target:?}"
            );
        }
    }

    #[test]
    fn convert_identity_passes_value_through() {
        let v = Value::Custom(Box::new(PgHllValue(vec![1, 2, 3])));
        let same = DataType::Custom(Box::new(PgHllType));
        let out = PgHllType.convert(v.clone(), &same, &ctx()).unwrap();
        assert_eq!(out, v);
    }

    #[test]
    fn convert_non_identity_is_unsupported() {
        let v = Value::Custom(Box::new(PgHllValue(vec![1])));
        let err = PgHllType.convert(v, &DataType::Bytes { size: None }, &ctx());
        assert!(matches!(err, Err(ConvertError::Unsupported { .. })));
    }

    #[test]
    fn construct_non_identity_is_unsupported() {
        let v = Value::Bytes(vec![1, 2, 3]);
        let err = PgHllType.construct(v, &DataType::Bytes { size: None }, &ctx());
        assert!(matches!(err, Err(ConvertError::Unsupported { .. })));
    }

    #[test]
    fn parse_default_unsupported() {
        let lit = toml::Value::String("anything".into());
        let parsed = PgHllType.parse_default(&lit).unwrap();
        assert!(parsed.is_none(), "HLL must not accept defaults");
    }

    #[test]
    fn value_roundtrip_through_clone_box_preserves_bytes() {
        let v: Box<dyn DynValue> = Box::new(PgHllValue(vec![9, 8, 7]));
        let cloned = v.clone_box();
        assert!(v.eq_dyn(&*cloned));
    }

    #[test]
    fn value_dyn_type_returns_hll_descriptor() {
        let v = PgHllValue(vec![]);
        assert_eq!(v.dyn_type().kind(), "postgresql.hll");
    }

    #[test]
    fn value_inequality_for_distinct_bytes() {
        let a: Box<dyn DynValue> = Box::new(PgHllValue(vec![1]));
        let b: Box<dyn DynValue> = Box::new(PgHllValue(vec![2]));
        assert!(!a.eq_dyn(&*b));
    }
}
