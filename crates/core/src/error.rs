use std::path::PathBuf;

use thiserror::Error;

use air_elt_commons::identifier::IdentifierError;

use crate::types::convert::ConvertError;
use crate::types::data_type::DataType;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse toml in {path:?}: {source}")]
    TomlParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("failed to parse yaml in {path:?}: {source}")]
    YamlParse {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },

    #[error(
        "config file {path:?} has unsupported extension {ext:?} — expected .toml, .yml, or .yaml"
    )]
    UnknownConfigExtension { path: PathBuf, ext: String },

    #[error(
        "duplicate secret key {key:?} declared in {first:?} and {second:?} — \
         each secret must be defined exactly once across all included files"
    )]
    DuplicateSecret {
        key: String,
        first: PathBuf,
        second: PathBuf,
    },

    #[error("failed to resolve env var {var}: {source}")]
    EnvVar {
        var: String,
        #[source]
        source: std::env::VarError,
    },

    #[error(
        "unresolved reference ${{{name}}} — not in env, not in [secrets], and no default provided"
    )]
    UnresolvedReference { name: String },

    #[error("config exceeds {max} bytes (read {actual})")]
    ConfigTooLarge { max: u64, actual: u64 },

    #[error("include path {path:?} is absolute — only relative paths are allowed")]
    AbsoluteIncludeNotAllowed { path: String },

    #[error("duplicate flow name {name:?} across included files")]
    DuplicateFlow { name: String },

    #[error("duplicate {kind} name {name:?}")]
    DuplicateName { kind: &'static str, name: String },

    #[error("invalid identifier {value:?}: contains unsupported character {ch:?}")]
    InvalidIdentifier { value: String, ch: char },

    #[error("unsupported in MVP: {what}")]
    UnsupportedInMvp { what: String },

    #[error("config validation failed: {reason}")]
    Invalid { reason: String },

    #[error("sink {sink:?} does not support conflict resolution: {hint}")]
    ConflictNotSupported { sink: String, hint: String },
}

#[derive(Debug, Error)]
pub enum TypeError {
    #[error("narrowing conversion from {from:?} to {to:?} is not allowed")]
    NarrowingNotAllowed { from: DataType, to: DataType },

    #[error("no cast from {from:?} to {to:?}")]
    UnsupportedCast { from: DataType, to: DataType },

    #[error("value does not match declared type {expected:?}: {actual:?}")]
    ValueTypeMismatch {
        expected: DataType,
        actual: &'static str,
    },

    #[error("unsupported source type {native:?}")]
    UnsupportedNativeType { native: String },

    #[error(
        "column {column:?}: canonical `Null` has no native representation — a sink column's \
         DataType cannot be `Null`"
    )]
    NullSinkColumn { column: String },

    #[error(
        "column {column:?}: value type {got_kind} is not supported for sink column \
         declared as {expected}"
    )]
    SinkValueUnsupported {
        column: String,
        expected: String,
        got_kind: String,
    },
}

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("source {0:?} referenced by flow is not declared")]
    UnknownSource(String),

    #[error("sink {0:?} referenced by flow is not declared")]
    UnknownSink(String),

    #[error("storage {0:?} referenced by flow is not declared")]
    UnknownStorage(String),

    #[error("field {field:?} referenced in mapping is missing in {side} schema")]
    MissingField { side: &'static str, field: String },

    #[error("field {field:?}: type {from:?} is not castable to {to:?}: {source}")]
    IncompatibleTypes {
        field: String,
        from: DataType,
        to: DataType,
        #[source]
        source: TypeError,
    },

    #[error(
        "field {field:?}: source is nullable (source_nullable=true, sink_nullable={sink_nullable}) \
         but sink column is NOT NULL — declare the sink column nullable or add a default"
    )]
    NullabilityMismatch {
        field: String,
        source_nullable: bool,
        sink_nullable: bool,
    },

    #[error("access check failed for {component} {name:?}: {source}")]
    AccessFailed {
        component: &'static str,
        name: String,
        #[source]
        source: Box<RuntimeError>,
    },

    #[error("cursor field {field:?} is missing in source schema for flow {flow:?}")]
    MissingCursorField { flow: String, field: String },

    #[error("cursor field '{field}' has type {data_type} which cannot be used as a cursor")]
    CursorTypeUnsupported { field: String, data_type: DataType },

    #[error(
        "field {column:?}: `default` is set but the source column is NOT NULL — \
         the default would never be applied"
    )]
    DefaultOnNotNullSource { flow: String, column: String },

    #[error(
        "flow {flow:?} field {column:?}: `default` requires `validation.fields = true` so \
         the sink type can be resolved; with `fields = false` no schema introspection runs"
    )]
    DefaultRequiresFields { flow: String, column: String },

    #[error("field {column:?}: failed to parse default literal: {source}")]
    DefaultParse {
        flow: String,
        column: String,
        #[source]
        source: crate::types::default_value::DefaultParseError,
    },

    #[error(
        "mapping declares the same sink field {field:?} twice (entries {first_index} and {duplicate_index}){detail}"
    )]
    DuplicateSinkField {
        field: String,
        first_index: usize,
        duplicate_index: usize,
        /// Optional clarifier appended to the error message — used to
        /// distinguish the plain "two entries with identical `to`" case
        /// from the nested-path "one is a prefix of the other" case.
        /// Empty string when no extra context is needed.
        detail: String,
    },

    #[error("field path {path:?} in mapping is invalid: {source}")]
    InvalidFieldPath {
        path: String,
        #[source]
        source: crate::mapping::FieldPathError,
    },

    #[error(
        "sampling validation failed for flow {flow:?}: row {row_index} column {field:?} ({source_type:?} -> {sink_type:?}): {detail}"
    )]
    SamplingFailed {
        flow: String,
        row_index: usize,
        field: String,
        source_type: DataType,
        sink_type: DataType,
        detail: String,
    },

    #[error(
        "flow {flow:?}: wildcard mapping ('*' / '*:*') requires a schema on at least one side, \
         but neither source nor sink exposed one and raw passthrough is unavailable"
    )]
    WildcardWithoutSchema { flow: String },

    #[error(
        "flow {flow:?}: wildcard expansion produced {count} columns which exceeds the 4096-column cap"
    )]
    WildcardUniverseTooLarge { flow: String, count: usize },

    #[error(
        "flow {flow:?}: wildcard expansion against the sink schema requires source column {column:?}, \
         but the source schema does not contain it and the sink column is NOT NULL"
    )]
    WildcardMissingNonNullableSource { flow: String, column: String },

    #[error(
        "flow {flow:?}: cursor.fields cannot be set on a raw-passthrough wildcard flow — \
         declare explicit cursor columns alongside the wildcard or remove cursor.fields"
    )]
    CursorRequiresExplicitFields { flow: String },

    #[error(
        "flow {flow:?}: conflict.key entry {key:?} cannot resolve under raw-passthrough wildcard — \
         declare explicit mapping entries for the key columns"
    )]
    ConflictKeyNotInMapping { flow: String, key: String },

    #[error(
        "table {table:?}: designated timestamp column {column:?} is not in mapping — \
         QuestDB requires the designated timestamp column to be written"
    )]
    MissingDesignatedTimestamp { table: String, column: String },

    #[error(
        "sink {sink:?} cannot accept type {type_name:?} for column {column:?} in table {table:?}: {hint}"
    )]
    UnsupportedSinkType {
        sink: String,
        table: String,
        column: String,
        type_name: String,
        hint: String,
    },

    #[error(
        "sink {sink:?}: target table {table:?} does not exist — schema introspection returned no columns"
    )]
    SinkTableNotFound { sink: String, table: String },
}

/// Errors produced by `value_to_json` and the source-side body-fill
/// path.
///
/// A sibling of [`TypeError`] — JSON encoding has its own contract
/// (depth cap, size cap, custom-type delegation) and folding it into
/// `TypeError` would muddle the variant set used by the matrix.
#[derive(Debug, Error)]
pub enum JsonEncodeError {
    /// A `Value` variant that has no JSON encoding rule, or a custom
    /// type whose `DynValue::to_json` default fired.
    #[error("json encode failure: {0}")]
    Variant(String),

    /// Recursive `Value::Json` payload deeper than `MAX_JSON_DEPTH`
    /// (see `crate::types::json_encode::MAX_JSON_DEPTH`).
    #[error("json encode depth exceeded the configured cap")]
    DepthExceeded,

    /// A `DynValue::to_json` impl returned an error. Wraps the inner
    /// reason as a string — the trait method does not enforce a
    /// concrete error type.
    #[error("custom value to_json failed: {0}")]
    CustomFailed(String),
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("backend error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("type error: {0}")]
    Type(#[from] TypeError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("flow {flow:?} aborted: {reason}")]
    FlowAborted { flow: String, reason: String },

    #[error("component {component:?} not registered in factory registry")]
    NotRegistered { component: String },

    #[error("config error: {0}")]
    Config(#[from] ConfigError),

    #[error("flow {flow:?} operation {op} timed out after {after:?}")]
    Timeout {
        flow: String,
        op: &'static str,
        after: std::time::Duration,
    },

    #[error("flow {flow:?} operation {op} cancelled by shutdown")]
    Cancelled { flow: String, op: &'static str },

    #[error("context type mismatch: expected {expected}")]
    ContextMismatch { expected: &'static str },

    #[error("schema for table {table:?} is missing column {column:?}")]
    SchemaColumnMissing { table: String, column: String },

    #[error("value conversion failed: {0}")]
    Conversion(#[from] ConvertError),

    #[error("invalid identifier: {0}")]
    Identifier(#[from] IdentifierError),

    /// Validation error surfaced at runtime — typically from
    /// `rebuild_derived` when live schemas reveal a constraint that the
    /// pre-flight pipeline could not see (e.g. a column that vanished
    /// between snapshots). Distinct from config-time validation: it is
    /// raised by the runner, not the loader. Preserves the
    /// `ValidationError` variant + source chain for telemetry.
    #[error("validation: {0}")]
    Validation(#[from] ValidationError),

    #[error("json encode error: {0}")]
    JsonEncode(#[from] JsonEncodeError),

    /// A derived-plan invariant was violated at runtime — the validation
    /// pipeline / expansion stage was supposed to guarantee the
    /// condition. Carries an operator-facing detail string. Distinct
    /// from `Other` so logs and telemetry can spot upstream-build bugs
    /// without text-matching.
    #[error("derived plan invariant: {detail}")]
    DerivedPlanInvariant { detail: String },

    #[error("{0}")]
    Other(String),
}

impl RuntimeError {
    pub fn backend<E>(err: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        RuntimeError::Backend(Box::new(err))
    }
}

pub type RuntimeResult<T> = Result<T, RuntimeError>;
