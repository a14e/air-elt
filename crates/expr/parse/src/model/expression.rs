/// Expression AST node.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// Literal value.
    Literal(LiteralValue),

    /// Variable reference.
    Variable(String),

    /// Function call: name(arg1, arg2, ...)
    FunctionCall { name: String, args: Vec<Expr> },

    /// Control-flow conditional — evaluated lazily (short-circuit).
    Conditional(ConditionalExpr),

    /// String with interpolation segments.
    Interpolation(Vec<InterpolationSegment>),

    /// Object literal: { "key" = expr, ... }
    Object(Vec<(String, Expr)>),

    /// Source column reference: `field(<expr>)` or the backtick shorthand `` `name` ``.
    /// The inner expression must (after optimization) fold to a constant string
    /// column name. It carries an arbitrary inner `Expr` for now.
    Field(Box<Expr>),

    /// Whole-body / named-fields JSON object: `fields("*")` or `fields("a,b,c")`.
    Fields(FieldsSelector),
}

/// Selector for the `fields(...)` expression node.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldsSelector {
    /// `fields("*")` — the whole record body as a JSON object.
    All,
    /// `fields("a,b,c")` — a JSON object of the named fields.
    Named(Vec<String>),
}

/// Short-circuit conditional expressions evaluated lazily by the evaluator.
#[derive(Debug, Clone, PartialEq)]
pub enum ConditionalExpr {
    /// `if(condition, then_branch, else_branch)`
    If {
        condition: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Box<Expr>,
    },
    /// `multiIf(cond1, val1, cond2, val2, ..., default)`
    MultiIf {
        branches: Vec<(Expr, Expr)>,
        default: Box<Expr>,
    },
    /// `ifNull(value, alternative)` — returns value if non-null, else alternative.
    IfNull {
        value: Box<Expr>,
        alternative: Box<Expr>,
    },
    /// `nullIf(value, sentinel)` — returns null if value equals sentinel, else value.
    NullIf {
        value: Box<Expr>,
        sentinel: Box<Expr>,
    },
    /// `a && b` — short-circuit logical AND.
    And { left: Box<Expr>, right: Box<Expr> },
    /// `a || b` — short-circuit logical OR.
    Or { left: Box<Expr>, right: Box<Expr> },
}

/// Literal values in the expression language.
#[derive(Debug, Clone, PartialEq)]
pub enum LiteralValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
}

/// A segment within an interpolated string.
#[derive(Debug, Clone, PartialEq)]
pub enum InterpolationSegment {
    /// Literal text.
    Text(String),
    /// An embedded expression.
    Expression(Expr),
}
