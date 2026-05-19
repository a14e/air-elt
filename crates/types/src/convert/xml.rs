//! XML well-formedness check + Xml ↔ Text conversions.
//!
//! `validate` is the single source of truth for "is this string a
//! well-formed XML document?" used both by `Text → Xml` runtime conversion
//! and the default-literal parser. "Well-formed" means: parses with
//! `quick-xml`, has exactly one top-level element, every close matches its
//! open. XML declarations, comments, and processing instructions are
//! tolerated; empty / whitespace-only / multi-root inputs are rejected.
//!
//! `xml_to_text(size)` is a pure serialise (XML is already a string)
//! followed by UTF-safe truncation when the sink is bounded — `Xml → Text*`
//! (unbounded) needs no truncation. `text_to_xml` validates via
//! [`validate`] so invalid markup never reaches a typed sink column.

use quick_xml::Reader;
use quick_xml::events::Event;

use super::error::ConvertError;
use super::text_truncate::truncate_chars;
use crate::{DataType, Value};

pub fn validate(s: &str) -> Result<(), String> {
    if s.trim().is_empty() {
        return Err("empty document".into());
    }
    let mut reader = Reader::from_str(s);
    let mut depth: i32 = 0;
    let mut roots: u32 = 0;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Err(e) => return Err(e.to_string()),
            Ok(Event::Eof) => break,
            Ok(Event::Start(_)) => {
                if depth == 0 {
                    roots += 1;
                    if roots > 1 {
                        return Err("multiple top-level elements".into());
                    }
                }
                depth += 1;
            }
            Ok(Event::End(_)) => {
                depth -= 1;
                if depth < 0 {
                    return Err("unbalanced closing tag".into());
                }
            }
            Ok(Event::Empty(_)) => {
                if depth == 0 {
                    roots += 1;
                    if roots > 1 {
                        return Err("multiple top-level elements".into());
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }
    if depth != 0 {
        return Err("unbalanced tags".into());
    }
    if roots == 0 {
        return Err("no element".into());
    }
    Ok(())
}

pub fn xml_to_text(
    value: Value,
    src: &DataType,
    sink_size: Option<u32>,
) -> Result<Value, ConvertError> {
    let s = match value {
        Value::Text(s) => s,
        _ => return Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
    };
    let out = match sink_size {
        None => s,
        Some(max) => truncate_chars(&s, max as usize).to_string(),
    };
    Ok(Value::Text(out))
}

pub fn text_to_xml(value: Value, src: &DataType) -> Result<Value, ConvertError> {
    let s = match value {
        Value::Text(s) => s,
        _ => return Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
    };
    validate(&s).map_err(|reason| ConvertError::InvalidXml { reason })?;
    Ok(Value::Text(s))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // ---- validate() ---------------------------------------------------

    #[test]
    fn empty_rejected() {
        assert!(validate("").is_err());
        assert!(validate("   \n\t").is_err());
    }

    #[test]
    fn single_self_closing_root_ok() {
        assert!(validate("<root/>").is_ok());
    }

    #[test]
    fn nested_ok() {
        assert!(validate("<a><b/><c>text</c></a>").is_ok());
    }

    #[test]
    fn multiple_roots_rejected() {
        assert!(validate("<a/><b/>").is_err());
        assert!(validate("<a></a><b></b>").is_err());
        assert!(validate("<a/><b></b>").is_err());
    }

    #[test]
    fn unbalanced_rejected() {
        assert!(validate("<a>").is_err());
        assert!(validate("</a>").is_err());
        assert!(validate("<a></b>").is_err());
    }

    #[test]
    fn xml_declaration_tolerated() {
        assert!(validate("<?xml version=\"1.0\"?><root/>").is_ok());
    }

    #[test]
    fn declaration_only_rejected() {
        // Declaration / PI alone — no element → reject.
        assert!(validate("<?xml version=\"1.0\"?>").is_err());
    }

    #[test]
    fn comment_only_rejected() {
        // Comment-only documents have no element — reject. This matches
        // the XML 1.0 production "document" which requires `prolog
        // element Misc*`.
        assert!(validate("<!-- hi -->").is_err());
    }

    #[test]
    fn root_with_attributes_ok() {
        assert!(validate("<root attr=\"x\" other=\"y\"/>").is_ok());
        assert!(validate("<root attr=\"x\"></root>").is_ok());
    }

    #[test]
    fn cdata_inside_root_ok() {
        assert!(validate("<a><![CDATA[<not-a-tag>]]></a>").is_ok());
    }

    #[test]
    fn comment_around_root_ok() {
        assert!(validate("<!-- prologue --><root/><!-- epilogue -->").is_ok());
    }

    // ---- xml_to_text() ------------------------------------------------

    #[test]
    fn xml_to_text_unbounded_passthrough() {
        let out = xml_to_text(Value::Text("<a/>".into()), &DataType::Xml, None).unwrap();
        assert_eq!(out, Value::Text("<a/>".into()));
    }

    #[test]
    fn xml_to_text_bounded_truncates() {
        let out = xml_to_text(Value::Text("<aaa/>".into()), &DataType::Xml, Some(3)).unwrap();
        assert_eq!(out, Value::Text("<aa".into()));
    }

    #[test]
    fn xml_to_text_size_zero_yields_empty() {
        let out = xml_to_text(Value::Text("<a/>".into()), &DataType::Xml, Some(0)).unwrap();
        assert_eq!(out, Value::Text(String::new()));
    }

    #[test]
    fn xml_to_text_value_shape_mismatch() {
        let res = xml_to_text(Value::Int32(1), &DataType::Xml, None);
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    // ---- text_to_xml() ------------------------------------------------

    #[test]
    fn text_to_xml_well_formed_passes() {
        let out = text_to_xml(
            Value::Text("<root/>".into()),
            &DataType::Text { size: Some(36) },
        )
        .unwrap();
        assert_eq!(out, Value::Text("<root/>".into()));
    }

    #[test]
    fn text_to_xml_malformed_rejected() {
        let res = text_to_xml(
            Value::Text("<root>".into()),
            &DataType::Text { size: Some(36) },
        );
        assert!(matches!(res, Err(ConvertError::InvalidXml { .. })));
    }

    #[test]
    fn text_to_xml_empty_rejected() {
        let res = text_to_xml(
            Value::Text(String::new()),
            &DataType::Text { size: Some(36) },
        );
        assert!(matches!(res, Err(ConvertError::InvalidXml { .. })));
    }

    #[test]
    fn text_to_xml_value_shape_mismatch() {
        let res = text_to_xml(Value::Int32(1), &DataType::Text { size: None });
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }
}
