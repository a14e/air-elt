//! Wildcard / JSON-pack expansion.
//!
//! Consumes the flow's [`ColumnMapping`] vector (post-shorthand,
//! pre-schema) along with the source/sink schemas and produces an
//! [`ExpandedMapping`] — the post-expansion shape that the validation
//! pipeline then drives matrix/uniqueness checks against and the runner
//! turns into per-cell `ColumnConversionPlan`s.
//!
//! When wildcard expansion picks the sink schema and a sink column has
//! no same-named source column, if the sink column is nullable the
//! column is **omitted** from the expansion (the sink writes the row
//! without it; the column gets its declared default / NULL); if the
//! sink column is NOT NULL → `WildcardMissingNonNullableSource`.

use crate::error::ValidationError;
use crate::mapping::ColumnMapping;
use crate::model::{Schema, SchemaKind};

/// Returns `Some(schema)` when the schema carries fields the expander
/// can fan out against — i.e. either fixed (DDL-derived) or schemaless
/// with a sample. Bare schemaless (no sample) collapses to `None`.
fn schema_for_expansion(s: &Schema) -> Option<&Schema> {
    match s.kind() {
        SchemaKind::Fixed | SchemaKind::SchemalessWithSample => Some(s),
        SchemaKind::Schemaless => None,
    }
}

/// Universe-size cap. Wildcard expansion against a schema with more
/// than this many columns is rejected — the runner's per-batch column
/// count would otherwise dwarf any reasonable batch limit and the
/// generated SQL / projection grows linearly with it.
pub const WILDCARD_UNIVERSE_CAP: usize = 4096;

/// Result of [`expand`]. Carries the post-expansion direct-column
/// vector and an optional shared body / wildcard-pack plan (the
/// Transform interpreter's `Body` op fans the body
/// payload out across one or more sink target columns). `body = None`
/// means a plain (possibly wildcard-driven) column-to-column flow.
/// When present, [`Body`] names the source columns the body draws
/// from and the sink targets the assembled payload lands in.
///
/// The schemaless-both wildcard `["*"]` flow (mongo→mongo today) is
/// expressed as a body block with empty `source_columns` and one
/// synthetic `_root` target — the Transform compiler lowers it to a
/// single `Body` op and the mongo sink writes the carried
/// document at root. There is no separate raw-passthrough plan.
#[derive(Debug, Clone)]
pub struct ExpandedMapping {
    pub direct: Vec<DirectMapping>,
    pub body: Option<Body>,
}

/// Shared body / wildcard auto-pack plan. `targets` lists the sink
/// column names that receive the body payload — one entry per `*:NAME`
/// rule. `source_columns` is the post-expansion list of source column
/// names that contribute to the body (deduplicated union of direct
/// `from` columns and the remaining source schema columns); the source's
/// `read_batch` implementation pairs `row.values[direct_count..]` with
/// the tail slice of these names to assemble the body payload — sources
/// populate `RawRow.body` directly. All targets
/// are duplicate-free (post-expansion `DuplicateSinkField` check).
#[derive(Debug, Clone)]
pub struct Body {
    pub source_columns: Vec<String>,
    pub targets: Vec<String>,
}

/// A 1:1 column mapping after wildcard expansion. Always reads from a
/// real source column. Sink-driven wildcard expansion omits
/// nullable sink columns that have no same-named source column entirely
/// — they never appear in the expanded mapping, and the sink relies on
/// its DDL default / NULL when writing the row.
#[derive(Debug, Clone, PartialEq)]
pub struct DirectMapping {
    pub from: String,
    pub to: String,
    pub truncate: bool,
    pub default_literal: Option<toml::Value>,
}

/// Synthetic body target name reserved for the schemaless-both
/// wildcard flow. Mongo sink recognises a single-column row whose
/// only value is `Value::Custom(BsonObjectValue)` and writes the
/// document at root, regardless of column name — but using a stable
/// reserved name makes the intent explicit on `WriteSpec.columns`
/// and lets log lines / dry-run probes mention "root document".
pub const ROOT_BODY_TARGET: &str = "_root";

/// Expand `rules` against the available schemas.
///
/// Each side is a [`Schema`] whose [`crate::model::SchemaKind`] discriminates
/// fixed (DDL-derived) from schemaless (with or without sample). The
/// wildcard-only schemaless-both fast path triggers when both sides are
/// schemaless (with or without sample) and the only rule is `"*"` — the
/// sample-derived schema is deliberately ignored there because samples
/// are informational, not authoritative.
///
/// `flow_name` is propagated into errors so operator output names the
/// failing flow without an extra wrapping layer.
pub fn expand(
    rules: &[ColumnMapping],
    src: &Schema,
    dst: &Schema,
    source_schemaless: bool,
    sink_schemaless: bool,
    flow_name: &str,
) -> Result<ExpandedMapping, ValidationError> {
    debug_assert!(!rules.is_empty(), "build() rejects empty mappings");

    let has_wildcard = rules.iter().any(|r| matches!(r, ColumnMapping::Wildcard));
    let body_targets: Vec<String> = rules
        .iter()
        .filter_map(|r| match r {
            ColumnMapping::Body { to } => Some(to.clone()),
            _ => None,
        })
        .collect();

    // Raw passthrough is the right choice for schemaless-both whenever
    // the mapping is just `["*"]` — sample-derived schemas are a sample,
    // not authoritative, so reducing through them would silently drop
    // fields that didn't appear in the sample and degrade BSON-specific
    // types (Decimal128, ObjectId, DateTime) through a canonical
    // round-trip. We bypass schema-driven expansion entirely here, even
    // when sample schemas are available (`SchemalessWithSample`). Mixed
    // mappings (`["id", "*"]`, `["*:body"]`, etc.) go through the typed
    // path below — they ask for typed columns explicitly.
    //
    // The gate uses the connector-level `schemaless()` flag rather
    // than the schema's kind: a non-schemaless connector (pg, mysql)
    // whose schema introspection was disabled
    // (`validation.fields = false`) collapses to `Schema::schemaless()`
    // upstream, but that doesn't make it eligible for raw passthrough —
    // only document-store connectors (mongo / mongo-cdc) advertise
    // `schemaless() == true`. `compile_to_transform` additionally
    // re-asserts the source's `body_data_type().is_object()` when
    // lowering body targets, so a future schemaless source that didn't
    // produce an object body would fail there with a clear invariant
    // message.
    let wildcard_only = rules.len() == 1 && has_wildcard;
    if wildcard_only && source_schemaless && sink_schemaless {
        return Ok(ExpandedMapping {
            direct: Vec::new(),
            body: Some(Body {
                source_columns: Vec::new(),
                targets: vec![ROOT_BODY_TARGET.to_string()],
            }),
        });
    }

    // For non-fast-path expansion, pull the borrowed schemas (if any).
    // `Schemaless` (kind without a sample) collapses to `None`;
    // `SchemalessWithSample` and `Fixed` both surface their carried fields.
    let src_schema: Option<&Schema> = schema_for_expansion(src);
    let dst_schema: Option<&Schema> = schema_for_expansion(dst);

    // Wildcard with neither schema available is unrecoverable: the
    // connector either advertises a schema we don't have or is
    // schemaless without sample — we cannot fan out. Reject with
    // `WildcardWithoutSchema`.
    if has_wildcard && src_schema.is_none() && dst_schema.is_none() {
        return Err(ValidationError::WildcardWithoutSchema {
            flow: flow_name.to_string(),
        });
    }

    // Universe selection: prefer sink schema, fall back to source.
    let universe_is_sink = dst_schema.is_some();
    let universe = dst_schema.or(src_schema);

    // Wildcard fan-out. Skipped if there is no Wildcard rule.
    // For every universe column, push a `DirectMapping`. When
    // the universe is the sink schema and the source schema is known,
    // a sink column that lacks a same-named source column is either
    // skipped entirely (nullable — sink will leave it at its DDL
    // default / NULL) or rejected (NOT NULL → `WildcardMissingNonNullableSource`).
    let mut direct: Vec<DirectMapping> = Vec::new();
    if has_wildcard {
        let universe = universe.ok_or(ValidationError::WildcardWithoutSchema {
            flow: flow_name.to_string(),
        })?;
        if universe.fields().len() > WILDCARD_UNIVERSE_CAP {
            return Err(ValidationError::WildcardUniverseTooLarge {
                flow: flow_name.to_string(),
                count: universe.fields().len(),
            });
        }
        for field in universe.fields() {
            // Sink-driven universe + known source schema: a sink
            // column with no same-named source column is either
            // skipped (nullable) or rejected (NOT NULL).
            if let (true, Some(src_s)) = (universe_is_sink, src_schema)
                && src_s.find(&field.name).is_none()
            {
                if !field.nullable {
                    return Err(ValidationError::WildcardMissingNonNullableSource {
                        flow: flow_name.to_string(),
                        column: field.name.clone(),
                    });
                }
                // Nullable — omit the column entirely.
                continue;
            }
            direct.push(DirectMapping {
                from: field.name.clone(),
                to: field.name.clone(),
                truncate: false,
                default_literal: None,
            });
        }
    }

    // Explicit overrides. Walk rules in user-declared order:
    // each Direct either replaces a wildcard slot of the same `to` or
    // appends to the tail.
    for rule in rules {
        if let ColumnMapping::Direct {
            from,
            to,
            truncate,
            default_literal,
        } = rule
        {
            let new_entry = DirectMapping {
                from: from.clone(),
                to: to.clone(),
                truncate: *truncate,
                default_literal: default_literal.clone(),
            };
            if let Some(idx) = direct.iter().position(|d| d.to == to.as_str()) {
                direct[idx] = new_entry;
            } else {
                direct.push(new_entry);
            }
        }
    }

    // Body / auto-pack synthesis. The body draws its values from the
    // **source** schema (we are encoding source rows), not the sink
    // universe — the sink column is the destination body slot, never a
    // row of source data. `source_columns` = (post-override direct
    // `from` columns, in slot order) ∪ (source schema columns not yet
    // consumed by an explicit `from`).
    //
    // Every `*:NAME` rule contributes a target sink column; the source
    // emits one body payload per row and the matrix fans it out into
    // each target slot.
    let body: Option<Body> = if body_targets.is_empty() {
        None
    } else {
        let src_s = src_schema.ok_or(ValidationError::WildcardWithoutSchema {
            flow: flow_name.to_string(),
        })?;
        let mut source_columns: Vec<String> = Vec::new();
        let mut seen: ahash::AHashSet<String> = ahash::AHashSet::new();
        for d in &direct {
            if seen.insert(d.from.clone()) {
                source_columns.push(d.from.clone());
            }
        }
        for field in src_s.fields() {
            if seen.insert(field.name.clone()) {
                source_columns.push(field.name.clone());
            }
        }
        Some(Body {
            source_columns,
            targets: body_targets,
        })
    };

    // Universe-size cap on the final shape — each pack target adds one
    // synthetic sink column.
    let pack_targets_len = body.as_ref().map(|p| p.targets.len()).unwrap_or(0);
    let total_cols = direct.len() + pack_targets_len;
    if total_cols > WILDCARD_UNIVERSE_CAP {
        return Err(ValidationError::WildcardUniverseTooLarge {
            flow: flow_name.to_string(),
            count: total_cols,
        });
    }

    // Sink uniqueness across `direct ∪ body.targets`.
    let pack_targets: &[String] = body.as_ref().map(|p| p.targets.as_slice()).unwrap_or(&[]);
    check_sink_uniqueness(&direct, pack_targets, flow_name)?;

    Ok(ExpandedMapping { direct, body })
}

impl ExpandedMapping {
    /// Source-side columns the runner must read per tick. Walks
    /// `direct.from` first, then appends each `body.source_columns`
    /// entry that isn't already in the list (deduplicated). Shared by
    /// the validation pipeline and the runner-side rebuild path so both
    /// produce byte-identical read columns.
    pub fn read_columns(&self) -> Vec<String> {
        let mut cols: Vec<String> = self.direct.iter().map(|d| d.from.clone()).collect();
        if let Some(jp) = &self.body {
            for col in &jp.source_columns {
                if !cols.iter().any(|c| c == col) {
                    cols.push(col.clone());
                }
            }
        }
        cols
    }

    /// Sink-side columns the runner must write per tick. Walks
    /// `direct.to` first, then appends each `body.targets` entry.
    /// Pack targets are post-expansion DuplicateSinkField-checked, so
    /// no dedupe is needed here.
    pub fn write_columns(&self) -> Vec<String> {
        let mut cols: Vec<String> = self.direct.iter().map(|d| d.to.clone()).collect();
        if let Some(jp) = &self.body {
            cols.extend(jp.targets.iter().cloned());
        }
        cols
    }
}

fn check_sink_uniqueness(
    direct: &[DirectMapping],
    body_targets: &[String],
    _flow_name: &str,
) -> Result<(), ValidationError> {
    use crate::mapping::FieldPath;

    let mut entries: Vec<(String, Option<FieldPath>)> =
        Vec::with_capacity(direct.len() + body_targets.len());
    for d in direct {
        let to = d.to.clone();
        // Propagate parse failures rather than swallowing them with `.ok()`:
        // a malformed sink path would otherwise silently skip ancestor/prefix
        // duplicate detection and surface only at runtime.
        let parsed = FieldPath::parse(&to).map_err(|e| ValidationError::InvalidFieldPath {
            path: to.clone(),
            source: e,
        })?;
        entries.push((to, Some(parsed)));
    }
    for to in body_targets {
        let parsed = FieldPath::parse(to).map_err(|e| ValidationError::InvalidFieldPath {
            path: to.clone(),
            source: e,
        })?;
        entries.push((to.clone(), Some(parsed)));
    }

    for (i, (to_i, path_i)) in entries.iter().enumerate() {
        for (j, (to_j, path_j)) in entries.iter().enumerate().take(i) {
            if to_i == to_j {
                return Err(ValidationError::DuplicateSinkField {
                    field: to_i.clone(),
                    first_index: j,
                    duplicate_index: i,
                    detail: String::new(),
                });
            }
            if let (Some(a), Some(b)) = (path_j, path_i)
                && (a.is_nested() || b.is_nested())
                && (a.is_prefix_or_equal(b) || b.is_prefix_or_equal(a))
            {
                return Err(ValidationError::DuplicateSinkField {
                    field: format!("{to_j} / {to_i}"),
                    first_index: j,
                    duplicate_index: i,
                    detail: " — one path is an ancestor of the other".to_string(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::model::{Field, Schema};
    use crate::types::DataType;

    fn field(name: &str, dt: DataType, nullable: bool) -> Field {
        Field {
            name: name.into(),
            data_type: dt,
            nullable,
        }
    }

    fn direct(from: &str, to: &str) -> ColumnMapping {
        ColumnMapping::Direct {
            from: from.into(),
            to: to.into(),
            truncate: false,
            default_literal: None,
        }
    }

    fn fs(from: &str, to: &str) -> DirectMapping {
        DirectMapping {
            from: from.into(),
            to: to.into(),
            truncate: false,
            default_literal: None,
        }
    }

    /// Sink schema preferred when both present.
    #[test]
    fn sink_schema_preferred_over_source() {
        let src = Schema::new(vec![
            field("a", DataType::Int32, false),
            field("extra", DataType::Int32, false),
        ]);
        let dst = Schema::new(vec![
            field("a", DataType::Int32, false),
            field("b", DataType::Int32, true),
        ]);
        // sink universe is {a, b}. Source has {a, extra}. `b` has no
        // matching source field and is nullable → omitted entirely
        // (sink writes the row without `b`; the column gets its DDL
        // default / NULL).
        let exp = expand(&[ColumnMapping::Wildcard], &src, &dst, false, false, "f").unwrap();
        assert_eq!(exp.direct, vec![fs("a", "a")]);
        assert!(exp.body.is_none());
    }

    /// Source-schema fallback when sink is schemaless.
    #[test]
    fn source_schema_fallback_when_sink_schemaless() {
        let src = Schema::new(vec![
            field("a", DataType::Int32, false),
            field("b", DataType::Text { size: None }, false),
        ]);
        let exp = expand(
            &[ColumnMapping::Wildcard],
            &src,
            &Schema::schemaless(),
            false,
            false,
            "f",
        )
        .unwrap();
        assert_eq!(exp.direct, vec![fs("a", "a"), fs("b", "b")]);
    }

    /// Schemaless-both with wildcard → body block with one `_root`
    /// target. Transform compiler lowers it to a single `Body`
    /// op; mongo sink writes the body document at root.
    #[test]
    fn schemaless_both_wildcard_only_returns_root_body_target() {
        let exp = expand(
            &[ColumnMapping::Wildcard],
            &Schema::schemaless(),
            &Schema::schemaless(),
            true,
            true,
            "f",
        )
        .unwrap();
        assert!(exp.direct.is_empty());
        let body = exp.body.as_ref().expect("body present");
        assert!(body.source_columns.is_empty());
        assert_eq!(body.targets, vec![ROOT_BODY_TARGET.to_string()]);
    }

    /// Schemaless-both with sample-derived schemas on both sides MUST
    /// still take the root-body fast path — samples are informational,
    /// not authoritative, so the wildcard-only mapping against
    /// `SchemalessWithSample(_)` on both sides bypasses schema-driven
    /// expansion exactly the same way as bare `Schemaless`.
    #[test]
    fn schemaless_both_with_samples_still_root_body() {
        let src_sample = Schema::schemaless_with_sample(vec![field("a", DataType::Int32, false)]);
        let dst_sample = Schema::schemaless_with_sample(vec![field("b", DataType::Int32, false)]);
        let exp = expand(
            &[ColumnMapping::Wildcard],
            &src_sample,
            &dst_sample,
            true,
            true,
            "f",
        )
        .unwrap();
        assert!(exp.direct.is_empty());
        let body = exp.body.as_ref().expect("body present");
        assert_eq!(body.targets, vec![ROOT_BODY_TARGET.to_string()]);
    }

    /// Mixed schemaless arms (one bare, one with sample) on the
    /// wildcard-only path also hit the root-body fast path.
    #[test]
    fn schemaless_both_mixed_arms_still_root_body() {
        let dst_sample = Schema::schemaless_with_sample(vec![field("b", DataType::Int32, false)]);
        let exp = expand(
            &[ColumnMapping::Wildcard],
            &Schema::schemaless(),
            &dst_sample,
            true,
            true,
            "f",
        )
        .unwrap();
        let body = exp.body.as_ref().expect("body present");
        assert_eq!(body.targets, vec![ROOT_BODY_TARGET.to_string()]);
    }

    /// Wildcard against a non-schemaless source whose introspection was
    /// disabled (collapsed to bare `Schema::schemaless()` upstream): the
    /// connector flag — not the schema's kind — gates the raw-passthrough
    /// fast path, so this case falls through to the typed expansion and
    /// surfaces `WildcardWithoutSchema` because no schema is available.
    #[test]
    fn wildcard_against_non_schemaless_source_without_schema_rejected() {
        let err = expand(
            &[ColumnMapping::Wildcard],
            &Schema::schemaless(),
            &Schema::schemaless(),
            false,
            true,
            "f",
        )
        .unwrap_err();
        assert!(matches!(err, ValidationError::WildcardWithoutSchema { .. }));
    }

    /// Only source schemaless but sink has a schema →
    /// expand against the sink schema (no root-body lowering).
    #[test]
    fn source_schemaless_but_sink_has_schema() {
        let dst = Schema::new(vec![field("a", DataType::Int32, false)]);
        let exp = expand(
            &[ColumnMapping::Wildcard],
            &Schema::schemaless(),
            &dst,
            false,
            false,
            "f",
        )
        .unwrap();
        // Source schema is None, sink schema present → universe is sink.
        // Source schema is None means we cannot detect "missing" so we
        // assume same-name source column exists. (Mongo source emits
        // that field via FieldPath::parse on read.)
        assert_eq!(exp.direct, vec![fs("a", "a")]);
        assert!(exp.body.is_none());
    }

    /// Only one side schemaless, no schema available →
    /// `WildcardWithoutSchema`.
    #[test]
    fn one_side_schemaless_no_schema_rejected() {
        let err = expand(
            &[ColumnMapping::Wildcard],
            &Schema::schemaless(),
            &Schema::schemaless(),
            false,
            false,
            "f",
        )
        .unwrap_err();
        assert!(matches!(err, ValidationError::WildcardWithoutSchema { .. }));
    }

    /// Override walkthrough (append after wildcard fan-out skip).
    /// `[{from=a,to=b}, "*"]`, sink `{b: nullable, c}`, source `{a, c}`.
    /// Wildcard fan-out: `b` has no source counterpart but is nullable →
    /// omitted; `c` survives. Override: explicit `a→b` does not match any
    /// surviving wildcard slot → appended. Final: [c→c, a→b].
    #[test]
    fn override_walkthrough_in_place_replacement() {
        let src = Schema::new(vec![
            field("a", DataType::Int32, false),
            field("c", DataType::Int32, false),
        ]);
        let dst = Schema::new(vec![
            field("b", DataType::Int32, true),
            field("c", DataType::Int32, false),
        ]);
        let exp = expand(
            &[direct("a", "b"), ColumnMapping::Wildcard],
            &src,
            &dst,
            false,
            false,
            "f",
        )
        .unwrap();
        assert_eq!(exp.direct, vec![fs("c", "c"), fs("a", "b")]);
    }

    /// In-place replacement when the wildcard slot survives fan-out
    /// because the source has the same-named column. `[{from=a,to=b}, "*"]`,
    /// sink `{b, c}` (both have source counterparts), source `{a, b, c}`.
    /// Wildcard fan-out → `[b→b, c→c]`. Override replaces slot 0 with `a→b`.
    #[test]
    fn override_walkthrough_in_place_when_slot_survives_pass_a() {
        let src = Schema::new(vec![
            field("a", DataType::Int32, false),
            field("b", DataType::Int32, false),
            field("c", DataType::Int32, false),
        ]);
        let dst = Schema::new(vec![
            field("b", DataType::Int32, false),
            field("c", DataType::Int32, false),
        ]);
        let exp = expand(
            &[direct("a", "b"), ColumnMapping::Wildcard],
            &src,
            &dst,
            false,
            false,
            "f",
        )
        .unwrap();
        assert_eq!(exp.direct, vec![fs("a", "b"), fs("c", "c")]);
    }

    /// Override walkthrough (append case).
    /// `["*", {from=a,to=b}]`, sink `{c}`, source `{a, c}`.
    /// Wildcard fan-out → `[c→c]`. Override appends `a→b`.
    #[test]
    fn override_walkthrough_append_case() {
        let src = Schema::new(vec![
            field("a", DataType::Int32, false),
            field("c", DataType::Int32, false),
        ]);
        let dst = Schema::new(vec![field("c", DataType::Int32, false)]);
        let exp = expand(
            &[ColumnMapping::Wildcard, direct("a", "b")],
            &src,
            &dst,
            false,
            false,
            "f",
        )
        .unwrap();
        assert_eq!(exp.direct, vec![fs("c", "c"), fs("a", "b")]);
    }

    /// Wildcard with empty sink schema → empty direct.
    /// No `WildcardUniverseTooLarge` (cap is 4096).
    #[test]
    fn wildcard_empty_universe_returns_empty_direct() {
        let src = Schema::new(vec![field("a", DataType::Int32, false)]);
        let dst = Schema::new(vec![]);
        let exp = expand(&[ColumnMapping::Wildcard], &src, &dst, false, false, "f").unwrap();
        assert!(exp.direct.is_empty());
        // The empty-universe outcome must also leave `body` empty —
        // the prior `direct.is_empty()` probe alone admitted any
        // combination.
        assert!(exp.body.is_none());
    }

    /// Sibling case to `wildcard_empty_universe_returns_empty_direct`:
    /// source has a one-column schema, sink schema is empty, mapping is
    /// `["*"]`. The universe is the sink schema, so the source column
    /// is **absent** from `direct` — the wildcard does not fall back to
    /// the source schema when the sink schema is present.
    #[test]
    fn wildcard_empty_sink_universe_omits_source_columns() {
        let src = Schema::new(vec![field("a", DataType::Int32, false)]);
        let dst = Schema::new(vec![]);
        let exp = expand(&[ColumnMapping::Wildcard], &src, &dst, false, false, "f").unwrap();
        assert!(
            exp.direct.iter().all(|d| d.from != "a"),
            "source column `a` must not leak into direct when sink universe is empty"
        );
        assert!(exp.direct.is_empty());
        assert!(exp.body.is_none());
    }

    /// `[{from=a,to=body}, "*:body"]` → `DuplicateSinkField`.
    /// Multiple `*:NAME` rules with distinct sink columns are allowed,
    /// so the failure here is the post-expansion duplicate-sink-field
    /// check: the explicit `Direct a → body` and the auto-pack
    /// `*:body` both target the same sink column `body`.
    #[test]
    fn body_collides_with_direct_on_sink_column() {
        let src = Schema::new(vec![field("a", DataType::Int32, false)]);
        let dst = Schema::new(vec![field("body", DataType::Json, false)]);
        let err = expand(
            &[
                direct("a", "body"),
                ColumnMapping::Body { to: "body".into() },
            ],
            &src,
            &dst,
            false,
            false,
            "f",
        )
        .unwrap_err();
        assert!(matches!(err, ValidationError::DuplicateSinkField { .. }));
    }

    /// Universe size 4097 → `WildcardUniverseTooLarge`.
    #[test]
    fn wildcard_universe_too_large() {
        let big_fields: Vec<Field> = (0..4097)
            .map(|i| field(&format!("c{i}"), DataType::Int32, false))
            .collect();
        let dst = Schema::new(big_fields);
        let err = expand(
            &[ColumnMapping::Wildcard],
            &Schema::schemaless(),
            &dst,
            false,
            false,
            "f",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ValidationError::WildcardUniverseTooLarge { count: 4097, .. }
        ));
    }

    /// Sink universe `{a, b: nullable}`, source `{a}`, `mapping=["*"]`
    /// → `b` is omitted entirely from the expansion. The sink writes
    /// the row without `b`; pg/mysql will use the column's DDL default
    /// / NULL, mongo simply doesn't set the path.
    #[test]
    fn nullable_missing_source_column_is_omitted() {
        let src = Schema::new(vec![field("a", DataType::Int32, false)]);
        let dst = Schema::new(vec![
            field("a", DataType::Int32, false),
            field("b", DataType::Int32, true),
        ]);
        let exp = expand(&[ColumnMapping::Wildcard], &src, &dst, false, false, "f").unwrap();
        assert_eq!(exp.direct, vec![fs("a", "a")]);
    }

    /// Sink universe with `b: NOT NULL` and missing source `b` →
    /// `WildcardMissingNonNullableSource`.
    #[test]
    fn null_inject_rejects_when_sink_not_null() {
        let src = Schema::new(vec![field("a", DataType::Int32, false)]);
        let dst = Schema::new(vec![
            field("a", DataType::Int32, false),
            field("b", DataType::Int32, false),
        ]);
        let err = expand(&[ColumnMapping::Wildcard], &src, &dst, false, false, "f").unwrap_err();
        assert!(matches!(
            err,
            ValidationError::WildcardMissingNonNullableSource { column, .. } if column == "b"
        ));
    }

    /// Source-driven universe (sink schemaless) with columns missing
    /// on sink: source `{a, b}`, sink schemaless, `mapping=["*"]` →
    /// both included. No null-inject path.
    #[test]
    fn source_driven_universe_no_null_inject() {
        let src = Schema::new(vec![
            field("a", DataType::Int32, false),
            field("b", DataType::Text { size: None }, false),
        ]);
        let exp = expand(
            &[ColumnMapping::Wildcard],
            &src,
            &Schema::schemaless(),
            false,
            false,
            "f",
        )
        .unwrap();
        assert_eq!(exp.direct, vec![fs("a", "a"), fs("b", "b")]);
    }

    /// JSON pack basic case — `["*:body"]` against pg `{body: Json}`,
    /// source `{a, b}` → direct empty, body with both source cols.
    #[test]
    fn body_basic() {
        let src = Schema::new(vec![
            field("a", DataType::Int32, false),
            field("b", DataType::Text { size: None }, false),
        ]);
        let dst = Schema::new(vec![field("body", DataType::Json, false)]);
        let exp = expand(
            &[ColumnMapping::Body { to: "body".into() }],
            &src,
            &dst,
            false,
            false,
            "f",
        )
        .unwrap();
        assert!(exp.direct.is_empty());
        let jp = exp.body.as_ref().unwrap();
        assert_eq!(jp.targets, vec!["body".to_string()]);
        assert_eq!(jp.source_columns, vec!["a".to_string(), "b".to_string()]);
    }

    /// JSON pack with key column: `["id", "*:body"]` against
    /// sink `{id, body}`, source `{id, name}` → direct=[id→id],
    /// body source_columns=[id, name].
    #[test]
    fn body_with_explicit_id() {
        let src = Schema::new(vec![
            field("id", DataType::Int64, false),
            field("name", DataType::Text { size: None }, false),
        ]);
        let dst = Schema::new(vec![
            field("id", DataType::Int64, false),
            field("body", DataType::Json, false),
        ]);
        let exp = expand(
            &[
                direct("id", "id"),
                ColumnMapping::Body { to: "body".into() },
            ],
            &src,
            &dst,
            false,
            false,
            "f",
        )
        .unwrap();
        assert_eq!(exp.direct, vec![fs("id", "id")]);
        let jp = exp.body.as_ref().unwrap();
        assert_eq!(jp.targets, vec!["body".to_string()]);
        // `id` already mapped via Direct → inherited as the first
        // source column; `name` (universe column not consumed by an
        // explicit override) appended.
        assert_eq!(
            jp.source_columns,
            vec!["id".to_string(), "name".to_string()]
        );
    }

    /// Multiple `*:NAME` rules with distinct sink columns — expansion
    /// produces one direct entry plus N json-pack plans, each carrying
    /// the same `source_columns`.
    #[test]
    fn multiple_body_distinct_targets() {
        let src = Schema::new(vec![
            field("id", DataType::Int64, false),
            field("name", DataType::Text { size: None }, false),
        ]);
        let dst = Schema::new(vec![
            field("id", DataType::Int64, false),
            field("body", DataType::Json, false),
            field("archive", DataType::Json, false),
        ]);
        let exp = expand(
            &[
                direct("id", "id"),
                ColumnMapping::Body { to: "body".into() },
                ColumnMapping::Body {
                    to: "archive".into(),
                },
            ],
            &src,
            &dst,
            false,
            false,
            "f",
        )
        .unwrap();
        assert_eq!(exp.direct, vec![fs("id", "id")]);
        let jp = exp.body.as_ref().unwrap();
        assert_eq!(jp.targets, vec!["body".to_string(), "archive".to_string()]);
        // The single shared body source-column list feeds every target.
        assert_eq!(
            jp.source_columns,
            vec!["id".to_string(), "name".to_string()]
        );
    }

    /// Two `*:NAME` rules with the **same** sink column collide on the
    /// post-expansion `DuplicateSinkField` check.
    #[test]
    fn multiple_body_same_target_rejected() {
        let src = Schema::new(vec![field("id", DataType::Int64, false)]);
        let dst = Schema::new(vec![field("body", DataType::Json, false)]);
        let err = expand(
            &[
                ColumnMapping::Body { to: "body".into() },
                ColumnMapping::Body { to: "body".into() },
            ],
            &src,
            &dst,
            false,
            false,
            "f",
        )
        .unwrap_err();
        assert!(matches!(err, ValidationError::DuplicateSinkField { .. }));
    }
}
