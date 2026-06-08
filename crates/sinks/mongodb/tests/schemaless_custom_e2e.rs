//! Regression test: a Mongo sink (schemaless) must accept a source
//! schema that carries `DataType::Custom(MongoObjectIdType)` without
//! tripping the matrix.
//!
//! The validation pipeline already special-cases `Sink::schemaless()`
//! by rebuilding `dst_schema` from the source schema, so `check_mapping`
//! sees `Custom == Custom` (identity short-circuit). This test guards
//! against future refactors that re-introduce a matrix call on
//! schemaless sinks for Custom-typed columns.

#![allow(clippy::unwrap_used)]

use air_elt_commons_mongodb::types::{MongoJsType, MongoObjectIdType};
use air_elt_core::mapping::DirectMapping;
use air_elt_core::model::{Field, Schema};
use air_elt_core::types::DataType;
use air_elt_core::validation::checks::check_mapping;

#[test]
fn check_mapping_object_id_identity_is_compatible() {
    let src_schema = Schema::new(vec![
        Field {
            name: "_id".into(),
            data_type: DataType::Custom(Box::new(MongoObjectIdType)),
            nullable: true,
        },
        Field {
            name: "name".into(),
            data_type: DataType::Text { size: None },
            nullable: true,
        },
    ]);
    // Schemaless sinks rebuild dst_schema from source; reproduce that
    // shape literally to exercise the matrix Custom-Custom identity arm.
    let sink_schema = src_schema.clone();
    let mappings = vec![
        DirectMapping {
            from: "_id".into(),
            to: "_id".into(),
            truncate: false,
            default_literal: None,
            switch: None,
            compute: None,
        },
        DirectMapping {
            from: "name".into(),
            to: "name".into(),
            truncate: false,
            default_literal: None,
            switch: None,
            compute: None,
        },
    ];
    check_mapping(&src_schema, &sink_schema, &mappings).expect("custom identity must be accepted");
}

#[test]
fn check_mapping_javascript_identity_is_compatible() {
    let src_schema = Schema::new(vec![Field {
        name: "code".into(),
        data_type: DataType::Custom(Box::new(MongoJsType)),
        nullable: true,
    }]);
    let sink_schema = src_schema.clone();
    let mappings = vec![DirectMapping {
        from: "code".into(),
        to: "code".into(),
        truncate: false,
        default_literal: None,
        switch: None,
        compute: None,
    }];
    check_mapping(&src_schema, &sink_schema, &mappings).expect("custom identity must be accepted");
}
