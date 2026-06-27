use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

use serde::de::{self, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Deserializer, Serialize};

use crate::dynamic::DynType;

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
    /// IPv4 host address. Carried as `std::net::Ipv4Addr` (4 octets,
    /// network byte order). Cursor-compatible (total numeric order).
    Ipv4,
    /// IPv6 host address. Carried as `std::net::Ipv6Addr` (16 octets,
    /// network byte order). Cursor-compatible (total numeric order).
    Ipv6,
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
    /// Structured key-value document (superset of BSON/JSON). Keys are
    /// always strings, values can be any type. Serialisable to JSON, BSON,
    /// or string representation.
    Object,
    /// Homogeneous ordered list. The element type lives on the *type*,
    /// not the value, so it is fixed at compile time and erased at the
    /// sink boundary (Java-style generics): `element = None` means
    /// empty/unknown (`[]`) and unifies with any concrete element type.
    /// `element_nullable` records whether elements may be `Null`; it must
    /// live here (not only on the expr-layer `NullableExprType`) because
    /// ClickHouse distinguishes `Array(T)` from `Array(Nullable(T))` and
    /// that distinction has to survive to the sink, where only the
    /// canonical `DataType` is available. The runtime payload is
    /// [`crate::Value::Array`]. Not cursor/switch/dedup-compatible.
    Array {
        element: Option<Box<DataType>>,
        element_nullable: bool,
    },
    /// Time span / duration. The value is carried as `std::time::Duration`
    /// in `Value::Interval`. Authored in configs as a duration literal
    /// (`10s`, `1h30m`, `PT1H30M`). Deliberately minimal: it has no
    /// conversion arms in the matrix (identity-only), is not cursor- or
    /// switch-compatible, and exists today solely to type the Redis sink
    /// `ttl` column.
    Interval,
    /// Connector-specific opaque type. The descriptor implements
    /// [`crate::dynamic::DynType`] which provides the matrix and
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
    /// [`crate::union_types::collapse_union`]: flattens nested
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
        crate::union_types::collapse_union(vs)
    }

    /// Whether this type is admissible as a cursor field. Any type
    /// with a canonical linear order qualifies — numerics, `Text`,
    /// `Bytes`, `Bool`, `Date`, `Timestamp`, `Uuid`. `Json` / `Xml`
    /// / `Union` are rejected (no total order); custom types delegate
    /// to [`DynType::cursor_compatible`].
    pub fn cursor_compatible(&self) -> bool {
        match self {
            DataType::Json
            | DataType::Xml
            | DataType::Object
            | DataType::Interval
            | DataType::Array { .. }
            | DataType::Union(_) => false,
            DataType::Custom(t) => t.cursor_compatible(),
            _ => true,
        }
    }

    /// Decode a cursor-JSON value back into [`crate::Value`]
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
    /// [`crate::dynamic::DynType::decode_cursor_value`].
    pub fn decode_cursor_json(&self, json: serde_json::Value) -> Result<crate::Value, String> {
        use crate::Value;
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
            DataType::Json | DataType::Object => true,
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
            | DataType::Ipv4
            | DataType::Ipv6
            | DataType::Json
            | DataType::Xml
            | DataType::Object
            | DataType::Interval => {}
            DataType::BigInt { width } => width.hash(state),
            DataType::Decimal { precision, scale } => {
                precision.hash(state);
                scale.hash(state);
            }
            DataType::Text { size } | DataType::Bytes { size } => size.hash(state),
            DataType::Union(vs) => vs.hash(state),
            DataType::Array {
                element,
                element_nullable,
            } => {
                element.hash(state);
                element_nullable.hash(state);
            }
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
            (
                DataType::Array {
                    element: left_element,
                    element_nullable: left_nullable,
                },
                DataType::Array {
                    element: right_element,
                    element_nullable: right_nullable,
                },
            ) => left_element
                .cmp(right_element)
                .then(left_nullable.cmp(right_nullable)),
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
        DataType::Ipv4 => 18,
        DataType::Ipv6 => 19,
        DataType::Json => 20,
        DataType::Xml => 21,
        DataType::Object => 22,
        DataType::Union(_) => 23,
        DataType::Custom(_) => 24,
        DataType::Interval => 25,
        DataType::Array { .. } => 26,
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
            DataType::Ipv4 => f.write_str("ipv4"),
            DataType::Ipv6 => f.write_str("ipv6"),
            DataType::Json => f.write_str("json"),
            DataType::Xml => f.write_str("xml"),
            DataType::Object => f.write_str("object"),
            DataType::Array {
                element,
                element_nullable,
            } => {
                f.write_str("array<")?;
                match element {
                    Some(e) => write!(f, "{e}")?,
                    None => f.write_str("?")?,
                }
                if *element_nullable {
                    f.write_str("?")?;
                }
                f.write_str(">")
            }
            DataType::Interval => f.write_str("interval"),
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
            DataType::Ipv4 => serializer.serialize_str("ipv4"),
            DataType::Ipv6 => serializer.serialize_str("ipv6"),
            DataType::Json => serializer.serialize_str("json"),
            DataType::Xml => serializer.serialize_str("xml"),
            DataType::Object => serializer.serialize_str("object"),
            DataType::Interval => serializer.serialize_str("interval"),
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
            DataType::Array {
                element,
                element_nullable,
            } => {
                let mut m = serializer.serialize_map(Some(1))?;
                m.serialize_entry(
                    "array",
                    &ArrayPayloadRef {
                        element: element.as_deref(),
                        element_nullable: *element_nullable,
                    },
                )?;
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

/// Borrowing payload for `DataType::Array` serialization. `element` is
/// flattened from `Option<Box<DataType>>` to `Option<&DataType>` via
/// `as_deref` so the boxed element serializes inline.
#[derive(Serialize)]
struct ArrayPayloadRef<'a> {
    element: Option<&'a DataType>,
    element_nullable: bool,
}

/// Owned mirror of [`ArrayPayloadRef`] for deserialization; the element
/// is re-boxed into the enum variant afterwards.
#[derive(Deserialize)]
struct ArrayPayloadOwned {
    element: Option<DataType>,
    element_nullable: bool,
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
            "ipv4" => Ok(DataType::Ipv4),
            "ipv6" => Ok(DataType::Ipv6),
            "json" => Ok(DataType::Json),
            "xml" => Ok(DataType::Xml),
            "object" => Ok(DataType::Object),
            "interval" => Ok(DataType::Interval),
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
                    "ipv4",
                    "ipv6",
                    "json",
                    "xml",
                    "object",
                    "interval",
                    "union",
                    "array",
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
            "array" => {
                let p: ArrayPayloadOwned = map.next_value()?;
                Ok(DataType::Array {
                    element: p.element.map(Box::new),
                    element_nullable: p.element_nullable,
                })
            }
            "custom" => Err(de::Error::custom(
                "DataType::Custom cannot be deserialized — \
                 connector-specific types have no global registry; \
                 cursor JSON storage must never carry a Custom type",
            )),
            other => Err(de::Error::unknown_variant(
                other,
                &["big_int", "decimal", "text", "bytes", "union", "array"],
            )),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::any::Any;

    use super::*;
    use crate::convert::ConvertError;
    use crate::convert::context::ConversionContext;
    use crate::value::Value;

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
        fn parse_default(&self, _lit: &toml::Value) -> Result<Option<Value>, String> {
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
            (DataType::Ipv4, serde_json::json!("ipv4")),
            (DataType::Ipv6, serde_json::json!("ipv6")),
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
            (
                DataType::Array {
                    element: Some(Box::new(DataType::Int32)),
                    element_nullable: false,
                },
                serde_json::json!({"array": {"element": "int32", "element_nullable": false}}),
            ),
            (
                DataType::Array {
                    element: Some(Box::new(DataType::Text { size: None })),
                    element_nullable: true,
                },
                serde_json::json!(
                    {"array": {"element": {"text": {"size": null}}, "element_nullable": true}}
                ),
            ),
            (
                DataType::Array {
                    element: None,
                    element_nullable: false,
                },
                serde_json::json!({"array": {"element": null, "element_nullable": false}}),
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
        assert!(DataType::Ipv4.cursor_compatible());
        assert!(DataType::Ipv6.cursor_compatible());
    }

    #[test]
    fn cursor_compatible_rejects_unordered() {
        assert!(!DataType::Json.cursor_compatible());
        assert!(!DataType::Xml.cursor_compatible());
        assert!(!DataType::Union(vec![DataType::Int32, DataType::Int64]).cursor_compatible());
        assert!(
            !DataType::Array {
                element: Some(Box::new(DataType::Int32)),
                element_nullable: false,
            }
            .cursor_compatible()
        );
    }

    #[test]
    fn cursor_compatible_delegates_to_dyn_type() {
        let ok = DataType::Custom(Box::new(TestCursorable));
        let no = DataType::Custom(Box::new(TestNonCursorable));
        assert!(ok.cursor_compatible());
        assert!(!no.cursor_compatible());
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
        ) -> Result<Box<dyn crate::dynamic::DynValue>, String> {
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

    impl crate::dynamic::DynValue for DecodableValue {
        fn dyn_type(&self) -> Box<dyn DynType> {
            Box::new(DecodableCustom)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn into_any(self: Box<Self>) -> Box<dyn Any> {
            self
        }
        fn is_equal(&self, other: &dyn crate::dynamic::DynValue) -> bool {
            other
                .as_any()
                .downcast_ref::<DecodableValue>()
                .is_some_and(|o| o.0 == self.0)
        }
        fn clone_box(&self) -> Box<dyn crate::dynamic::DynValue> {
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

    // ---- Property-based tests --------------------------------------

    use proptest::prelude::*;

    /// Strategy yielding any non-`Custom`, non-`Union` `DataType`,
    /// covering both unit-shaped variants and the parametric ones.
    fn any_simple_data_type() -> impl Strategy<Value = DataType> {
        prop_oneof![
            Just(DataType::Bool),
            Just(DataType::Int8),
            Just(DataType::Int16),
            Just(DataType::Int32),
            Just(DataType::Int64),
            Just(DataType::UInt8),
            Just(DataType::UInt16),
            Just(DataType::UInt32),
            Just(DataType::UInt64),
            Just(DataType::Float32),
            Just(DataType::Float64),
            Just(DataType::Date),
            Just(DataType::Timestamp),
            Just(DataType::Uuid),
            Just(DataType::Ipv4),
            Just(DataType::Ipv6),
            Just(DataType::Json),
            Just(DataType::Xml),
            Just(DataType::Interval),
            prop::option::of(1u32..=64).prop_map(|w| DataType::BigInt { width: w }),
            (1u32..=38, 0u32..=18).prop_map(|(p, s)| DataType::Decimal {
                precision: Some(p),
                scale: Some(s.min(p)),
            }),
            Just(DataType::Decimal {
                precision: None,
                scale: None,
            }),
            prop::option::of(1u32..=1024).prop_map(|sz| DataType::Text { size: sz }),
            prop::option::of(1u32..=1024).prop_map(|sz| DataType::Bytes { size: sz }),
            (
                prop::option::of(prop_oneof![
                    Just(DataType::Int32),
                    Just(DataType::Float64),
                    Just(DataType::Text { size: None }),
                ]),
                any::<bool>(),
            )
                .prop_map(|(element, element_nullable)| DataType::Array {
                    element: element.map(Box::new),
                    element_nullable,
                }),
        ]
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn data_type_serde_round_trip_non_custom(#[strategy(any_simple_data_type())] t: DataType) {
        let json = serde_json::to_value(&t).expect("serialize");
        let back: DataType = serde_json::from_value(json).expect("deserialize");
        prop_assert_eq!(back, t);
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(512))]
    fn cursor_compatible_classification(#[strategy(any_simple_data_type())] t: DataType) {
        let expected = !matches!(
            t,
            DataType::Json | DataType::Xml | DataType::Interval | DataType::Array { .. }
        );
        prop_assert_eq!(t.cursor_compatible(), expected);
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(64))]
    fn cursor_compatible_rejects_union(
        #[strategy(prop::collection::vec(any_simple_data_type(), 0..4))] inner: Vec<DataType>,
    ) {
        let dt = DataType::Union(inner);
        prop_assert!(!dt.cursor_compatible());
    }

    /// Strategy yielding a `(DataType, Value)` pair whose value matches
    /// the declared canonical type. Used to drive `decode_cursor_json`
    /// round-trip without inventing per-variant golden cases.
    fn any_canonical_type_value_pair() -> impl Strategy<Value = (DataType, Value)> {
        let bool_pair = any::<bool>().prop_map(|b| (DataType::Bool, Value::Bool(b)));
        let int8_pair = any::<i8>().prop_map(|n| (DataType::Int8, Value::Int8(n)));
        let int16_pair = any::<i16>().prop_map(|n| (DataType::Int16, Value::Int16(n)));
        let int32_pair = any::<i32>().prop_map(|n| (DataType::Int32, Value::Int32(n)));
        let int64_pair = any::<i64>().prop_map(|n| (DataType::Int64, Value::Int64(n)));
        let uint8_pair = any::<u8>().prop_map(|n| (DataType::UInt8, Value::UInt8(n)));
        let uint16_pair = any::<u16>().prop_map(|n| (DataType::UInt16, Value::UInt16(n)));
        let uint32_pair = any::<u32>().prop_map(|n| (DataType::UInt32, Value::UInt32(n)));
        let uint64_pair = any::<u64>().prop_map(|n| (DataType::UInt64, Value::UInt64(n)));
        let float32_pair = any::<f32>()
            .prop_filter("no NaN", |f| !f.is_nan())
            .prop_map(|n| (DataType::Float32, Value::Float32(n)));
        let float64_pair = any::<f64>()
            .prop_filter("no NaN", |f| !f.is_nan())
            .prop_map(|n| (DataType::Float64, Value::Float64(n)));
        let big_int_pair = any::<i128>().prop_map(|n| {
            (
                DataType::BigInt { width: None },
                Value::BigInt(num_bigint::BigInt::from(n)),
            )
        });
        let decimal_pair = (any::<i64>(), 0i64..18).prop_map(|(mantissa, scale)| {
            (
                DataType::Decimal {
                    precision: None,
                    scale: None,
                },
                Value::Decimal(bigdecimal::BigDecimal::new(
                    num_bigint::BigInt::from(mantissa),
                    scale,
                )),
            )
        });
        let text_pair = ".*".prop_map(|s: String| (DataType::Text { size: None }, Value::Text(s)));
        let bytes_pair = prop::collection::vec(any::<u8>(), 0..32)
            .prop_map(|b| (DataType::Bytes { size: None }, Value::Bytes(b)));
        let date_pair = (1970i32..2100, 1u32..=12, 1u32..=28).prop_map(|(y, m, d)| {
            (
                DataType::Date,
                Value::Date(chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap()),
            )
        });
        let timestamp_pair = any::<i64>().prop_filter_map("range", |seconds| {
            let s = seconds % 4_000_000_000;
            chrono::DateTime::<chrono::Utc>::from_timestamp(s, 0)
                .map(|t| (DataType::Timestamp, Value::Timestamp(t)))
        });
        let uuid_pair = any::<[u8; 16]>()
            .prop_map(|b| (DataType::Uuid, Value::Uuid(uuid::Uuid::from_bytes(b))));
        let ipv4_pair = any::<u32>().prop_map(|n| {
            (
                DataType::Ipv4,
                Value::Ipv4(std::net::Ipv4Addr::from(n.to_be_bytes())),
            )
        });
        let ipv6_pair = any::<[u8; 16]>()
            .prop_map(|b| (DataType::Ipv6, Value::Ipv6(std::net::Ipv6Addr::from(b))));
        let json_pair =
            any::<i64>().prop_map(|n| (DataType::Json, Value::Json(serde_json::json!({ "n": n }))));

        prop_oneof![
            bool_pair,
            int8_pair,
            int16_pair,
            int32_pair,
            int64_pair,
            uint8_pair,
            uint16_pair,
            uint32_pair,
            uint64_pair,
            float32_pair,
            float64_pair,
            big_int_pair,
            decimal_pair,
            text_pair,
            bytes_pair,
            date_pair,
            timestamp_pair,
            uuid_pair,
            ipv4_pair,
            ipv6_pair,
            json_pair,
        ]
    }

    /// `DataType::decode_cursor_json` round-trips every canonical
    /// `(DataType, Value)` pair where the value matches the declared
    /// type. Replaces the per-variant table that used to live as a
    /// `#[test]` golden case — the property is "serialize via Value
    /// then decode through the typed entry yields the original",
    /// independent of any particular variant.
    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn decode_cursor_json_canonical_round_trip(
        #[strategy(any_canonical_type_value_pair())] pair: (DataType, Value),
    ) {
        let (dt, value) = pair;
        let envelope = serde_json::to_value(&value).expect("serialize");
        let decoded = dt.decode_cursor_json(envelope).expect("decode");
        prop_assert_eq!(decoded, value);
    }
}
