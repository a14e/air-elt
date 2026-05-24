pub mod ast;
pub(crate) mod detect;
pub mod error;
pub mod evaluator;
pub(crate) mod lexer;
pub mod parser;
pub(crate) mod token;
pub mod type_check;

pub use ast::{Expr, InterpolationSegment, LiteralValue, Program, Statement};
pub use detect::{has_interpolation, is_expression};
pub use error::ExprError;
pub use evaluator::{eval_expression, eval_interpolated, evaluate};
pub use parser::parse;
pub use type_check::{infer_expression_type, infer_type};
