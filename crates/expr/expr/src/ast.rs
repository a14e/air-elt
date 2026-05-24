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

    /// String with interpolation segments.
    Interpolation(Vec<InterpolationSegment>),

    /// Object literal: { "key" = expr, ... }
    Object(Vec<(String, Expr)>),
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
