//! Dot-notation field paths.
//!
//! `FieldPath` parses an operator-supplied identifier like `"a.b.c"` into
//! its segments. Each segment is validated via the project-wide
//! identifier rules (`air_elt_commons::identifier::validate_segment`),
//! so paths cannot smuggle quotes, whitespace, or empty segments past
//! the validator.
//!
//! In MVP only the MongoDB connector traverses nested paths. Postgres
//! / MySQL connectors require flat identifiers and reject any path with
//! `is_nested() == true` at validation time.

use std::fmt;

use thiserror::Error;

use air_elt_commons::identifier::{IdentifierError, validate_segment};

#[derive(Debug, Error)]
pub enum FieldPathError {
    #[error("field path is empty")]
    Empty,

    #[error("field path {path:?} contains an empty segment")]
    EmptySegment { path: String },

    #[error("field path {path:?}: invalid segment {segment:?}: {source}")]
    InvalidSegment {
        path: String,
        segment: String,
        #[source]
        source: IdentifierError,
    },
}

/// Parsed dot-notation field path.
///
/// `segments` is guaranteed non-empty and each segment is a valid
/// identifier per `validate_segment`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldPath {
    segments: Vec<String>,
}

impl FieldPath {
    pub fn parse(raw: &str) -> Result<Self, FieldPathError> {
        if raw.is_empty() {
            return Err(FieldPathError::Empty);
        }
        let segments: Vec<String> = raw.split('.').map(|s| s.to_string()).collect();
        Self::from_segments(segments)
    }

    /// Construct from already-split segments, skipping the `split`
    /// step in `parse`. Used by Mongo schema inference, which walks a
    /// borrowed segment stack and shouldn't pay for a re-join +
    /// re-split round trip.
    pub fn from_segments(segments: Vec<String>) -> Result<Self, FieldPathError> {
        if segments.is_empty() {
            return Err(FieldPathError::Empty);
        }
        for seg in &segments {
            if seg.is_empty() {
                return Err(FieldPathError::EmptySegment {
                    path: segments.join("."),
                });
            }
            if let Err(source) = validate_segment(seg) {
                return Err(FieldPathError::InvalidSegment {
                    path: segments.join("."),
                    segment: seg.clone(),
                    source,
                });
            }
        }
        Ok(Self { segments })
    }

    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    pub fn is_nested(&self) -> bool {
        self.segments.len() > 1
    }

    pub fn first(&self) -> &str {
        &self.segments[0]
    }

    /// `true` when `self` equals `other` or when `self` is a strict
    /// prefix of `other` segment-wise. Used by the duplicate-sink-field
    /// check to flag ambiguous mappings like `to = "a"` paired with
    /// `to = "a.b"` for nested-document sinks.
    pub fn is_prefix_or_equal(&self, other: &FieldPath) -> bool {
        if self.segments.len() > other.segments.len() {
            return false;
        }
        self.segments
            .iter()
            .zip(other.segments.iter())
            .all(|(a, b)| a == b)
    }
}

impl fmt::Display for FieldPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.segments.join("."))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_flat() {
        let p = FieldPath::parse("id").unwrap();
        assert_eq!(p.segments(), &["id"]);
        assert!(!p.is_nested());
        assert_eq!(p.to_string(), "id");
    }

    #[test]
    fn parses_nested() {
        let p = FieldPath::parse("address.city").unwrap();
        assert_eq!(p.segments(), &["address", "city"]);
        assert!(p.is_nested());
        assert_eq!(p.to_string(), "address.city");
    }

    #[test]
    fn empty_rejected() {
        assert!(matches!(FieldPath::parse(""), Err(FieldPathError::Empty)));
    }

    #[test]
    fn empty_segment_rejected() {
        assert!(matches!(
            FieldPath::parse("a..b"),
            Err(FieldPathError::EmptySegment { .. })
        ));
        assert!(matches!(
            FieldPath::parse(".a"),
            Err(FieldPathError::EmptySegment { .. })
        ));
        assert!(matches!(
            FieldPath::parse("a."),
            Err(FieldPathError::EmptySegment { .. })
        ));
    }

    #[test]
    fn quote_in_segment_rejected() {
        assert!(matches!(
            FieldPath::parse("a.\"b\""),
            Err(FieldPathError::InvalidSegment { .. })
        ));
    }

    #[test]
    fn prefix_relations() {
        let a = FieldPath::parse("a").unwrap();
        let ab = FieldPath::parse("a.b").unwrap();
        let ac = FieldPath::parse("a.c").unwrap();
        let b = FieldPath::parse("b").unwrap();
        assert!(a.is_prefix_or_equal(&ab));
        assert!(a.is_prefix_or_equal(&a));
        assert!(!ab.is_prefix_or_equal(&a));
        assert!(!ab.is_prefix_or_equal(&ac));
        assert!(!a.is_prefix_or_equal(&b));
    }
}
