//! Expression language type system for Air Elt.
//!
//! Uses the canonical `DataType` from `air-elt-types` directly.
//! `NullableExprType` wraps `DataType` with nullability tracking
//! and optional `int_bound` for precise integer width propagation.

pub mod bounds;
pub mod error;
pub mod limits;
pub mod nullable;

pub use bounds::{ArithmeticOp, arithmetic_result_type, concat_result_type};
pub use error::ExprTypeError;
pub use limits::*;
pub use nullable::NullableExprType;
