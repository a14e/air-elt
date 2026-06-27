//! Binary-operator metadata for the precedence-climbing expression parser.
//!
//! The parser resolves every infix operator through this single
//! [`PrattOperator`] table instead of a function-per-precedence-level ladder.
//! Each operator carries its precedence, its associativity, and how it builds an
//! AST node, so the parser's climbing loop is *one* function. That keeps a deeply
//! nested expression to a shallow native call stack — the [depth
//! guard](crate::parser) trips and returns a clean error long before the stack
//! could overflow, which the per-level ladder (≈13 native frames per nesting
//! level) could not guarantee.

use crate::model::{ConditionalExpr, Expr};
use crate::token::Token;

/// How equal-precedence operators group.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Associativity {
    /// `a - b - c` parses as `(a - b) - c`.
    Left,
    /// `a ** b ** c` parses as `a ** (b ** c)`.
    Right,
    /// Chaining is a syntax error: `a < b < c` must be parenthesized.
    NonAssociative,
}

/// How a binary operator turns its two operands into an AST node.
enum NodeBuilder {
    /// Logical `||` — short-circuit, lowered to [`ConditionalExpr::Or`].
    Or,
    /// Logical `&&` — short-circuit, lowered to [`ConditionalExpr::And`].
    And,
    /// Membership `in`: `x in y` desugars to `contains(y, x)` — operands
    /// swapped so the container is the first argument (matching the
    /// `contains(container, element)` builtin shape).
    In,
    /// Every other operator desugars to a named function call (`add`, `equals`, …).
    Function(&'static str),
}

/// One infix operator's parsing metadata: how tightly it binds, how it groups,
/// and how it builds a node. Resolve it from a token with
/// [`PrattOperator::for_token`].
pub(crate) struct PrattOperator {
    builder: NodeBuilder,
    precedence: u8,
    associativity: Associativity,
}

impl PrattOperator {
    /// The infix operator a token introduces, or `None` if the token is not a
    /// binary operator. Precedence increases from `||` (loosest) to `**`
    /// (tightest); this ordering reproduces the original parse ladder exactly.
    pub(crate) fn for_token(token: &Token) -> Option<PrattOperator> {
        let (builder, precedence, associativity) = match token {
            Token::Or => (NodeBuilder::Or, 1, Associativity::Left),
            Token::And => (NodeBuilder::And, 2, Associativity::Left),
            Token::Pipe => (NodeBuilder::Function("bitOr"), 3, Associativity::Left),
            Token::Caret => (NodeBuilder::Function("bitXor"), 4, Associativity::Left),
            Token::Ampersand => (NodeBuilder::Function("bitAnd"), 5, Associativity::Left),
            Token::EqEq => (
                NodeBuilder::Function("equals"),
                6,
                Associativity::NonAssociative,
            ),
            Token::NotEq => (
                NodeBuilder::Function("notEquals"),
                6,
                Associativity::NonAssociative,
            ),
            Token::Lt => (
                NodeBuilder::Function("less"),
                6,
                Associativity::NonAssociative,
            ),
            Token::Gt => (
                NodeBuilder::Function("greater"),
                6,
                Associativity::NonAssociative,
            ),
            Token::LtEq => (
                NodeBuilder::Function("lessOrEquals"),
                6,
                Associativity::NonAssociative,
            ),
            Token::GtEq => (
                NodeBuilder::Function("greaterOrEquals"),
                6,
                Associativity::NonAssociative,
            ),
            Token::ShiftLeft => (
                NodeBuilder::Function("bitShiftLeft"),
                7,
                Associativity::Left,
            ),
            Token::ShiftRight => (
                NodeBuilder::Function("bitShiftRight"),
                7,
                Associativity::Left,
            ),
            Token::Plus => (NodeBuilder::Function("add"), 8, Associativity::Left),
            Token::Minus => (NodeBuilder::Function("subtract"), 8, Associativity::Left),
            Token::Star => (NodeBuilder::Function("multiply"), 9, Associativity::Left),
            Token::Slash => (NodeBuilder::Function("divide"), 9, Associativity::Left),
            Token::Percent => (NodeBuilder::Function("modulo"), 9, Associativity::Left),
            Token::Power => (NodeBuilder::Function("power"), 10, Associativity::Right),
            // Membership shares comparison precedence and is non-associative
            // (`a in b in c` must be parenthesised), matching `==` / `<`.
            Token::In => (NodeBuilder::In, 6, Associativity::NonAssociative),
            _ => return None,
        };
        Some(PrattOperator {
            builder,
            precedence,
            associativity,
        })
    }

    /// The `(left, right)` binding powers for precedence climbing. A
    /// left-associative (or non-associative) operator's right power sits just
    /// above its left power, so an equal-precedence operator re-binds to the
    /// *left*; a right-associative operator dips its right power below, so an
    /// equal operator re-binds to the *right*.
    pub(crate) fn binding_powers(&self) -> (u8, u8) {
        let base = self.precedence * 2;
        match self.associativity {
            Associativity::Left | Associativity::NonAssociative => (base, base + 1),
            Associativity::Right => (base + 1, base),
        }
    }

    pub(crate) fn associativity(&self) -> Associativity {
        self.associativity
    }

    /// Build the operator's AST node over its already-parsed operands.
    pub(crate) fn build(&self, left: Expr, right: Expr) -> Expr {
        match self.builder {
            NodeBuilder::Or => Expr::Conditional(ConditionalExpr::Or {
                left: Box::new(left),
                right: Box::new(right),
            }),
            NodeBuilder::And => Expr::Conditional(ConditionalExpr::And {
                left: Box::new(left),
                right: Box::new(right),
            }),
            NodeBuilder::In => Expr::FunctionCall {
                name: "contains".to_string(),
                args: vec![right, left],
            },
            NodeBuilder::Function(name) => Expr::FunctionCall {
                name: name.to_string(),
                args: vec![left, right],
            },
        }
    }
}
