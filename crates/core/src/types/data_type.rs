use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

use serde::de::{self, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Deserializer, Serialize};

use crate::types::dynamic::DynType;

/// Canonical "pivot" type. Each connector maps native ↔ canonical, the runner
/// uses the matrix in `super::matrix` to validate compatibility.
///
/// `Text` and `Bytes` carry an optional declared size (`varchar(36)`,
/// `binary(16)`, etc.). `None` means unbounded (`text`, `mediumtext`, `blob`).
/// The size is part of the *schema*, not the *value* — `Value::Text` stores a
/// plain `String` regardless. Width is consulted only at validation time so
/// the matrix can reject narrowing pairs.
#[derive(Debug, Clone, PartialEq)]
pub enum DataType {
    Bool,
    Int8,
    Int16,
    Int32,
    Int64,
    /// Unsigned fixed-width integers. Mapped from MySQL/MariaDB
    /// `tinyint|smallint|mediumint|int|bigint UNSIGNED`. Postgres has no
    /// unsigned int types — these variants never originate from a pg column.
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    Float32,
    Float64,
    /// Arbitrary-precision integer. `width = Some(n)` means the column was
    /// declared with `numeric(n, 0)` / `decimal(n, 0)` and at most `n`
    /// decimal digits fit. `width = None` means unbounded (PG `numeric`
    /// without modifier and scale 0).
    BigInt {
        width: Option<u32>,
    },
    /// Fractional decimal. `precision`/`scale` mirror SQL `decimal(p, s)`.
    /// Both fields = `None` means fully unbounded (PG `numeric` without
    /// modifier with non-zero or unknown scale). `precision = Some(p)`
    /// implies `scale = Some(s)` and `0 ≤ s ≤ p`.
    Decimal {
        precision: Option<u32>,
        scale: Option<u32>,
    },
    Text {
        size: Option<u32>,
    },
    Bytes {
        size: Option<u32>,
    },
    Date,
    Timestamp,
    Uuid,
    Json,
    /// XML payload, carried as canonical text. Distinct from `Text` so the
    /// matrix and convert dispatcher can apply XML-specific rules
    /// (well-formedness validation on `Text → Xml`, forbidding
    /// `Xml → Xml` truncation).
    Xml,
    /// Heterogeneous source field — Mongo schema sampling produced
    /// multiple non-widening types in one column. The convert dispatcher
    /// inspects the actual `Value` variant at runtime and re-dispatches
    /// to the concrete source type. Sinks never carry `Union` (schemaful
    /// sinks declare a concrete column type; the schemaless Mongo sink
    /// inherits the source's `Union` and writes BSON via `Value` match,
    /// which doesn't consult the schema type).
    ///
    /// Variants are normalised by `union(...)`: deduplicated and sorted
    /// in `Debug` order so two equal unions compare equal regardless of
    /// observation order.
    Union(Vec<DataType>),
    /// Connector-specific opaque type. The descriptor implements
    /// [`crate::types::dynamic::DynType`] which provides the matrix and
    /// convert hooks. Cursor JSON storage MUST never see this — the
    /// validation pipeline rejects flows whose cursor field carries a
    /// non-cursorable type.
    Custom(Box<dyn DynType>),
}

impl DataType {
    /// Convenience constructor for unbounded text.
    pub const fn text() -> Self {
        DataType::Text { size: None }
    }

    /// Convenience constructor for unbounded bytes.
    pub const fn bytes() -> Self {
        DataType::Bytes { size: None }
    }

    /// Build a normalised type from a multiset of observed members.
    ///
    /// Thin wrapper over
    /// [`crate::types::union_types::collapse_union`]: flattens nested
    /// `Union(...)` inputs, widens any same-kind family (Int/UInt/
    /// Float/Text/Bytes) where the matrix permits, and otherwise
    /// returns a sorted + dedup'd `DataType::Union` so equality is
    /// observation-order-independent. A 1-element input (after
    /// normalisation) collapses to the bare variant; an empty input
    /// returns `DataType::Union(Vec::new())`.
    ///
    /// Accepts any `IntoIterator<Item = DataType>` so callers that
    /// already have an iterator (e.g. schema-inference folds) avoid
    /// materialising a `Vec` just to call this. `Vec<DataType>`
    /// satisfies the bound directly, so existing call sites keep
    /// compiling unchanged.
    pub fn union<I>(vs: I) -> Self
    where
        I: IntoIterator<Item = DataType>,
    {
        crate::types::union_types::collapse_union(vs)
    }

    /// Whether this type is admissible as a cursor field. Any type
    /// with a canonical linear order qualifies — numerics, `Text`,
    /// `Bytes`, `Bool`, `Date`, `Timestamp`, `Uuid`. `Json` / `Xml`
    /// / `Union` are rejected (no total order); custom types delegate
    /// to [`DynType::cursor_compatible`].
    pub fn cursor_compatible(&self) -> bool {
        match self {
            DataType::Json | DataType::Xml | DataType::Union(_) => false,
            DataType::Custom(t) => t.cursor_compatible(),
            _ => true,
        }
    }

    /// Decode a cursor-JSON value back into [`crate::types::Value`]
    /// using **this** descriptor as the expected type. The cursor
    /// caller (storage layer) resolves the expected `DataType` for
    /// each cursor field from the source schema; no global registry is
    /// consulted.
    ///
    /// Canonical variants delegate to the standard
    /// `serde_json::from_value::<Value>(...)` path — non-Custom values
    /// already self-describe in the JSON envelope and round-trip via
    /// the `Value` serde impl. `Custom(t)` parses the
    /// `{"type":"custom","kind":...,"value":...}` envelope, asserts the
    /// kind matches `t.kind()`, and delegates payload decode to
    /// [`crate::types::dynamic::DynType::decode_cursor_value`].
    pub fn decode_cursor_json(
        &self,
        json: serde_json::Value,
    ) -> Result<crate::types::Value, String> {
        use crate::types::Value;
        if let DataType::Custom(t) = self {
            // Envelope shape: { "type":"custom", "kind":"<kind>", "value": ... }.
            // Validate the kind matches the expected descriptor before
            // delegating — a mismatch means the persisted cursor type
            // drifted from the source schema and the caller should
            // resync rather than silently mis-decode.
            let obj = json.as_object().ok_or_else(|| {
                format!(
                    "expected an object envelope for cursor Value::Custom (kind={:?}), got {json}",
                    t.kind()
                )
            })?;
            let tag = obj
                .get("type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "cursor envelope missing string field `type`".to_string())?;
            if tag != "custom" {
                return Err(format!(
                    "cursor envelope tag {tag:?} does not match expected DataType::Custom \
                     (kind={:?})",
                    t.kind()
                ));
            }
            let kind = obj.get("kind").and_then(|v| v.as_str()).ok_or_else(|| {
                "cursor `custom` envelope missing string field `kind`".to_string()
            })?;
            if kind != t.kind() {
                return Err(format!(
                    "cursor envelope kind {kind:?} does not match expected DynType kind {:?}",
                    t.kind()
                ));
            }
            let payload = obj.get("value").cloned().unwrap_or(serde_json::Value::Null);
            let inner = t.decode_cursor_value(&payload)?;
            return Ok(Value::Custom(inner));
        }
        // Canonical variants: trust the tagged envelope on the wire
        // and let `Value::Deserialize` parse it. The type-check on the
        // source schema side is what gives us this dispatch — we don't
        // strictly need to assert that `self == parsed.data_type()`
        // here because a mismatch is already a bug in the caller's
        // source-schema bookkeeping.
        serde_json::from_value::<Value>(json).map_err(|e| e.to_string())
    }

    /// Whether this type is document/object-shaped. Required by the
    /// Transform compiler for `Body` ops — only object-shaped source
    /// bodies can be absorbed. `Json` is hard-coded to `true`; custom
    /// types delegate to [`DynType::is_object`].
    pub fn is_object(&self) -> bool {
        match self {
            DataType::Json => true,
            DataType::Custom(t) => t.is_object(),
            _ => false,
        }
    }
}

impl Eq for DataType {}

impl Hash for DataType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Discriminant first so distinct variants never collide; then
        // payload. `Box<dyn DynType>` hashes by `kind()` (see
        // `dynamic.rs`), which is the documented identity.
        std::mem::discriminant(self).hash(state);
        match self {
            DataType::Bool
            | DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::Float32
            | DataType::Float64
            | DataType::Date
            | DataType::Timestamp
            | DataType::Uuid
            | DataType::Json
            | DataType::Xml => {}
            DataType::BigInt { width } => width.hash(state),
            DataType::Decimal { precision, scale } => {
                precision.hash(state);
                scale.hash(state);
            }
            DataType::Text { size } | DataType::Bytes { size } => size.hash(state),
            DataType::Union(vs) => vs.hash(state),
            DataType::Custom(t) => t.hash(state),
        }
    }
}

impl PartialOrd for DataType {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DataType {
    fn cmp(&self, other: &Self) -> Ordering {
        let lhs = variant_order(self);
        let rhs = variant_order(other);
        match lhs.cmp(&rhs) {
            Ordering::Equal => {}
            ord => return ord,
        }
        match (self, other) {
            (DataType::BigInt { width: a }, DataType::BigInt { width: b }) => a.cmp(b),
            (
                DataType::Decimal {
                    precision: pa,
                    scale: sa,
                },
                DataType::Decimal {
                    precision: pb,
                    scale: sb,
                },
            ) => pa.cmp(pb).then(sa.cmp(sb)),
            (DataType::Text { size: a }, DataType::Text { size: b }) => a.cmp(b),
            (DataType::Bytes { size: a }, DataType::Bytes { size: b }) => a.cmp(b),
            (DataType::Union(a), DataType::Union(b)) => a.cmp(b),
            (DataType::Custom(a), DataType::Custom(b)) => a.cmp(b),
            // Unit variants — equal once their discriminant matches.
            _ => Ordering::Equal,
        }
    }
}

/// Stable order key for `DataType` variants. Mirrors the original
/// derived-`Ord` declaration order so existing union normalisation and
/// any persisted ordering stay byte-identical.
fn variant_order(d: &DataType) -> u8 {
    match d {
        DataType::Bool => 0,
        DataType::Int8 => 1,
        DataType::Int16 => 2,
        DataType::Int32 => 3,
        DataType::Int64 => 4,
        DataType::UInt8 => 5,
        DataType::UInt16 => 6,
        DataType::UInt32 => 7,
        DataType::UInt64 => 8,
        DataType::Float32 => 9,
        DataType::Float64 => 10,
        DataType::BigInt { .. } => 11,
        DataType::Decimal { .. } => 12,
        DataType::Text { .. } => 13,
        DataType::Bytes { .. } => 14,
        DataType::Date => 15,
        DataType::Timestamp => 16,
        DataType::Uuid => 17,
        DataType::Json => 18,
        DataType::Xml => 19,
        DataType::Union(_) => 20,
        DataType::Custom(_) => 21,
    }
}

impl std::fmt::Display for DataType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataType::Bool => f.write_str("bool"),
            DataType::Int8 => f.write_str("int8"),
            DataType::Int16 => f.write_str("int16"),
            DataType::Int32 => f.write_str("int32"),
            DataType::Int64 => f.write_str("int64"),
            DataType::UInt8 => f.write_str("uint8"),
            DataType::UInt16 => f.write_str("uint16"),
            DataType::UInt32 => f.write_str("uint32"),
            DataType::UInt64 => f.write_str("uint64"),
            DataType::Float32 => f.write_str("float32"),
            DataType::Float64 => f.write_str("float64"),
            DataType::BigInt { width: None } => f.write_str("bigint"),
            DataType::BigInt { width: Some(n) } => write!(f, "bigint({n})"),
            DataType::Decimal {
                precision: None,
                scale: _,
            } => f.write_str("decimal"),
            DataType::Decimal {
                precision: Some(p),
                scale: None,
            } => write!(f, "decimal({p})"),
            DataType::Decimal {
                precision: Some(p),
                scale: Some(s),
            } => write!(f, "decimal({p},{s})"),
            DataType::Text { size: None } => f.write_str("text"),
            DataType::Text { size: Some(n) } => write!(f, "text({n})"),
            DataType::Bytes { size: None } => f.write_str("bytes"),
            DataType::Bytes { size: Some(n) } => write!(f, "bytes({n})"),
            DataType::Date => f.write_str("date"),
            DataType::Timestamp => f.write_str("timestamp"),
            DataType::Uuid => f.write_str("uuid"),
            DataType::Json => f.write_str("json"),
            DataType::Xml => f.write_str("xml"),
            DataType::Union(vs) => {
                f.write_str("union<")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        f.write_str("|")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str(">")
            }
            DataType::Custom(t) => f.write_str(&t.display()),
        }
    }
}

// ---- Hand-rolled Serialize/Deserialize ---------------------------------
//
// Format mirrors the prior `#[derive(Serialize, Deserialize)]
// #[serde(rename_all = "snake_case")]` byte-for-byte: a JSON string for
// unit variants (`"bool"`, `"date"`, ...) and a single-key map for
// payload-bearing variants (`{"big_int": {"width": 5}}` etc.).
//
// `Custom` is a special case. The plan calls for emitting
// `{"type":"custom","kind":"<kind>"}` for telemetry. To preserve binary
// compatibility for non-Custom variants we keep the plain shape for
// them and use the same single-key map shape for Custom too:
// `{"custom":{"kind":"<kind>"}}`. That matches the plan's intent
// (`Serialize emits ... "custom" ... "kind" ...`) while keeping the
// serde "internally-tagged" feel consistent with the rest of the enum.
// Deserialize errors on `"custom"` because the kind cannot be resolved
// to a `Box<dyn DynType>` without a registry.

impl Serialize for DataType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            DataType::Bool => serializer.serialize_str("bool"),
            DataType::Int8 => serializer.serialize_str("int8"),
            DataType::Int16 => serializer.serialize_str("int16"),
            DataType::Int32 => serializer.serialize_str("int32"),
            DataType::Int64 => serializer.serialize_str("int64"),
            DataType::UInt8 => serializer.serialize_str("u_int8"),
            DataType::UInt16 => serializer.serialize_str("u_int16"),
            DataType::UInt32 => serializer.serialize_str("u_int32"),
            DataType::UInt64 => serializer.serialize_str("u_int64"),
            DataType::Float32 => serializer.serialize_str("float32"),
            DataType::Float64 => serializer.serialize_str("float64"),
            DataType::Date => serializer.serialize_str("date"),
            DataType::Timestamp => serializer.serialize_str("timestamp"),
            DataType::Uuid => serializer.serialize_str("uuid"),
            DataType::Json => serializer.serialize_str("json"),
            DataType::Xml => serializer.serialize_str("xml"),
            DataType::BigInt { width } => {
                let mut m = serializer.serialize_map(Some(1))?;
                m.serialize_entry("big_int", &BigIntPayload { width: *width })?;
                m.end()
            }
            DataType::Decimal { precision, scale } => {
                let mut m = serializer.serialize_map(Some(1))?;
                m.serialize_entry(
                    "decimal",
                    &DecimalPayload {
                        precision: *precision,
                        scale: *scale,
                    },
                )?;
                m.end()
            }
            DataType::Text { size } => {
                let mut m = serializer.serialize_map(Some(1))?;
                m.serialize_entry("text", &SizePayload { size: *size })?;
                m.end()
            }
            DataType::Bytes { size } => {
                let mut m = serializer.serialize_map(Some(1))?;
                m.serialize_entry("bytes", &SizePayload { size: *size })?;
                m.end()
            }
            DataType::Union(vs) => {
                let mut m = serializer.serialize_map(Some(1))?;
                m.serialize_entry("union", vs)?;
                m.end()
            }
            DataType::Custom(t) => {
                let mut m = serializer.serialize_map(Some(1))?;
                m.serialize_entry("custom", &CustomPayload { kind: t.kind() })?;
                m.end()
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
struct BigIntPayload {
    width: Option<u32>,
}

#[derive(Serialize, Deserialize)]
struct DecimalPayload {
    precision: Option<u32>,
    scale: Option<u32>,
}

#[derive(Serialize, Deserialize)]
struct SizePayload {
    size: Option<u32>,
}

#[derive(Serialize)]
struct CustomPayload<'a> {
    kind: &'a str,
}

impl<'de> Deserialize<'de> for DataType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(DataTypeVisitor)
    }
}

struct DataTypeVisitor;

impl<'de> Visitor<'de> for DataTypeVisitor {
    type Value = DataType;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a DataType tag string or single-key map")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<DataType, E> {
        match v {
            "bool" => Ok(DataType::Bool),
            "int8" => Ok(DataType::Int8),
            "int16" => Ok(DataType::Int16),
            "int32" => Ok(DataType::Int32),
            "int64" => Ok(DataType::Int64),
            "u_int8" => Ok(DataType::UInt8),
            "u_int16" => Ok(DataType::UInt16),
            "u_int32" => Ok(DataType::UInt32),
            "u_int64" => Ok(DataType::UInt64),
            "float32" => Ok(DataType::Float32),
            "float64" => Ok(DataType::Float64),
            "date" => Ok(DataType::Date),
            "timestamp" => Ok(DataType::Timestamp),
            "uuid" => Ok(DataType::Uuid),
            "json" => Ok(DataType::Json),
            "xml" => Ok(DataType::Xml),
            // Payload-bearing variants must arrive as a map; if a bare
            // string slips through we report the same error serde_derive
            // would have produced.
            other => Err(de::Error::unknown_variant(
                other,
                &[
                    "bool",
                    "int8",
                    "int16",
                    "int32",
                    "int64",
                    "u_int8",
                    "u_int16",
                    "u_int32",
                    "u_int64",
                    "float32",
                    "float64",
                    "big_int",
                    "decimal",
                    "text",
                    "bytes",
                    "date",
                    "timestamp",
                    "uuid",
                    "json",
                    "xml",
                    "union",
                ],
            )),
        }
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<DataType, E> {
        self.visit_str(&v)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<DataType, A::Error> {
        let key: String = map
            .next_key()?
            .ok_or_else(|| de::Error::custom("DataType map missing key"))?;
        match key.as_str() {
            "big_int" => {
                let p: BigIntPayload = map.next_value()?;
                Ok(DataType::BigInt { width: p.width })
            }
            "decimal" => {
                let p: DecimalPayload = map.next_value()?;
                Ok(DataType::Decimal {
                    precision: p.precision,
                    scale: p.scale,
                })
            }
            "text" => {
                let p: SizePayload = map.next_value()?;
                Ok(DataType::Text { size: p.size })
            }
            "bytes" => {
                let p: SizePayload = map.next_value()?;
                Ok(DataType::Bytes { size: p.size })
            }
            "union" => {
                let vs: Vec<DataType> = map.next_value()?;
                Ok(DataType::Union(vs))
            }
            "custom" => Err(de::Error::custom(
                "DataType::Custom cannot be deserialized — \
                 connector-specific types have no global registry; \
                 cursor JSON storage must never carry a Custom type",
            )),
            other => Err(de::Error::unknown_variant(
                other,
                &["big_int", "decimal", "text", "bytes", "union"],
            )),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::any::Any;

    use chrono::TimeZone;

    use super::*;
    use crate::types::convert::ConvertError;
    use crate::types::convert::context::ConversionContext;
    use crate::types::default_value::DefaultParseError;
    use crate::types::value::Value;

    /// Test-only Custom type with `cursor_compatible = true` so `cursor_compatible`
    /// arm coverage is unambiguous.
    #[derive(Debug)]
    struct TestCursorable;

    impl DynType for TestCursorable {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn kind(&self) -> &str {
            "test.cursorable"
        }
        fn cursor_compatible(&self) -> bool {
            true
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
            unimplemented!()
        }
        fn construct(
            &self,
            _v: Value,
            _t: &DataType,
            _ctx: &ConversionContext,
        ) -> Result<Value, ConvertError> {
            unimplemented!()
        }
        fn parse_default(&self, _lit: &toml::Value) -> Result<Option<Value>, DefaultParseError> {
            Ok(None)
        }
        fn clone_box(&self) -> Box<dyn DynType> {
            Box::new(TestCursorable)
        }
    }

    #[derive(Debug)]
    struct TestNonCursorable;

    impl DynType for TestNonCursorable {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn kind(&self) -> &str {
            "test.non_cursorable"
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
            unimplemented!()
        }
        fn construct(
            &self,
            _v: Value,
            _t: &DataType,
            _ctx: &ConversionContext,
        ) -> Result<Value, ConvertError> {
            unimplemented!()
        }
        fn clone_box(&self) -> Box<dyn DynType> {
            Box::new(TestNonCursorable)
        }
    }

    /// Regression: each non-Custom variant serialises to a hand-spelled
    /// JSON value. Any future drift in the wire format breaks this
    /// test, which is critical because cursor JSON storage already
    /// relies on the pre-Custom shape.
    #[test]
    fn serde_binary_compat_per_variant() {
        let cases: Vec<(DataType, serde_json::Value)> = vec![
            (DataType::Bool, serde_json::json!("bool")),
            (DataType::Int8, serde_json::json!("int8")),
            (DataType::Int16, serde_json::json!("int16")),
            (DataType::Int32, serde_json::json!("int32")),
            (DataType::Int64, serde_json::json!("int64")),
            (DataType::UInt8, serde_json::json!("u_int8")),
            (DataType::UInt16, serde_json::json!("u_int16")),
            (DataType::UInt32, serde_json::json!("u_int32")),
            (DataType::UInt64, serde_json::json!("u_int64")),
            (DataType::Float32, serde_json::json!("float32")),
            (DataType::Float64, serde_json::json!("float64")),
            (DataType::Date, serde_json::json!("date")),
            (DataType::Timestamp, serde_json::json!("timestamp")),
            (DataType::Uuid, serde_json::json!("uuid")),
            (DataType::Json, serde_json::json!("json")),
            (DataType::Xml, serde_json::json!("xml")),
            (
                DataType::BigInt { width: None },
                serde_json::json!({"big_int": {"width": null}}),
            ),
            (
                DataType::BigInt { width: Some(20) },
                serde_json::json!({"big_int": {"width": 20}}),
            ),
            (
                DataType::Decimal {
                    precision: Some(10),
                    scale: Some(2),
                },
                serde_json::json!({"decimal": {"precision": 10, "scale": 2}}),
            ),
            (
                DataType::Decimal {
                    precision: None,
                    scale: None,
                },
                serde_json::json!({"decimal": {"precision": null, "scale": null}}),
            ),
            (
                DataType::Text { size: None },
                serde_json::json!({"text": {"size": null}}),
            ),
            (
                DataType::Text { size: Some(36) },
                serde_json::json!({"text": {"size": 36}}),
            ),
            (
                DataType::Bytes { size: None },
                serde_json::json!({"bytes": {"size": null}}),
            ),
            (
                DataType::Bytes { size: Some(16) },
                serde_json::json!({"bytes": {"size": 16}}),
            ),
            (
                DataType::Union(vec![DataType::Int32, DataType::Int64]),
                serde_json::json!({"union": ["int32", "int64"]}),
            ),
        ];
        for (variant, expected) in cases {
            let got = serde_json::to_value(&variant).unwrap();
            assert_eq!(got, expected, "serialise mismatch for {variant:?}");
            let round: DataType = serde_json::from_value(expected).unwrap();
            assert_eq!(round, variant, "round-trip mismatch for {variant:?}");
        }
    }

    #[test]
    fn custom_serialize_includes_kind() {
        let t = DataType::Custom(Box::new(TestCursorable));
        let got = serde_json::to_value(&t).unwrap();
        assert_eq!(
            got,
            serde_json::json!({"custom": {"kind": "test.cursorable"}})
        );
    }

    #[test]
    fn custom_deserialize_errors() {
        let raw = serde_json::json!({"custom": {"kind": "test.cursorable"}});
        let res: Result<DataType, _> = serde_json::from_value(raw);
        assert!(res.is_err(), "Custom must not deserialize without registry");
    }

    #[test]
    fn cursor_compatible_basic_variants() {
        assert!(DataType::Int32.cursor_compatible());
        assert!(DataType::Text { size: None }.cursor_compatible());
        assert!(DataType::Uuid.cursor_compatible());
        assert!(DataType::Date.cursor_compatible());
        assert!(DataType::Timestamp.cursor_compatible());
    }

    #[test]
    fn cursor_compatible_rejects_unordered() {
        assert!(!DataType::Json.cursor_compatible());
        assert!(!DataType::Xml.cursor_compatible());
        assert!(!DataType::Union(vec![DataType::Int32, DataType::Int64]).cursor_compatible());
    }

    #[test]
    fn cursor_compatible_delegates_to_dyn_type() {
        let ok = DataType::Custom(Box::new(TestCursorable));
        let no = DataType::Custom(Box::new(TestNonCursorable));
        assert!(ok.cursor_compatible());
        assert!(!no.cursor_compatible());
    }

    /// Cursor-decode round-trip for canonical types: the typed entry
    /// path on `DataType` parses each tagged envelope back into the
    /// matching `Value` variant without consulting any registry. The
    /// test pairs a representative sample of the variants `Value`
    /// supports.
    #[test]
    fn decode_cursor_json_canonical_variants() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap();
        let ts = chrono::Utc.with_ymd_and_hms(2026, 4, 22, 12, 0, 0).unwrap();
        let cases: Vec<(DataType, Value)> = vec![
            (DataType::Int64, Value::Int64(42)),
            (DataType::Text { size: None }, Value::Text("hi".into())),
            (DataType::Date, Value::Date(date)),
            (DataType::Timestamp, Value::Timestamp(ts)),
            (
                DataType::Json,
                Value::Json(serde_json::json!({"nested": 1})),
            ),
        ];
        for (dt, v) in cases {
            let envelope = serde_json::to_value(&v).expect("serialize");
            let decoded = dt.decode_cursor_json(envelope).expect("decode");
            assert_eq!(decoded, v, "round-trip mismatch for {dt:?}");
        }
    }

    /// A cursor-compatible Custom type that mirrors its payload as a
    /// `u32`. The `decode_cursor_value` override is the registry-free
    /// reload site exercised by `DataType::decode_cursor_json`.
    #[derive(Debug, Clone, Copy)]
    struct DecodableCustom;

    impl DynType for DecodableCustom {
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn kind(&self) -> &str {
            "test.decodable"
        }
        fn cursor_compatible(&self) -> bool {
            true
        }
        fn decode_cursor_value(
            &self,
            json: &serde_json::Value,
        ) -> Result<Box<dyn crate::types::dynamic::DynValue>, String> {
            let n = json
                .as_u64()
                .ok_or_else(|| format!("expected u64 for {}", self.kind()))?;
            Ok(Box::new(DecodableValue(n as u32)))
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
            unimplemented!()
        }
        fn construct(
            &self,
            _v: Value,
            _t: &DataType,
            _ctx: &ConversionContext,
        ) -> Result<Value, ConvertError> {
            unimplemented!()
        }
        fn clone_box(&self) -> Box<dyn DynType> {
            Box::new(*self)
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct DecodableValue(u32);

    impl crate::types::dynamic::DynValue for DecodableValue {
        fn dyn_type(&self) -> Box<dyn DynType> {
            Box::new(DecodableCustom)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn into_any(self: Box<Self>) -> Box<dyn Any> {
            self
        }
        fn eq_dyn(&self, other: &dyn crate::types::dynamic::DynValue) -> bool {
            other
                .as_any()
                .downcast_ref::<DecodableValue>()
                .is_some_and(|o| o.0 == self.0)
        }
        fn clone_box(&self) -> Box<dyn crate::types::dynamic::DynValue> {
            Box::new(self.clone())
        }
        fn to_json(&self) -> Result<serde_json::Value, crate::error::JsonEncodeError> {
            Ok(serde_json::Value::from(self.0))
        }
    }

    /// Round-trip: serialise a cursor-compatible Custom through the
    /// `Value` envelope, then recover it through
    /// `DataType::decode_cursor_json` — no global registry, the
    /// expected descriptor is supplied by the caller.
    #[test]
    fn decode_cursor_json_custom_round_trips() {
        let original = Value::Custom(Box::new(DecodableValue(7)));
        let envelope = serde_json::to_value(&original).expect("serialize");
        assert_eq!(
            envelope,
            serde_json::json!({
                "type": "custom",
                "kind": "test.decodable",
                "value": 7,
            })
        );
        let dt = DataType::Custom(Box::new(DecodableCustom));
        let decoded = dt.decode_cursor_json(envelope).expect("decode");
        assert_eq!(decoded, original);
    }

    /// The default `decode_cursor_value` errors. A
    /// cursor-incompatible Custom plumbed through
    /// `DataType::decode_cursor_json` surfaces that error rather than
    /// silently accepting whatever the envelope carries.
    #[test]
    fn decode_cursor_json_errors_for_non_cursor_compatible_custom() {
        let dt = DataType::Custom(Box::new(TestNonCursorable));
        let envelope = serde_json::json!({
            "type": "custom",
            "kind": "test.non_cursorable",
            "value": 1,
        });
        let res = dt.decode_cursor_json(envelope);
        assert!(
            res.is_err(),
            "default DynType::decode_cursor_value must reject the call"
        );
    }

    /// Mismatched kind in the persisted envelope vs. expected
    /// descriptor surfaces a clean error — cursor type drift between
    /// the source schema and the cursor row should fail loud rather
    /// than silently mis-decode.
    #[test]
    fn decode_cursor_json_rejects_kind_mismatch() {
        let dt = DataType::Custom(Box::new(DecodableCustom));
        let envelope = serde_json::json!({
            "type": "custom",
            "kind": "test.someone_else",
            "value": 7,
        });
        let res = dt.decode_cursor_json(envelope);
        assert!(res.is_err(), "kind mismatch must error");
    }
}
