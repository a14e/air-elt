//! `type_supported` rejection logic.
//!
//! QuestDB cannot create `XML` / `UNION` / unsigned columns at DDL level,
//! so we cannot drive a real cluster through `validate_access` with an
//! `Xml` column. Instead we call the gating helper directly with each
//! canonical type that QuestDB declines and assert the rejection.

use air_elt_commons_questdb::types::geohash::QuestDbGeohashType;
use air_elt_commons_questdb::types::ipv4::QuestDbIpv4Type;
use air_elt_commons_questdb::types::long256::QuestDbLong256Type;
use air_elt_commons_questdb::types::symbol::QuestDbSymbolType;
use air_elt_core::types::data_type::DataType;
use air_elt_sink_questdb::type_supported;

#[test]
fn xml_is_rejected() {
    assert!(!type_supported(&DataType::Xml));
}

#[test]
fn union_is_rejected() {
    assert!(!type_supported(&DataType::Union(vec![
        DataType::Int32,
        DataType::Text { size: None },
    ])));
}

#[test]
fn unsigned_ints_are_rejected() {
    for dt in [
        DataType::UInt8,
        DataType::UInt16,
        DataType::UInt32,
        DataType::UInt64,
    ] {
        assert!(
            !type_supported(&dt),
            "unsigned type {dt:?} must be rejected"
        );
    }
}

#[test]
fn bigint_decimal_rejected_without_truncate() {
    // BigInt / Decimal need an explicit truncate to Float64 in the
    // mapping; raw BigInt / Decimal cannot land in any QuestDB column.
    assert!(!type_supported(&DataType::BigInt { width: Some(38) }));
    assert!(!type_supported(&DataType::Decimal {
        precision: Some(20),
        scale: Some(4),
    }));
}

#[test]
fn questdb_native_customs_are_accepted() {
    let cases: Vec<DataType> = vec![
        DataType::Custom(Box::new(QuestDbSymbolType)),
        DataType::Custom(Box::new(QuestDbLong256Type)),
        DataType::Custom(Box::new(QuestDbIpv4Type)),
        DataType::Custom(Box::new(QuestDbGeohashType { bits: 35 })),
    ];
    for dt in cases {
        assert!(
            type_supported(&dt),
            "questdb native custom type {dt:?} must be accepted"
        );
    }
}

#[test]
fn canonical_supported_types_are_accepted() {
    for dt in [
        DataType::Bool,
        DataType::Int8,
        DataType::Int16,
        DataType::Int32,
        DataType::Int64,
        DataType::Float32,
        DataType::Float64,
        DataType::Text { size: None },
        DataType::Bytes { size: None },
        DataType::Date,
        DataType::Timestamp,
        DataType::Uuid,
        DataType::Json,
    ] {
        assert!(
            type_supported(&dt),
            "canonical type {dt:?} must be accepted"
        );
    }
}
