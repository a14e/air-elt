//! Single-source XML well-formedness check shared by `xml_text` and the
//! default-literal parser.
//!
//! "Well-formed" here means: parses with `quick-xml`, has exactly one
//! top-level element, and every element close matches the open. Comments,
//! processing instructions, and the XML declaration are tolerated alongside
//! the single root element. Empty / whitespace-only input is rejected.

use quick_xml::Reader;
use quick_xml::events::Event;

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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

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
    fn comment_only_rejected() {
        // Comment-only documents have no element — reject. This matches
        // the XML 1.0 production "document" which requires `prolog
        // element Misc*`.
        assert!(validate("<!-- hi -->").is_err());
    }
}
