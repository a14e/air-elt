//! Cross-cutting helper modules shared across the optimizer's passes and rules,
//! grouped here to keep the crate root focused on the pipeline stages:
//! - [`fallibility`] — the `can_fail` over-approximation used to decide when an
//!   evaluation may be dropped.
//! - [`type_utils`] — the typed-rule predicates (purity, drop-safety, numeric
//!   classification) consulted by the type-gated rewrites.

pub(crate) mod fallibility;
pub(crate) mod type_utils;
