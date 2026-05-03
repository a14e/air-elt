//! Read / write nested BSON values via `core::mapping::FieldPath`.
//!
//! Mongo documents are tree-shaped, so `from = "address.city"` in a
//! mapping descends through `address`, then reads `city`. Writes do
//! the same in reverse: building parent documents on the way down if
//! they don't exist yet. Arrays are *not* indexable through the path
//! grammar — `a.0` would be parsed as a literal segment named `"0"`,
//! not as array index zero. This matches the project's "minimal
//! transformation" stance: we don't reshape, we move data field by
//! field.

use bson::{Bson, Document};

use air_elt_core::mapping::FieldPath;

/// Read the value at `path` from `doc`. Missing intermediate
/// documents return `None`. A non-document encountered mid-path also
/// returns `None` (the caller treats that as a missing field).
pub fn get<'a>(doc: &'a Document, path: &FieldPath) -> Option<&'a Bson> {
    let segs = path.segments();
    let mut cursor: Option<&Bson> = doc.get(&segs[0]);
    for seg in &segs[1..] {
        cursor = match cursor {
            Some(Bson::Document(inner)) => inner.get(seg),
            _ => return None,
        };
    }
    cursor
}

/// Write `value` at `path` into `doc`, creating any missing
/// intermediate documents along the way. If an intermediate slot
/// already holds a non-document, it is overwritten with a fresh
/// document — operators that opt into nested writes have already
/// declared via the `to` path that they expect a tree there.
pub fn set(doc: &mut Document, path: &FieldPath, value: Bson) {
    let segs = path.segments();
    if segs.len() == 1 {
        doc.insert(&segs[0], value);
        return;
    }
    let (head, rest) = segs
        .split_first()
        .expect("path is non-empty by construction");
    let entry = doc
        .entry(head.clone())
        .or_insert_with(|| Bson::Document(Document::new()));
    if !matches!(entry, Bson::Document(_)) {
        *entry = Bson::Document(Document::new());
    }
    if let Bson::Document(inner) = entry {
        set_segments(inner, rest, value);
    }
}

fn set_segments(doc: &mut Document, segs: &[String], value: Bson) {
    if segs.len() == 1 {
        doc.insert(&segs[0], value);
        return;
    }
    let (head, rest) = segs.split_first().expect("non-empty");
    let entry = doc
        .entry(head.clone())
        .or_insert_with(|| Bson::Document(Document::new()));
    if !matches!(entry, Bson::Document(_)) {
        *entry = Bson::Document(Document::new());
    }
    if let Bson::Document(inner) = entry {
        set_segments(inner, rest, value);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn get_flat() {
        let d = doc! { "id": 1_i32, "name": "alice" };
        let p = FieldPath::parse("id").unwrap();
        assert_eq!(get(&d, &p), Some(&Bson::Int32(1)));
    }

    #[test]
    fn get_nested() {
        let d = doc! { "addr": { "city": "Berlin" } };
        let p = FieldPath::parse("addr.city").unwrap();
        assert_eq!(get(&d, &p), Some(&Bson::String("Berlin".into())));
    }

    #[test]
    fn get_missing_returns_none() {
        let d = doc! { "addr": { "city": "Berlin" } };
        let p = FieldPath::parse("addr.zip").unwrap();
        assert_eq!(get(&d, &p), None);
    }

    #[test]
    fn get_through_non_document_returns_none() {
        let d = doc! { "addr": "Berlin" };
        let p = FieldPath::parse("addr.city").unwrap();
        assert_eq!(get(&d, &p), None);
    }

    #[test]
    fn set_flat() {
        let mut d = Document::new();
        let p = FieldPath::parse("id").unwrap();
        set(&mut d, &p, Bson::Int64(7));
        assert_eq!(d.get("id"), Some(&Bson::Int64(7)));
    }

    #[test]
    fn set_nested_creates_parent() {
        let mut d = Document::new();
        let p = FieldPath::parse("addr.city").unwrap();
        set(&mut d, &p, Bson::String("Berlin".into()));
        let inner = d.get_document("addr").unwrap();
        assert_eq!(inner.get_str("city").unwrap(), "Berlin");
    }

    #[test]
    fn set_overwrites_scalar_with_document() {
        let mut d = doc! { "addr": "Berlin" };
        let p = FieldPath::parse("addr.city").unwrap();
        set(&mut d, &p, Bson::String("Munich".into()));
        let inner = d.get_document("addr").unwrap();
        assert_eq!(inner.get_str("city").unwrap(), "Munich");
    }

    #[test]
    fn set_three_levels() {
        let mut d = Document::new();
        let p = FieldPath::parse("a.b.c").unwrap();
        set(&mut d, &p, Bson::Int32(42));
        assert_eq!(
            d.get_document("a")
                .unwrap()
                .get_document("b")
                .unwrap()
                .get_i32("c")
                .unwrap(),
            42
        );
    }
}
