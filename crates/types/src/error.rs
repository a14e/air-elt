use thiserror::Error;

/// Errors produced by `value_to_json` and the source-side body-fill
/// path.
///
/// A sibling of `TypeError` — JSON encoding has its own contract
/// (depth cap, size cap, custom-type delegation) and folding it into
/// `TypeError` would muddle the variant set used by the matrix.
#[derive(Debug, Error)]
pub enum JsonEncodeError {
    /// A `Value` variant that has no JSON encoding rule, or a custom
    /// type whose `DynValue::to_json` default fired.
    #[error("json encode failure: {0}")]
    Variant(String),

    /// Recursive `Value::Json` payload deeper than `MAX_JSON_DEPTH`
    /// (see `crate::json_encode::MAX_JSON_DEPTH`).
    #[error("json encode depth exceeded the configured cap")]
    DepthExceeded,

    /// A `DynValue::to_json` impl returned an error. Wraps the inner
    /// reason as a string — the trait method does not enforce a
    /// concrete error type.
    #[error("custom value to_json failed: {0}")]
    CustomFailed(String),
}
