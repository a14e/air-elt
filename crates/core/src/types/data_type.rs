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

    /// Build a normalised `Union`. Flattens any nested `Union(...)`
    /// inputs so the result is always one level deep, then sorts and
    /// deduplicates so equality is observation-order-independent, and
    /// collapses a 1-element union to the bare variant.
    pub fn union(vs: Vec<DataType>) -> Self {
        let mut flat: Vec<DataType> = Vec::with_capacity(vs.len());
        for v in vs {
            match v {
                DataType::Union(inner) => flat.extend(inner),
                other => flat.push(other),
            }
        }
        // Why: derived `Ord` is allocation-free (lexicographic on the
        // discriminant + fields), unlike a `format!`-based sort key
        // which would allocate two `String`s per comparison for a type
        // that's otherwise stack-resident.
        flat.sort();
        flat.dedup();
        if flat.len() == 1 {
            return flat.into_iter().next().expect("len==1");
        }
        DataType::Union(flat)
    }

    /// Whether this type is admissible as a cursor field. `Json`/`Xml`/
    /// `Union` are rejected (no total order); custom types delegate to
    /// `DynType::can_be_cursor()`.
    pub fn can_be_cursor(&self) -> bool {
        match self {
            DataType::Json | DataType::Xml | DataType::Union(_) => false,
            DataType::Custom(t) => t.can_be_cursor(),
            _ => true,
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
        DataType::Int16 => 1,
        DataType::Int32 => 2,
        DataType::Int64 => 3,
        DataType::UInt8 => 4,
        DataType::UInt16 => 5,
        DataType::UInt32 => 6,
        DataType::UInt64 => 7,
        DataType::Float32 => 8,
        DataType::Float64 => 9,
        DataType::BigInt { .. } => 10,
        DataType::Decimal { .. } => 11,
        DataType::Text { .. } => 12,
        DataType::Bytes { .. } => 13,
        DataType::Date => 14,
        DataType::Timestamp => 15,
        DataType::Uuid => 16,
        DataType::Json => 17,
        DataType::Xml => 18,
        DataType::Union(_) => 19,
        DataType::Custom(_) => 20,
    }
}

impl std::fmt::Display for DataType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataType::Bool => f.write_str("bool"),
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
    use super::*;
    use crate::types::convert::ConvertError;
    use crate::types::convert::context::ConversionContext;
    use crate::types::default_value::DefaultParseError;
    use crate::types::value::Value;

    /// Test-only Custom type with `can_be_cursor = true` so `can_be_cursor`
    /// arm coverage is unambiguous.
    #[derive(Debug)]
    struct TestCursorable;

    impl DynType for TestCursorable {
        fn kind(&self) -> &'static str {
            "test.cursorable"
        }
        fn can_be_cursor(&self) -> bool {
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
        fn kind(&self) -> &'static str {
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
    fn can_be_cursor_basic_variants() {
        assert!(DataType::Int32.can_be_cursor());
        assert!(DataType::Text { size: None }.can_be_cursor());
        assert!(DataType::Uuid.can_be_cursor());
        assert!(DataType::Date.can_be_cursor());
        assert!(DataType::Timestamp.can_be_cursor());
    }

    #[test]
    fn can_be_cursor_rejects_unordered() {
        assert!(!DataType::Json.can_be_cursor());
        assert!(!DataType::Xml.can_be_cursor());
        assert!(!DataType::Union(vec![DataType::Int32, DataType::Int64]).can_be_cursor());
    }

    #[test]
    fn can_be_cursor_delegates_to_dyn_type() {
        let ok = DataType::Custom(Box::new(TestCursorable));
        let no = DataType::Custom(Box::new(TestNonCursorable));
        assert!(ok.can_be_cursor());
        assert!(!no.can_be_cursor());
    }
}
