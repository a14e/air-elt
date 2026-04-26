//! Cross-type value conversion shared across all connectors.
//!
//! The flow runner calls [`convert`] per cell only when the validated source
//! and sink `DataType`s differ for that column. Identity columns skip the
//! call entirely. Connectors stay ignorant of these rules — UUID stored in a
//! MySQL `BINARY(16)` column is encoded *here*, not in the MySQL crate.
//!
//! `Value::Null` is preserved unchanged regardless of `(src, dst)` —
//! nullability mismatches are caught earlier by `NullabilityMismatch` in
//! `validate`, not here.

pub mod dispatch;
pub mod error;
pub mod uuid;

pub use dispatch::convert;
pub use error::ConvertError;
