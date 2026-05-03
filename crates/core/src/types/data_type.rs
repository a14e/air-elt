use serde::{Deserialize, Serialize};

/// Canonical "pivot" type. Each connector maps native ↔ canonical, the runner
/// uses the matrix in `super::matrix` to validate compatibility.
///
/// `Text` and `Bytes` carry an optional declared size (`varchar(36)`,
/// `binary(16)`, etc.). `None` means unbounded (`text`, `mediumtext`, `blob`).
/// The size is part of the *schema*, not the *value* — `Value::Text` stores a
/// plain `String` regardless. Width is consulted only at validation time so
/// the matrix can reject narrowing pairs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
        }
    }
}
