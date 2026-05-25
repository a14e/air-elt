use crate::token::StringPart;

/// A complete expression program: zero or more variable bindings followed by a result expression.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub statements: Vec<Statement>,
    pub result: Expr,
}

/// A variable binding statement: `name = expr`
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub name: String,
    pub value: Expr,
}

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

impl From<Vec<StringPart>> for Expr {
    fn from(parts: Vec<StringPart>) -> Self {
        if parts.len() == 1 {
            if let StringPart::Literal(s) = &parts[0] {
                return Expr::Literal(LiteralValue::String(s.clone()));
            }
        }

        let segments = parts
            .into_iter()
            .map(|part| match part {
                StringPart::Literal(s) => InterpolationSegment::Text(s),
                StringPart::Expr(_source) => {
                    // Interpolation expressions are parsed in a second pass
                    InterpolationSegment::Text(String::new())
                }
            })
            .collect();

        Expr::Interpolation(segments)
    }
}
