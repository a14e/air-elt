//! Cross-type value conversion shared across all connectors.
//!
//! The flow runner calls [`convert`] per cell only when the validated source
//! and sink `DataType`s differ for that column. Identity columns skip the
//! call entirely. Connectors stay ignorant of these rules — UUID stored in a
//! MySQL `BINARY(16)` column is encoded *here*, not in the MySQL crate.
//!
//! `Value::Null` is preserved unchanged regardless of `(src, dst)` unless
//! the per-mapping [`ConversionContext::default`] supplies a fallback —
//! nullability mismatches without a default are caught earlier by
//! `NullabilityMismatch` in `validate`, not here.

pub mod bigint_narrow;
pub mod bytes_narrow;
pub mod context;
pub mod decimal_narrow;
pub mod decimal_to_float;
pub mod dispatch;
pub mod error;
pub mod float_narrow;
pub mod int_narrow;
pub mod json_text;
pub mod saturate;
pub mod text_bool;
pub mod text_narrow;
pub mod text_truncate;
pub mod timestamp_date;
pub mod uuid;
pub mod xml;

pub use context::ConversionContext;
pub use dispatch::convert;
pub use error::ConvertError;
