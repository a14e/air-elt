use serde::{Deserialize, Serialize};

use crate::types::data_type::DataType;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
}

/// What the source/sink advertises about its own schema.
///
/// - [`Fixed`]: connector advertises a definitive schema (Postgres / MySQL DDL).
/// - [`Schemaless`]: connector is schemaless and no sample is available.
/// - [`SchemalessWithSample`]: connector is schemaless but a sample-derived
///   schema is in `fields`. The wildcard-only schemaless-both fast path
///   ignores the sample and still picks raw-passthrough — sampling is
///   informational, not authoritative.
///
/// [`Fixed`]: SchemaKind::Fixed
/// [`Schemaless`]: SchemaKind::Schemaless
/// [`SchemalessWithSample`]: SchemaKind::SchemalessWithSample
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SchemaKind {
    #[default]
    Fixed,
    Schemaless,
    SchemalessWithSample,
}

impl SchemaKind {
    /// `true` iff the connector is schemaless (with or without sample).
    pub fn is_schemaless(self) -> bool {
        matches!(self, Self::Schemaless | Self::SchemalessWithSample)
    }
}

/// Source / sink schema. Carries the field list, an O(1) name → index
/// lookup table, and a [`SchemaKind`] discriminating fixed (DDL-derived)
/// from schemaless (sample-derived or absent) schemas.
///
/// `name_to_index` is rebuilt from `fields` on construction and after
/// deserialization — it is **not** part of the wire format.
#[derive(Debug, Clone, Default)]
pub struct Schema {
    fields: Vec<Field>,
    name_to_index: ahash::AHashMap<String, usize>,
    kind: SchemaKind,
}

impl PartialEq for Schema {
    fn eq(&self, other: &Self) -> bool {
        // `name_to_index` is derived from `fields`, so comparing
        // `fields + kind` is equivalent and cheaper.
        self.kind == other.kind && self.fields == other.fields
    }
}

impl Schema {
    /// Build a schema with [`SchemaKind::Fixed`] — connector advertises a
    /// definitive schema (e.g. Postgres / MySQL DDL).
    pub fn new(fields: Vec<Field>) -> Self {
        let name_to_index = build_index(&fields);
        Self {
            fields,
            name_to_index,
            kind: SchemaKind::Fixed,
        }
    }

    /// Empty schema with [`SchemaKind::Schemaless`] — no sample available.
    pub fn schemaless() -> Self {
        Self {
            fields: Vec::new(),
            name_to_index: ahash::AHashMap::default(),
            kind: SchemaKind::Schemaless,
        }
    }

    /// Sample-derived schema for a schemaless connector, kind
    /// [`SchemaKind::SchemalessWithSample`].
    pub fn schemaless_with_sample(fields: Vec<Field>) -> Self {
        let name_to_index = build_index(&fields);
        Self {
            fields,
            name_to_index,
            kind: SchemaKind::SchemalessWithSample,
        }
    }

    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    pub fn kind(&self) -> SchemaKind {
        self.kind
    }

    pub fn is_schemaless(&self) -> bool {
        self.kind.is_schemaless()
    }

    pub fn find(&self, name: &str) -> Option<&Field> {
        self.name_to_index
            .get(name)
            .and_then(|&i| self.fields.get(i))
    }

    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.name_to_index.get(name).copied()
    }
}

fn build_index(fields: &[Field]) -> ahash::AHashMap<String, usize> {
    fields
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.clone(), i))
        .collect()
}

/// Wire shim for [`Schema`]. The on-wire format is
/// `{"fields": [...], "kind": "fixed"|"schemaless"|"schemaless-with-sample"}`.
/// `name_to_index` is private and is rebuilt from `fields` on
/// deserialize. `kind` defaults to `Fixed` so legacy payloads that lack
/// the field continue to deserialize as fixed schemas — none currently
/// persist to disk, but the default keeps the wire surface forward-
/// compatible if one is added later.
#[derive(Serialize, Deserialize)]
struct SchemaWire {
    fields: Vec<Field>,
    #[serde(default)]
    kind: SchemaKind,
}

impl Serialize for Schema {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let wire = SchemaWire {
            fields: self.fields.clone(),
            kind: self.kind,
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Schema {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = SchemaWire::deserialize(deserializer)?;
        let name_to_index = build_index(&wire.fields);
        Ok(Self {
            fields: wire.fields,
            name_to_index,
            kind: wire.kind,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn f(name: &str) -> Field {
        Field {
            name: name.into(),
            data_type: DataType::Int32,
            nullable: false,
        }
    }

    #[test]
    fn fixed_schema_kind_and_lookup() {
        let s = Schema::new(vec![f("a"), f("b"), f("c")]);
        assert_eq!(s.kind(), SchemaKind::Fixed);
        assert!(!s.is_schemaless());
        assert_eq!(s.index_of("a"), Some(0));
        assert_eq!(s.index_of("c"), Some(2));
        assert_eq!(s.index_of("missing"), None);
        assert_eq!(s.find("b").unwrap().name, "b");
        assert!(s.find("missing").is_none());
    }

    #[test]
    fn schemaless_is_empty() {
        let s = Schema::schemaless();
        assert_eq!(s.kind(), SchemaKind::Schemaless);
        assert!(s.is_schemaless());
        assert!(s.fields().is_empty());
        assert!(s.find("anything").is_none());
    }

    #[test]
    fn schemaless_with_sample_carries_fields() {
        let s = Schema::schemaless_with_sample(vec![f("a")]);
        assert_eq!(s.kind(), SchemaKind::SchemalessWithSample);
        assert!(s.is_schemaless());
        assert_eq!(s.fields().len(), 1);
        assert_eq!(s.index_of("a"), Some(0));
    }

    #[test]
    fn default_kind_is_fixed() {
        assert_eq!(SchemaKind::default(), SchemaKind::Fixed);
        assert_eq!(Schema::default().kind(), SchemaKind::Fixed);
    }

    #[test]
    fn serde_round_trip_fixed() {
        let s = Schema::new(vec![f("a"), f("b")]);
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"kind\":\"fixed\""));
        let back: Schema = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.index_of("b"), Some(1));
    }

    #[test]
    fn serde_round_trip_schemaless() {
        let s = Schema::schemaless();
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"kind\":\"schemaless\""));
        let back: Schema = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn serde_round_trip_schemaless_with_sample() {
        let s = Schema::schemaless_with_sample(vec![f("a")]);
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"kind\":\"schemaless-with-sample\""));
        let back: Schema = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.index_of("a"), Some(0));
    }

    #[test]
    fn deserialize_without_kind_defaults_to_fixed() {
        // Legacy / loose payload — no `kind` field. Defaults to Fixed.
        let json = r#"{"fields":[{"name":"a","data_type":"int32","nullable":false}]}"#;
        let s: Schema = serde_json::from_str(json).unwrap();
        assert_eq!(s.kind(), SchemaKind::Fixed);
        assert_eq!(s.index_of("a"), Some(0));
    }
}
