//! Open extension points for connector-specific types.
//!
//! `DynType` is the schema-side descriptor (analogous to a [`DataType`]
//! variant); `DynValue` is the runtime-side carrier (analogous to a
//! [`Value`] variant). Concrete implementations live in the connector
//! crates (e.g. `commons-mongodb::types::object_id`,
//! `commons-pg::types::hll`) and are surfaced through the
//! [`DataType::Custom`] / [`Value::Custom`] arms.
//!
//! This avoids bloating the canonical [`DataType`] enum with
//! vendor-specific variants. The matrix and convert dispatcher delegate
//! through the trait when either side is a `Custom`.
//!
//! ## `kind()` contract
//!
//! Every `DynType` must return a stable string in the form
//! `"<vendor>.<type>"` — for example `"mongodb.object_id"`,
//! `"mongodb.javascript"`, `"postgresql.hll"`. The string is the
//! **identity** of the type for the purposes of `Eq`, `Hash`, `Ord`,
//! `Display`, and serde diagnostics. Two `Box<dyn DynType>` compare equal
//! iff their `kind()` is equal AND `is_equal` agrees. `Hash` and `Ord` are
//! computed solely from `kind()`.
//!
//! Because of that, `kind()` MUST be stable across the lifetime of the
//! process and across versions of the same connector — change the kind
//! and persisted cursor metadata + telemetry stops correlating. The
//! string is `'static` so the trait can stay object-safe and so
//! implementations don't allocate on the hot path.
//!
//! ## `is_equal` default
//!
//! The default `is_equal` compares `kind()` only. That is correct for
//! parameter-less unit types (the three concrete types we ship).
//! Any future `DynType` that carries parameters in its descriptor (e.g.
//! `Bytes { size }`-style) MUST override `is_equal` to compare those
//! parameters; otherwise the matrix would treat structurally-different
//! descriptors as identical.
//!
//! ## Soundness
//!
//! `Box<dyn DynType>` and `Box<dyn DynValue>` participate in the global
//! [`DataType`] / [`Value`] derives — `Clone`, `PartialEq`, `Hash`, `Ord`,
//! `Debug`. Auto-derive can't see through trait objects, so we wire the
//! trait methods (`clone_box`, `is_equal`, `kind`) into hand-rolled impls
//! on `Box<dyn …>` here. The downstream enums then derive normally and
//! everything just works at the `DataType`/`Value` layer.

use std::any::Any;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

use crate::convert::ConvertError;
use crate::convert::context::ConversionContext;
use crate::data_type::DataType;
use crate::error::JsonEncodeError;
use crate::value::Value;

/// Connector-defined schema-side type descriptor. See module docs for the
/// `kind()` and `is_equal` contracts.
pub trait DynType: fmt::Debug + Send + Sync + 'static {
    /// `Any` access for downcasting in connector code.
    fn as_any(&self) -> &dyn Any;

    /// Stable identity in `"<vendor>.<type>"` form. See module docs.
    fn kind(&self) -> &str;

    /// Human-readable label used by `DataType: Display`. Defaults to
    /// `kind()`.
    fn display(&self) -> String {
        self.kind().to_string()
    }

    /// Whether values of this type can serve as a cursor field —
    /// i.e. they have a canonical linear order AND
    /// [`Self::decode_cursor_value`] can round-trip them through JSON.
    /// Default `false`; implementations override to opt in. Every
    /// `DynType` returning `true` MUST also override
    /// [`Self::decode_cursor_value`].
    fn cursor_compatible(&self) -> bool {
        false
    }

    /// Decode a cursor-JSON payload back into a connector-specific
    /// [`DynValue`]. The `serde_json::Value` passed in is whatever the
    /// matching [`DynValue::to_json`] produced at serialize time.
    ///
    /// Default returns an error. Every `DynType` that returns
    /// [`Self::cursor_compatible`] `== true` MUST override this — the
    /// caller resolves the expected `DataType` from the source schema
    /// (cursor fields are looked up there) and dispatches into the
    /// matching descriptor; no global registry is consulted.
    fn decode_cursor_value(&self, _json: &serde_json::Value) -> Result<Box<dyn DynValue>, String> {
        Err(format!(
            "DynType::decode_cursor_value not implemented for kind={:?} — \
             type is not cursor-compatible",
            self.kind()
        ))
    }

    /// Whether values of this type are document/object-shaped. The
    /// Transform compiler uses this to validate that a source's
    /// `body_data_type()` produces an object — the body-fold ops
    /// (`Body`) require an object value to absorb. Default `false`;
    /// override to `true` on document-shaped custom types
    /// (e.g. `BsonObjectType`).
    fn is_object(&self) -> bool {
        false
    }

    /// Predicate consulted by the matrix when this type appears on the
    /// **source** side and the sink is a canonical `target`. Returning
    /// `true` means a runtime `convert(...)` call is expected to succeed
    /// (subject to value-shape validation). `truncate` mirrors the
    /// per-mapping `truncate` flag.
    fn can_convert_to(&self, target: &DataType, truncate: bool) -> bool;

    /// Predicate consulted by the matrix when this type appears on the
    /// **sink** side and the source is a canonical `src`. Returning
    /// `true` means a runtime `construct(...)` call is expected to
    /// succeed.
    fn can_construct_from(&self, src: &DataType, truncate: bool) -> bool;

    /// Runtime conversion for `Custom -> canonical`.
    fn convert(
        &self,
        value: Value,
        target: &DataType,
        ctx: &ConversionContext,
    ) -> Result<Value, ConvertError>;

    /// Runtime conversion for `canonical -> Custom`.
    fn construct(
        &self,
        value: Value,
        src: &DataType,
        ctx: &ConversionContext,
    ) -> Result<Value, ConvertError>;

    /// Parse a TOML default literal into a value of this type. Default
    /// returns `Ok(None)` meaning "this type does not accept defaults"
    /// — the caller wraps that as `TypeMismatch`.
    fn parse_default(&self, _literal: &toml::Value) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// Fixed byte-width for types that require exact lengths on the wire
    /// (e.g. `FixedString(N)`). Returns `None` for variable-width types.
    /// The RowBinary encoder uses this to pad or reject mismatched values.
    fn fixed_size(&self) -> Option<u32> {
        None
    }

    /// Equality between this descriptor and `other`. The default
    /// compares `kind()` only — correct for parameter-less unit types.
    /// Implementations carrying descriptor parameters MUST override.
    fn is_equal(&self, other: &dyn DynType) -> bool {
        self.kind() == other.kind()
    }

    /// Deep-clone behind a `Box<dyn DynType>`. Cheap for unit structs.
    fn clone_box(&self) -> Box<dyn DynType>;
}

/// Connector-defined runtime value carrier. Counterpart of [`DynType`].
pub trait DynValue: fmt::Debug + Send + Sync + 'static {
    /// Descriptor for this value. The runner uses this to resolve the
    /// `DataType::Custom(...)` for a `Value::Custom(...)` when the
    /// source declares a `Union` and we must pick a concrete type at
    /// dispatch time.
    fn dyn_type(&self) -> Box<dyn DynType>;

    /// `Any` access for downcasting in connector code.
    fn as_any(&self) -> &dyn Any;

    /// Owning `Any` access for moving the inner concrete value out of a
    /// `Box<dyn DynValue>` without cloning. Connectors that own the
    /// payload (raw passthrough sinks) call `Box::<dyn Any>::downcast`
    /// on the result and `*box_t` to recover the typed value. The
    /// default unimplemented signature would force every impl to spell
    /// `Box::new(*self)` — every concrete type can do that uniformly,
    /// so we keep it as a required method.
    fn into_any(self: Box<Self>) -> Box<dyn Any>;

    /// Equality with another opaque value. Implementations typically
    /// downcast via `as_any` and compare concrete fields.
    fn is_equal(&self, other: &dyn DynValue) -> bool;

    /// Deep-clone behind a `Box<dyn DynValue>`.
    fn clone_box(&self) -> Box<dyn DynValue>;

    /// Optional ordering against another opaque value of the same kind.
    /// Default `None` (unordered). Override for types that participate in
    /// expression comparisons (e.g. ObjectId).
    fn partial_cmp(&self, _other: &dyn DynValue) -> Option<std::cmp::Ordering> {
        None
    }

    /// Feed this value into a [`Hasher`](std::hash::Hasher) so that
    /// cursor-compatible custom values can be used in [`Key`](crate::Key).
    /// Default does nothing — override together with `is_equal` so that
    /// `a.is_equal(b)` implies identical hashes.
    fn hash(&self, _state: &mut dyn std::hash::Hasher) {}

    /// Encode this value as a `serde_json::Value` for the JSON
    /// auto-pack path (`*:body` mapping). Default returns
    /// `JsonEncodeError::Variant("to_json not implemented")` — every
    /// custom type that participates in JSON-pack must override.
    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        Err(JsonEncodeError::Variant(
            "to_json not implemented".to_string(),
        ))
    }
}

// ---- Box<dyn DynType> trait plumbing -----------------------------------

impl Clone for Box<dyn DynType> {
    fn clone(&self) -> Self {
        (**self).clone_box()
    }
}

impl PartialEq for Box<dyn DynType> {
    fn eq(&self, other: &Self) -> bool {
        // kind() equality is the cheap pre-check; is_equal is the
        // structural follow-up (overridden by parametric types).
        self.kind() == other.kind() && (**self).is_equal(&**other)
    }
}

impl Eq for Box<dyn DynType> {}

impl Hash for Box<dyn DynType> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Hash by kind() only. PartialEq narrows further via is_equal,
        // but Hash is permitted to bucket by kind alone — collisions
        // are resolved by Eq.
        self.kind().hash(state);
    }
}

impl PartialOrd for Box<dyn DynType> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Box<dyn DynType> {
    fn cmp(&self, other: &Self) -> Ordering {
        // Stable across versions: relies only on the documented `kind()`
        // contract. Ordering of `DataType::Custom(...)` participates in
        // `DataType::union(...)` normalisation, which sorts variants for
        // observation-order-independent equality.
        self.kind().cmp(other.kind())
    }
}

// ---- Box<dyn DynValue> trait plumbing ----------------------------------

impl Clone for Box<dyn DynValue> {
    fn clone(&self) -> Self {
        (**self).clone_box()
    }
}

impl PartialEq for Box<dyn DynValue> {
    fn eq(&self, other: &Self) -> bool {
        (**self).is_equal(&**other)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ahash::AHashMap;

    /// Minimal parameter-less `DynType` for plumbing tests.
    #[derive(Debug)]
    struct TestType;

    impl DynType for TestType {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn kind(&self) -> &str {
            "test.type_a"
        }
        fn can_convert_to(&self, _t: &DataType, _trunc: bool) -> bool {
            false
        }
        fn can_construct_from(&self, _t: &DataType, _trunc: bool) -> bool {
            false
        }
        fn convert(
            &self,
            _v: Value,
            _t: &DataType,
            _ctx: &ConversionContext,
        ) -> Result<Value, ConvertError> {
            Err(ConvertError::Unsupported {
                src: DataType::Custom(Box::new(TestType)),
                dst: DataType::Bool,
            })
        }
        fn construct(
            &self,
            _v: Value,
            _t: &DataType,
            _ctx: &ConversionContext,
        ) -> Result<Value, ConvertError> {
            Err(ConvertError::Unsupported {
                src: DataType::Bool,
                dst: DataType::Custom(Box::new(TestType)),
            })
        }
        fn clone_box(&self) -> Box<dyn DynType> {
            Box::new(TestType)
        }
    }

    /// Second test type with a different `kind()` for ordering tests.
    #[derive(Debug)]
    struct TestTypeB;

    impl DynType for TestTypeB {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn kind(&self) -> &str {
            "test.type_b"
        }
        fn can_convert_to(&self, _t: &DataType, _trunc: bool) -> bool {
            false
        }
        fn can_construct_from(&self, _t: &DataType, _trunc: bool) -> bool {
            false
        }
        fn convert(
            &self,
            _v: Value,
            _t: &DataType,
            _ctx: &ConversionContext,
        ) -> Result<Value, ConvertError> {
            Err(ConvertError::Unsupported {
                src: DataType::Custom(Box::new(TestTypeB)),
                dst: DataType::Bool,
            })
        }
        fn construct(
            &self,
            _v: Value,
            _t: &DataType,
            _ctx: &ConversionContext,
        ) -> Result<Value, ConvertError> {
            Err(ConvertError::Unsupported {
                src: DataType::Bool,
                dst: DataType::Custom(Box::new(TestTypeB)),
            })
        }
        fn clone_box(&self) -> Box<dyn DynType> {
            Box::new(TestTypeB)
        }
    }

    #[test]
    fn clone_through_box_preserves_kind() {
        let a: Box<dyn DynType> = Box::new(TestType);
        let b = a.clone();
        assert_eq!(a.kind(), b.kind());
    }

    #[test]
    fn equal_kinds_compare_equal() {
        let a: Box<dyn DynType> = Box::new(TestType);
        let b: Box<dyn DynType> = Box::new(TestType);
        assert!(a == b);
    }

    #[test]
    fn distinct_kinds_compare_unequal() {
        let a: Box<dyn DynType> = Box::new(TestType);
        let b: Box<dyn DynType> = Box::new(TestTypeB);
        assert!(a != b);
    }

    #[test]
    fn hash_buckets_by_kind() {
        let a: Box<dyn DynType> = Box::new(TestType);
        let b: Box<dyn DynType> = Box::new(TestType);
        let mut map: AHashMap<Box<dyn DynType>, u32> = AHashMap::new();
        map.insert(a, 1);
        // Same kind must look up the same bucket.
        assert_eq!(map.get(&b), Some(&1));
    }

    #[test]
    fn ord_uses_kind_string() {
        let a: Box<dyn DynType> = Box::new(TestType);
        let b: Box<dyn DynType> = Box::new(TestTypeB);
        assert!(a < b);
    }

    // ---- Property-based tests --------------------------------------

    use proptest::prelude::*;

    /// Pick one of the two stub `DynType`s by a coin flip. The pair
    /// covers the only documented contract surface (`kind()` identity
    /// is preserved through `clone_box`) — a bigger family would
    /// duplicate the unit tests above without adding signal.
    fn any_stub_type() -> impl Strategy<Value = Box<dyn DynType>> {
        prop_oneof![
            Just(Box::new(TestType) as Box<dyn DynType>),
            Just(Box::new(TestTypeB) as Box<dyn DynType>),
        ]
    }

    /// Cloning a `Box<dyn DynType>` through the `clone_box` plumbing
    /// must preserve `kind()`. This is the cross-cutting invariant the
    /// matrix and JSON encoder both rely on; a regression here would
    /// silently change `DataType::Custom(...)` identity after a clone.
    #[test_strategy::proptest(ProptestConfig::with_cases(64))]
    fn box_dyn_type_clone_preserves_kind(#[strategy(any_stub_type())] t: Box<dyn DynType>) {
        let original_kind = t.kind().to_string();
        let cloned = t.clone();
        prop_assert_eq!(cloned.kind(), &original_kind);
    }
}
