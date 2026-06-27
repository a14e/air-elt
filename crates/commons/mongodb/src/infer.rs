//! Sample-based schema inference.
//!
//! Mongo collections have no central schema, so the source connector
//! infers one by reading a handful of documents and walking them in a
//! single pass to produce a `Schema` of every leaf path it observed.
//!
//! Heterogeneity rule: if a field appears as multiple non-null types
//! across the sample, we widen up the matrix where possible (`Int32 →
//! Int64`, `Int + Float → Float64`); anything genuinely heterogeneous
//! becomes `DataType::Union(...)` and the convert dispatcher resolves
//! it per-row.
//!
//! Nullability rule: every inferred field is marked `nullable = true`.
//! Sampling is non-exhaustive (capped by `schema-sample-size`, default
//! 100), so doc N+1 may legitimately carry null even if every sampled
//! doc had a present non-null value. Treating inferred fields as
//! nullable trades a tiny precision loss in `check_mapping` for
//! soundness — the matrix accepts `nullable_src → nullable_sink`, and
//! the Mongo sink is schemaless so `validate_flow` rebuilds `dst_schema`
//! with `nullable: true` regardless.
//!
//! `_id` is **not** an exception. Mongo guarantees the field is
//! present on every document, but applications can store `null` as
//! the unique `_id` value of one document — and the server-inserted
//! default is an `ObjectId`, not a NOT NULL constraint. Treating
//! `_id` as nullable is the only safe call; a sink-side `NOT NULL`
//! that rejects a legal `_id: null` will surface at validate-time
//! against the inferred schema instead of at runtime against a real
//! row.
//!
//! Allocation profile: leaf paths and types are collected into a
//! borrowed-key trie keyed by `&str` slices into the BSON documents.
//! `String` is allocated once per *unique leaf path* during DFS emit,
//! not once per (doc × leaf) observation. Type accumulation reuses an
//! `AHashMap<DataType, ()>` per leaf node.
//!
//! Depth limit: at `MAX_INFER_DEPTH` we stop descending and tag the
//! current node as `Json` (the whole subtree at that point is treated
//! as an opaque document). Mongo's own server-side limit is 100; we
//! match that as defence-in-depth against pathological inputs.

use std::collections::BTreeSet;

use ahash::{AHashMap, AHashSet};
use bson::{Bson, Document};
use thiserror::Error;
use tracing::debug;

use air_elt_core::mapping::FieldPath;
use air_elt_core::model::{Field, Schema};
use air_elt_core::types::DataType;

use crate::bson_value::infer_type;

/// Maximum BSON nesting depth the trie will descend before collapsing
/// the remaining subtree into a single `Json`-typed leaf. Mongo's
/// server enforces 100; we match it.
pub const MAX_INFER_DEPTH: usize = 100;

#[derive(Debug, Error)]
pub enum InferenceError {
    #[error("field {path:?} was absent in every sampled document")]
    AllMissing { path: String },
}

/// Walk `docs` once and return a `Schema` of every leaf path observed
/// across the sample. Each `Field` carries the merged `DataType`
/// (widened where possible, otherwise `Union`) and `nullable = true`.
pub fn infer_schema_from_sample(docs: &[Document]) -> Result<Schema, InferenceError> {
    /// Trie node: borrows `&str` keys directly out of the BSON
    /// documents. `types_seen` accumulates the leaf-level `DataType`
    /// observations across the whole sample.
    #[derive(Default)]
    struct Node<'a> {
        children: AHashMap<&'a str, Node<'a>>,
        types_seen: AHashSet<DataType>,
        leaf_observed: bool,
    }

    fn ingest<'a>(doc: &'a Document, node: &mut Node<'a>, depth: usize) {
        for (k, v) in doc {
            let child = node.children.entry(k.as_str()).or_default();
            ingest_value(v, child, depth + 1);
        }
    }

    fn ingest_value<'a>(v: &'a Bson, node: &mut Node<'a>, depth: usize) {
        if depth > MAX_INFER_DEPTH {
            // Subtree too deep — collapse into Json and stop descending.
            node.leaf_observed = true;
            node.types_seen.insert(DataType::Json);
            return;
        }
        match v {
            Bson::Document(sub) => {
                // Sub-document: recurse without recording a leaf type
                // here. The leaf observations come from the actual
                // scalar/array values inside.
                ingest(sub, node, depth);
            }
            other => {
                node.leaf_observed = true;
                if let Some(dt) = infer_type(other) {
                    node.types_seen.insert(dt);
                }
            }
        }
    }

    fn emit(node: &Node<'_>, prefix: &mut Vec<String>, out: &mut Vec<Field>) {
        // Heterogeneous shape at the same path: some docs had this
        // path as a sub-document (so children were populated), others
        // had it as a scalar/null (so `leaf_observed` was set). The
        // path cannot be both a leaf *and* a parent at once — the
        // mapping only resolves one entry per dotted path. Collapse
        // into a single leaf that preserves the observed scalar types
        // alongside `Json` (so the operator still sees, e.g. `Int32 |
        // Json`) and drop the children; the matrix accepts `Json →
        // Json` and most sinks can ingest the raw sub-document as
        // JSON, while the convert dispatcher resolves the per-row
        // variant from the union.
        let mixed_shape = node.leaf_observed && !node.children.is_empty();
        if (node.leaf_observed || mixed_shape)
            && !prefix.is_empty()
            && let Ok(p) = FieldPath::from_segments(prefix.clone())
        {
            let merged = if mixed_shape {
                let path_str = p.to_string();
                let observed: Vec<DataType> = node.types_seen.iter().cloned().collect();
                debug!(
                    path = %path_str,
                    observed_kinds = ?observed,
                    "heterogeneous-shape collapse: this path has mixed shapes; emitting Union"
                );
                if observed.is_empty() {
                    // Only Null observed at the leaf side — no scalar
                    // types to preserve, fall back to plain Json.
                    DataType::Json
                } else {
                    let mut variants = observed;
                    variants.push(DataType::Json);
                    DataType::union(variants)
                }
            } else {
                merge_types(node.types_seen.iter().cloned())
            };
            // Every inferred field is treated as nullable — sampling
            // is non-exhaustive (we look at ~100 docs out of millions)
            // and `_id` in particular can legally be `null` once per
            // collection because the application chose `null` as that
            // collection's unique `_id` value. Forcing `nullable =
            // false` on `_id` would let the validator accept it
            // mapped onto a `NOT NULL` sink column, then fail at
            // runtime on a valid Mongo document. The Mongo sink is
            // schemaless so `validate_flow` rebuilds the dst schema
            // with `nullable = true` anyway.
            let name = p.to_string();
            out.push(Field {
                name,
                data_type: merged,
                nullable: true,
            });
        }
        if mixed_shape {
            // Children are subsumed by the Json leaf above — do not
            // emit nested paths, they would shadow the parent entry.
            return;
        }
        for (k, child) in &node.children {
            prefix.push((*k).to_string());
            emit(child, prefix, out);
            prefix.pop();
        }
    }

    let mut root: Node<'_> = Node::default();
    for d in docs {
        ingest(d, &mut root, 0);
    }
    let mut fields: Vec<Field> = Vec::new();
    let mut prefix: Vec<String> = Vec::new();
    emit(&root, &mut prefix, &mut fields);

    if fields.is_empty() {
        return Err(InferenceError::AllMissing {
            path: "<root>".into(),
        });
    }
    Ok(Schema::schemaless_with_sample(fields))
}

/// Merge the set of types observed for a single field across the
/// sample into one canonical `DataType`. Pure widening collapses to
/// the wider variant (`Int32 + Int64 → Int64`, `Int + Float →
/// Float64`); anything genuinely heterogeneous becomes
/// `DataType::Union(...)`, which the convert dispatcher resolves at
/// runtime by inspecting the actual `Value`.
///
/// Takes `impl IntoIterator` so the homogeneous case (the vast
/// majority of fields) never allocates a `Vec`: a single observed
/// type returns directly. The `Vec` only materialises when the field
/// genuinely had 2+ distinct types and we need to inspect the kind
/// set.
fn merge_types(types: impl IntoIterator<Item = DataType>) -> DataType {
    let mut iter = types.into_iter();
    let Some(first) = iter.next() else {
        // Every observation was BSON null — fall back to Text unbounded
        // so the matrix can still pair it with most sinks.
        return DataType::Text { size: None };
    };
    let Some(second) = iter.next() else {
        return first;
    };
    // 2+ distinct types: now we have to materialise the set to dispatch.
    let mut all = Vec::with_capacity(2 + iter.size_hint().0);
    all.push(first);
    all.push(second);
    all.extend(iter);
    let set: BTreeSet<TypeKind> = all.iter().map(TypeKind::of).collect();
    if set.iter().all(|k| matches!(k, TypeKind::Int)) {
        return DataType::Int64;
    }
    if set
        .iter()
        .all(|k| matches!(k, TypeKind::Int | TypeKind::Float))
    {
        return DataType::Float64;
    }
    DataType::union(all)
}

/// Coarse-grained type bucket used by [`merge_types`] to detect
/// homogeneous-modulo-width observations. The discriminant fields
/// matter for `Eq`/`Ord` (used as a `BTreeSet` key): two `Custom`
/// observations carry the full `DataType` so distinct vendor types
/// (e.g. `mongodb.object_id` vs `mongodb.javascript`) are kept apart
/// while two `mongodb.object_id` observations collapse.
#[derive(Debug, Clone, PartialEq, Eq, Ord, PartialOrd)]
enum TypeKind {
    Bool,
    Int,
    Float,
    Text,
    Bytes,
    Uuid,
    Ipv4,
    Ipv6,
    Date,
    Timestamp,
    Decimal,
    Json,
    Xml,
    Union,
    Custom(DataType),
}

impl TypeKind {
    fn of(dt: &DataType) -> Self {
        match dt {
            DataType::Bool => TypeKind::Bool,
            DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::BigInt { .. } => TypeKind::Int,
            DataType::Float32 | DataType::Float64 => TypeKind::Float,
            DataType::Text { .. } => TypeKind::Text,
            DataType::Bytes { .. } => TypeKind::Bytes,
            DataType::Uuid => TypeKind::Uuid,
            DataType::Ipv4 => TypeKind::Ipv4,
            DataType::Ipv6 => TypeKind::Ipv6,
            DataType::Date => TypeKind::Date,
            DataType::Timestamp => TypeKind::Timestamp,
            DataType::Decimal { .. } => TypeKind::Decimal,
            DataType::Json | DataType::Object => TypeKind::Json,
            DataType::Xml => TypeKind::Xml,
            DataType::Union(_) => TypeKind::Union,
            // Interval is a redis-only type; it is never inferred from a
            // BSON document. Treat it as opaque so it never widens with
            // anything if it somehow reaches the union-collapse path.
            DataType::Interval => TypeKind::Custom(dt.clone()),
            DataType::Custom(_) => TypeKind::Custom(dt.clone()),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use bson::doc;

    fn find<'a>(s: &'a Schema, name: &str) -> &'a Field {
        s.fields()
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("field {name:?} not in schema {:?}", s.fields()))
    }

    #[test]
    fn flat_int32_inferred() {
        let docs = vec![doc! { "id": 1_i32 }, doc! { "id": 2_i32 }];
        let s = infer_schema_from_sample(&docs).unwrap();
        let f = find(&s, "id");
        assert_eq!(f.data_type, DataType::Int32);
        assert!(f.nullable);
    }

    #[test]
    fn missing_in_one_still_nullable() {
        // Sampling is non-exhaustive — every non-`_id` field is
        // nullable regardless of presence count.
        let docs = vec![doc! { "id": 1_i32 }, doc! {}];
        let s = infer_schema_from_sample(&docs).unwrap();
        assert!(find(&s, "id").nullable);
    }

    #[test]
    fn id_field_is_nullable_like_everything_else() {
        // `_id` exists on every Mongo document but an application can
        // legally store `null` as the unique `_id` value of one
        // document. Treating it as non-nullable would let the
        // validator accept it mapped onto a `NOT NULL` sink column,
        // then fail at runtime on a valid Mongo document.
        use bson::oid::ObjectId;
        let docs = vec![
            doc! { "_id": ObjectId::new(), "name": "alice" },
            doc! { "_id": ObjectId::new(), "name": "bob" },
        ];
        let s = infer_schema_from_sample(&docs).unwrap();
        assert!(find(&s, "_id").nullable, "_id must be nullable");
        assert!(find(&s, "name").nullable);
    }

    #[test]
    fn id_field_stays_nullable_with_varying_other_fields() {
        use bson::oid::ObjectId;
        let docs = vec![
            doc! { "_id": ObjectId::new(), "a": 1_i32 },
            doc! { "_id": ObjectId::new(), "b": "x" },
            doc! { "_id": ObjectId::new() },
        ];
        let s = infer_schema_from_sample(&docs).unwrap();
        assert!(find(&s, "_id").nullable);
        assert!(find(&s, "a").nullable);
        assert!(find(&s, "b").nullable);
    }

    #[test]
    fn nested_id_inside_subdocument_is_nullable() {
        // Same nullable-by-default rule for any nested `_id`.
        let docs = vec![doc! { "addr": { "_id": "x" } }];
        let s = infer_schema_from_sample(&docs).unwrap();
        let f = find(&s, "addr._id");
        assert!(f.nullable);
    }

    #[test]
    fn nested_field_emitted_as_dotted() {
        let docs = vec![doc! { "addr": { "city": "Berlin" } }];
        let s = infer_schema_from_sample(&docs).unwrap();
        let f = find(&s, "addr.city");
        assert_eq!(f.data_type, DataType::Text { size: None });
        assert!(f.nullable);
    }

    #[test]
    fn always_nullable_even_if_full() {
        let docs = vec![
            doc! { "id": 1_i32 },
            doc! { "id": 2_i32 },
            doc! { "id": 3_i32 },
        ];
        let s = infer_schema_from_sample(&docs).unwrap();
        assert!(
            find(&s, "id").nullable,
            "sampling is non-exhaustive — every inferred field must be nullable"
        );
    }

    #[test]
    fn heterogeneous_becomes_union() {
        let docs = vec![doc! { "id": 1_i32 }, doc! { "id": "abc" }];
        let s = infer_schema_from_sample(&docs).unwrap();
        let f = find(&s, "id");
        match &f.data_type {
            DataType::Union(vs) => {
                assert!(vs.contains(&DataType::Int32));
                assert!(vs.contains(&DataType::Text { size: None }));
                assert_eq!(vs.len(), 2);
            }
            other => panic!("expected Union, got {other:?}"),
        }
        assert!(f.nullable);
    }

    #[test]
    fn empty_sample_rejected() {
        // No documents → no fields. Surface as AllMissing so callers
        // know the sample yielded nothing usable.
        let docs: Vec<Document> = vec![doc! {}, doc! {}];
        let err = infer_schema_from_sample(&docs).unwrap_err();
        assert!(matches!(err, InferenceError::AllMissing { .. }));
    }

    #[test]
    fn int_widening() {
        let docs = vec![doc! { "n": 1_i32 }, doc! { "n": 2_i64 }];
        let s = infer_schema_from_sample(&docs).unwrap();
        let f = find(&s, "n");
        assert_eq!(f.data_type, DataType::Int64);
        assert!(f.nullable);
    }

    #[test]
    fn depth_limit_collapses_to_json() {
        // Build a doc nested >MAX_INFER_DEPTH levels. The trie should
        // stop descending and tag the cutoff path as Json.
        let mut inner = doc! { "leaf": 1_i32 };
        // 150 wraps puts the leaf well past the 100-depth limit.
        for _ in 0..150 {
            inner = doc! { "n": inner };
        }
        let s = infer_schema_from_sample(&[inner]).unwrap();
        // At least one field whose data_type is Json must exist —
        // the deep-cut subtree.
        let has_json = s
            .fields()
            .iter()
            .any(|f| matches!(f.data_type, DataType::Json));
        assert!(
            has_json,
            "expected at least one Json-typed cutoff leaf, got {:?}",
            s.fields()
        );
    }

    #[test]
    fn int_float_widens_to_float64() {
        let docs = vec![doc! { "n": 1_i32 }, doc! { "n": 1.5_f64 }];
        let s = infer_schema_from_sample(&docs).unwrap();
        assert_eq!(find(&s, "n").data_type, DataType::Float64);
    }

    #[test]
    fn mixed_doc_and_scalar_at_same_path_emits_union() {
        // Some docs carry `x` as a scalar, others as a sub-document.
        // The trie used to emit BOTH a `x` leaf (Text/Int) AND a
        // `x.y` leaf, which broke `from = "x"` resolution. The fix
        // collapses heterogeneous shapes at one path into a single
        // leaf that preserves the scalar type alongside `Json`, and
        // drops the nested children.
        let docs = vec![doc! { "x": 1_i32 }, doc! { "x": { "y": 2_i32 } }];
        let s = infer_schema_from_sample(&docs).unwrap();
        let f = find(&s, "x");
        let expected = DataType::union(vec![DataType::Int32, DataType::Json]);
        assert_eq!(
            f.data_type, expected,
            "mixed doc/scalar shape at one path must become Union(scalar, Json), got {:?}",
            f.data_type
        );
        assert!(f.nullable);
        assert!(
            !s.fields().iter().any(|f| f.name == "x.y"),
            "nested children must not be emitted alongside the parent leaf, got {:?}",
            s.fields()
        );
    }

    #[test]
    fn mixed_doc_and_null_at_same_path_emits_json() {
        // BSON null at a path that elsewhere holds a sub-document
        // also counts as the heterogeneous case (the path is a leaf
        // in the null doc but a parent in the document doc).
        let docs = vec![doc! { "x": bson::Bson::Null }, doc! { "x": { "y": 2_i32 } }];
        let s = infer_schema_from_sample(&docs).unwrap();
        let f = find(&s, "x");
        assert_eq!(f.data_type, DataType::Json);
        assert!(!s.fields().iter().any(|f| f.name == "x.y"));
    }

    #[test]
    fn two_object_id_observations_collapse_to_single_custom_type() {
        use bson::oid::ObjectId;
        let docs = vec![doc! { "k": ObjectId::new() }, doc! { "k": ObjectId::new() }];
        let s = infer_schema_from_sample(&docs).unwrap();
        let f = find(&s, "k");
        match &f.data_type {
            DataType::Custom(t) => assert_eq!(t.kind(), "mongodb.object_id"),
            other => panic!("expected DataType::Custom(mongodb.object_id), got {other:?}"),
        }
    }

    #[test]
    fn object_id_vs_string_observation_emits_union() {
        use bson::oid::ObjectId;
        let docs = vec![doc! { "k": ObjectId::new() }, doc! { "k": "abc" }];
        let s = infer_schema_from_sample(&docs).unwrap();
        let f = find(&s, "k");
        match &f.data_type {
            DataType::Union(vs) => {
                assert_eq!(vs.len(), 2);
                assert!(
                    vs.iter().any(|v| matches!(
                        v,
                        DataType::Custom(t) if t.kind() == crate::types::MongoObjectIdType::KIND
                    )),
                    "Union must contain ObjectId Custom: {vs:?}"
                );
                assert!(
                    vs.contains(&DataType::Text { size: None }),
                    "Union must contain unbounded Text: {vs:?}"
                );
            }
            other => panic!("expected Union, got {other:?}"),
        }
    }

    #[test]
    fn object_id_and_javascript_observations_emit_union_of_two_customs() {
        use bson::oid::ObjectId;
        let docs = vec![
            doc! { "k": ObjectId::new() },
            doc! { "k": Bson::JavaScriptCode("function () {}".into()) },
        ];
        let s = infer_schema_from_sample(&docs).unwrap();
        let f = find(&s, "k");
        match &f.data_type {
            DataType::Union(vs) => {
                assert_eq!(vs.len(), 2);
                let kinds: Vec<&str> = vs
                    .iter()
                    .filter_map(|v| match v {
                        DataType::Custom(t) => Some(t.kind()),
                        _ => None,
                    })
                    .collect();
                assert!(kinds.contains(&"mongodb.object_id"));
                assert!(kinds.contains(&"mongodb.javascript"));
            }
            other => panic!("expected Union, got {other:?}"),
        }
    }

    #[test]
    fn multiple_top_level_fields_emitted() {
        let docs = vec![doc! { "a": 1_i32, "b": "x" }, doc! { "a": 2_i32, "b": "y" }];
        let s = infer_schema_from_sample(&docs).unwrap();
        assert_eq!(s.fields().len(), 2);
        assert_eq!(find(&s, "a").data_type, DataType::Int32);
        assert_eq!(find(&s, "b").data_type, DataType::Text { size: None });
    }
}
