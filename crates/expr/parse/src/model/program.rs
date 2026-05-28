use super::expression::Expr;

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
