/// Maximum nesting depth for expression AST nodes.
pub const MAX_EXPR_DEPTH: usize = 128;

/// Maximum length of a single expression source string in bytes.
pub const MAX_EXPR_SOURCE_LEN: usize = 65_536;

/// Maximum number of AST nodes in a single parsed expression.
pub const MAX_AST_NODES: usize = 10_000;

/// Maximum number of arguments a function call can accept.
pub const MAX_FUNCTION_ARGS: usize = 128;

/// Maximum number of distinct variables in a single expression scope.
pub const MAX_VARIABLES: usize = 64;

/// Maximum width (decimal digits) for BigInt arithmetic results.
pub const MAX_BIGINT_WIDTH: u32 = 1_024;

/// Maximum byte length for string values produced by expression evaluation.
pub const MAX_EXPR_STRING_BYTES: usize = 1_048_576;

/// Maximum byte length for expression source files.
pub const MAX_EXPR_FILE_BYTES: usize = 1_048_576;

/// Maximum nesting depth for object literals in expressions.
pub const MAX_OBJECT_DEPTH: usize = 128;

/// Names reserved for expressions handled at the AST level (control flow,
/// plus the `field`/`fields` source-reference grammar). These cannot be
/// registered as regular functions.
pub const RESERVED_CONTROL_FLOW_NAMES: &[&str] = &[
    "if", "multiIf", "coalesce", "ifNull", "nullIf", "and", "or", "field", "fields",
];
