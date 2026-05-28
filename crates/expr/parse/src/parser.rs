use air_elt_expr_types::limits::{
    MAX_AST_NODES, MAX_EXPR_DEPTH, MAX_EXPR_SOURCE_LEN, MAX_FUNCTION_ARGS, MAX_VARIABLES,
};

use crate::detect;
use crate::error::ExprError;
use crate::lexer::Lexer;
use crate::model::{ConditionalExpr, Expr, InterpolationSegment, LiteralValue, Program, Statement};
use crate::token::{SpannedToken, StringPart, Token};

/// Expression parser. Converts expression source strings into [`Program`] ASTs.
pub struct Parser;

impl Parser {
    /// Create a new parser instance.
    pub fn create() -> Self {
        Self
    }

    pub fn parse(&self, input: &str) -> Result<Program, ExprError> {
        if detect::is_expression(input) {
            return parse(input);
        }
        if detect::has_interpolation(input) {
            return parse_interpolation_template(input);
        }
        Ok(Program {
            statements: vec![],
            result: Expr::Literal(LiteralValue::String(input.to_owned())),
        })
    }

    /// Parse `input` as expression source directly, bypassing config-value detection.
    /// Use this only when you already know the string is an expression (e.g. when
    /// implementing a new evaluator pathway or in unit tests).
    pub fn parse_expression(&self, input: &str) -> Result<Program, ExprError> {
        parse(input)
    }

    pub fn parse_toml(&self, value: &toml::Value) -> Result<Program, ExprError> {
        let result = self.parse_toml_expr(value)?;
        Ok(Program {
            statements: vec![],
            result,
        })
    }

    fn parse_toml_expr(&self, value: &toml::Value) -> Result<Expr, ExprError> {
        match value {
            toml::Value::String(s) => Ok(self.parse(s)?.result),
            toml::Value::Integer(n) => Ok(Expr::Literal(LiteralValue::Int(*n))),
            toml::Value::Float(f) => Ok(Expr::Literal(LiteralValue::Float(*f))),
            toml::Value::Boolean(b) => Ok(Expr::Literal(LiteralValue::Bool(*b))),
            toml::Value::Table(t) => {
                let mut entries = Vec::with_capacity(t.len());
                for (key, val) in t {
                    entries.push((key.clone(), self.parse_toml_expr(val)?));
                }
                Ok(Expr::Object(entries))
            }
            toml::Value::Array(arr) => {
                let mut elements = Vec::with_capacity(arr.len());
                for val in arr {
                    elements.push(self.parse_toml_expr(val)?);
                }
                Ok(Expr::Literal(LiteralValue::String(
                    toml::Value::Array(arr.clone()).to_string(),
                )))
            }
            _ => Ok(Expr::Literal(LiteralValue::String(value.to_string()))),
        }
    }

    pub fn is_expr(&self, input: &str) -> bool {
        detect::is_expression(input) || detect::has_interpolation(input)
    }
}

fn parse_interpolation_template(input: &str) -> Result<Program, ExprError> {
    if input.len() > MAX_EXPR_SOURCE_LEN {
        return Err(ExprError::ExpressionTooLong {
            len: input.len(),
            max: MAX_EXPR_SOURCE_LEN,
        });
    }
    let mut lexer = Lexer::new(input);
    let tokens = lexer.tokenize_as_interpolation()?;
    let mut state = ParseState::new(tokens);
    state.parse_program()
}

/// Parse an expression source string into a [`Program`] AST.
///
/// Convenience wrapper around `Parser::create().parse(input)`.
pub fn parse(input: &str) -> Result<Program, ExprError> {
    if input.len() > MAX_EXPR_SOURCE_LEN {
        return Err(ExprError::ExpressionTooLong {
            len: input.len(),
            max: MAX_EXPR_SOURCE_LEN,
        });
    }

    let mut lexer = Lexer::new(input);
    let spanned_tokens = lexer.tokenize()?;
    let mut state = ParseState::new(spanned_tokens);
    state.parse_program()
}

struct ParseState {
    tokens: Vec<SpannedToken>,
    pos: usize,
    depth: usize,
    node_count: usize,
    variable_count: usize,
}

impl ParseState {
    fn new(tokens: Vec<SpannedToken>) -> Self {
        Self {
            tokens,
            pos: 0,
            depth: 0,
            node_count: 0,
            variable_count: 0,
        }
    }

    fn peek(&self) -> &Token {
        self.tokens
            .get(self.pos)
            .map(|st| &st.token)
            .unwrap_or(&Token::Eof)
    }

    fn peek_line(&self) -> u32 {
        self.tokens
            .get(self.pos)
            .map(|st| st.line)
            .unwrap_or(u32::MAX)
    }

    fn peek_ahead(&self, offset: usize) -> &Token {
        self.tokens
            .get(self.pos + offset)
            .map(|st| &st.token)
            .unwrap_or(&Token::Eof)
    }

    fn advance(&mut self) -> Token {
        let token = self
            .tokens
            .get(self.pos)
            .map(|st| st.token.clone())
            .unwrap_or(Token::Eof);
        self.pos += 1;
        token
    }

    fn current_line(&self) -> u32 {
        if self.pos > 0 {
            self.tokens.get(self.pos - 1).map(|st| st.line).unwrap_or(1)
        } else {
            1
        }
    }

    fn expect(&mut self, expected: &Token) -> Result<(), ExprError> {
        let actual = self.peek().clone();
        if std::mem::discriminant(&actual) == std::mem::discriminant(expected) {
            self.advance();
            Ok(())
        } else {
            Err(ExprError::Parse {
                position: self.pos,
                message: format!("expected {expected:?}, got {actual:?}"),
            })
        }
    }

    fn increment_depth(&mut self) -> Result<(), ExprError> {
        self.depth += 1;
        if self.depth > MAX_EXPR_DEPTH {
            return Err(ExprError::NestingTooDeep {
                max: MAX_EXPR_DEPTH,
            });
        }
        Ok(())
    }

    fn decrement_depth(&mut self) {
        self.depth -= 1;
    }

    fn count_node(&mut self) -> Result<(), ExprError> {
        self.node_count += 1;
        if self.node_count > MAX_AST_NODES {
            return Err(ExprError::TooManyNodes {
                count: self.node_count,
                max: MAX_AST_NODES,
            });
        }
        Ok(())
    }

    fn parse_program(&mut self) -> Result<Program, ExprError> {
        let mut statements = Vec::new();

        while self.is_statement_start() {
            let statement = self.parse_statement()?;
            statements.push(statement);
        }

        let result = self.parse_expr()?;

        if *self.peek() != Token::Eof {
            return Err(ExprError::Parse {
                position: self.pos,
                message: format!("unexpected token after expression: {:?}", self.peek()),
            });
        }

        Ok(Program { statements, result })
    }

    fn is_statement_start(&self) -> bool {
        matches!(self.peek(), Token::Ident(_)) && *self.peek_ahead(1) == Token::Eq
    }

    fn parse_statement(&mut self) -> Result<Statement, ExprError> {
        let name = match self.advance() {
            Token::Ident(name) => name,
            other => {
                return Err(ExprError::Parse {
                    position: self.pos,
                    message: format!("expected identifier, got {other:?}"),
                });
            }
        };

        self.expect(&Token::Eq)?;

        self.variable_count += 1;
        if self.variable_count > MAX_VARIABLES {
            return Err(ExprError::TooManyVariables {
                count: self.variable_count,
                max: MAX_VARIABLES,
            });
        }

        let value = self.parse_expr()?;

        // After parsing the RHS, check for statement termination:
        // - `;` is an explicit separator (consume it)
        // - EOF or a token on a different line is implicit separation
        // - A token on the same line that is not `;` is an error
        let rhs_line = self.current_line();
        if *self.peek() == Token::Semicolon {
            self.advance();
        } else if *self.peek() != Token::Eof {
            let next_line = self.peek_line();
            if next_line == rhs_line {
                return Err(ExprError::Parse {
                    position: self.pos,
                    message: "expected newline or ';' after statement".to_string(),
                });
            }
        }

        Ok(Statement { name, value })
    }

    fn parse_expr(&mut self) -> Result<Expr, ExprError> {
        self.increment_depth()?;
        let result = self.parse_or_expr();
        self.decrement_depth();
        result
    }

    fn parse_or_expr(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_and_expr()?;

        while *self.peek() == Token::Or {
            self.advance();
            let right = self.parse_and_expr()?;
            self.count_node()?;
            left = Expr::Conditional(ConditionalExpr::Or {
                left: Box::new(left),
                right: Box::new(right),
            });
        }

        Ok(left)
    }

    fn parse_and_expr(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_bit_or_expr()?;

        while *self.peek() == Token::And {
            self.advance();
            let right = self.parse_bit_or_expr()?;
            self.count_node()?;
            left = Expr::Conditional(ConditionalExpr::And {
                left: Box::new(left),
                right: Box::new(right),
            });
        }

        Ok(left)
    }

    fn parse_bit_or_expr(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_bit_xor_expr()?;

        while *self.peek() == Token::Pipe {
            self.advance();
            let right = self.parse_bit_xor_expr()?;
            self.count_node()?;
            left = Expr::FunctionCall {
                name: "bitOr".to_string(),
                args: vec![left, right],
            };
        }

        Ok(left)
    }

    fn parse_bit_xor_expr(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_bit_and_expr()?;

        while *self.peek() == Token::Caret {
            self.advance();
            let right = self.parse_bit_and_expr()?;
            self.count_node()?;
            left = Expr::FunctionCall {
                name: "bitXor".to_string(),
                args: vec![left, right],
            };
        }

        Ok(left)
    }

    fn parse_bit_and_expr(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_cmp_expr()?;

        while *self.peek() == Token::Ampersand {
            self.advance();
            let right = self.parse_cmp_expr()?;
            self.count_node()?;
            left = Expr::FunctionCall {
                name: "bitAnd".to_string(),
                args: vec![left, right],
            };
        }

        Ok(left)
    }

    fn parse_cmp_expr(&mut self) -> Result<Expr, ExprError> {
        let left = self.parse_shift_expr()?;

        let operator_name = match self.peek() {
            Token::EqEq => "equals",
            Token::NotEq => "notEquals",
            Token::Lt => "less",
            Token::Gt => "greater",
            Token::LtEq => "lessOrEquals",
            Token::GtEq => "greaterOrEquals",
            _ => return Ok(left),
        };

        self.advance();
        let right = self.parse_shift_expr()?;
        self.count_node()?;

        Ok(Expr::FunctionCall {
            name: operator_name.to_string(),
            args: vec![left, right],
        })
    }

    fn parse_shift_expr(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_add_expr()?;

        loop {
            let operator_name = match self.peek() {
                Token::ShiftLeft => "bitShiftLeft",
                Token::ShiftRight => "bitShiftRight",
                _ => break,
            };

            self.advance();
            let right = self.parse_add_expr()?;
            self.count_node()?;
            left = Expr::FunctionCall {
                name: operator_name.to_string(),
                args: vec![left, right],
            };
        }

        Ok(left)
    }

    fn parse_add_expr(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_mul_expr()?;

        loop {
            let operator_name = match self.peek() {
                Token::Plus => "add",
                Token::Minus => "subtract",
                _ => break,
            };

            self.advance();
            let right = self.parse_mul_expr()?;
            self.count_node()?;
            left = Expr::FunctionCall {
                name: operator_name.to_string(),
                args: vec![left, right],
            };
        }

        Ok(left)
    }

    fn parse_mul_expr(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_power_expr()?;

        loop {
            let operator_name = match self.peek() {
                Token::Star => "multiply",
                Token::Slash => "divide",
                Token::Percent => "modulo",
                _ => break,
            };

            self.advance();
            let right = self.parse_power_expr()?;
            self.count_node()?;
            left = Expr::FunctionCall {
                name: operator_name.to_string(),
                args: vec![left, right],
            };
        }

        Ok(left)
    }

    fn parse_power_expr(&mut self) -> Result<Expr, ExprError> {
        let base = self.parse_unary_expr()?;

        if *self.peek() == Token::Power {
            self.advance();
            let exponent = self.parse_power_expr()?;
            self.count_node()?;
            return Ok(Expr::FunctionCall {
                name: "power".to_string(),
                args: vec![base, exponent],
            });
        }

        Ok(base)
    }

    fn parse_unary_expr(&mut self) -> Result<Expr, ExprError> {
        match self.peek() {
            Token::Minus => {
                self.advance();
                let operand = self.parse_unary_expr()?;
                self.count_node()?;
                Ok(Expr::FunctionCall {
                    name: "negate".to_string(),
                    args: vec![operand],
                })
            }
            Token::Not => {
                self.advance();
                let operand = self.parse_unary_expr()?;
                self.count_node()?;
                Ok(Expr::FunctionCall {
                    name: "not".to_string(),
                    args: vec![operand],
                })
            }
            Token::Tilde => {
                self.advance();
                let operand = self.parse_unary_expr()?;
                self.count_node()?;
                Ok(Expr::FunctionCall {
                    name: "bitNot".to_string(),
                    args: vec![operand],
                })
            }
            _ => self.parse_call_expr(),
        }
    }

    fn parse_call_expr(&mut self) -> Result<Expr, ExprError> {
        if let Token::Ident(name) = self.peek().clone() {
            if *self.peek_ahead(1) == Token::LParen {
                match name.as_str() {
                    "if" => return self.parse_if_conditional(),
                    "multiIf" => return self.parse_multi_if_conditional(),
                    "coalesce" => return self.parse_coalesce_conditional(),
                    "ifNull" => return self.parse_if_null_conditional(),
                    "nullIf" => return self.parse_null_if_conditional(),
                    _ => {}
                }

                self.advance(); // consume ident
                self.advance(); // consume '('

                let args = self.parse_args()?;

                if args.len() > MAX_FUNCTION_ARGS {
                    return Err(ExprError::Parse {
                        position: self.pos,
                        message: format!(
                            "too many function arguments: {} (max {MAX_FUNCTION_ARGS})",
                            args.len()
                        ),
                    });
                }

                self.expect(&Token::RParen)?;
                self.count_node()?;

                return Ok(Expr::FunctionCall { name, args });
            }
        }

        self.parse_primary()
    }

    fn parse_if_conditional(&mut self) -> Result<Expr, ExprError> {
        self.advance(); // consume "if"
        self.advance(); // consume "("
        let condition = self.parse_expr()?;
        self.expect(&Token::Comma)?;
        let then_branch = self.parse_expr()?;
        self.expect(&Token::Comma)?;
        let else_branch = self.parse_expr()?;
        self.expect(&Token::RParen)?;
        self.count_node()?;
        Ok(Expr::Conditional(ConditionalExpr::If {
            condition: Box::new(condition),
            then_branch: Box::new(then_branch),
            else_branch: Box::new(else_branch),
        }))
    }

    fn parse_multi_if_conditional(&mut self) -> Result<Expr, ExprError> {
        self.advance(); // consume "multiIf"
        self.advance(); // consume "("
        let mut args = vec![self.parse_expr()?];
        while *self.peek() == Token::Comma {
            self.advance();
            if *self.peek() == Token::RParen {
                break;
            }
            args.push(self.parse_expr()?);
        }
        self.expect(&Token::RParen)?;

        if args.len() < 3 || args.len() % 2 == 0 {
            return Err(ExprError::Parse {
                position: self.pos,
                message: "multiIf requires odd number of arguments (cond1, val1, ..., default)"
                    .to_string(),
            });
        }

        let default = args.pop().expect("checked length above");
        let mut branches = Vec::with_capacity(args.len() / 2);
        let mut iter = args.into_iter();
        while let Some(condition) = iter.next() {
            let value = iter.next().expect("checked odd length");
            branches.push((condition, value));
        }

        self.count_node()?;
        Ok(Expr::Conditional(ConditionalExpr::MultiIf {
            branches,
            default: Box::new(default),
        }))
    }

    fn parse_coalesce_conditional(&mut self) -> Result<Expr, ExprError> {
        self.advance(); // consume "coalesce"
        self.advance(); // consume "("
        let mut args = vec![self.parse_expr()?];
        while *self.peek() == Token::Comma {
            self.advance();
            if *self.peek() == Token::RParen {
                break;
            }
            args.push(self.parse_expr()?);
        }
        self.expect(&Token::RParen)?;

        if args.is_empty() {
            return Err(ExprError::Parse {
                position: self.pos,
                message: "coalesce requires at least one argument".to_string(),
            });
        }

        // Desugar: coalesce(a, b, c) -> IfNull(a, IfNull(b, c))
        let mut result = args.pop().expect("checked non-empty");
        while let Some(arg) = args.pop() {
            self.count_node()?;
            result = Expr::Conditional(ConditionalExpr::IfNull {
                value: Box::new(arg),
                alternative: Box::new(result),
            });
        }
        Ok(result)
    }

    fn parse_if_null_conditional(&mut self) -> Result<Expr, ExprError> {
        self.advance(); // consume "ifNull"
        self.advance(); // consume "("
        let value = self.parse_expr()?;
        self.expect(&Token::Comma)?;
        let alternative = self.parse_expr()?;
        self.expect(&Token::RParen)?;
        self.count_node()?;
        Ok(Expr::Conditional(ConditionalExpr::IfNull {
            value: Box::new(value),
            alternative: Box::new(alternative),
        }))
    }

    fn parse_null_if_conditional(&mut self) -> Result<Expr, ExprError> {
        self.advance(); // consume "nullIf"
        self.advance(); // consume "("
        let value = self.parse_expr()?;
        self.expect(&Token::Comma)?;
        let sentinel = self.parse_expr()?;
        self.expect(&Token::RParen)?;
        self.count_node()?;
        Ok(Expr::Conditional(ConditionalExpr::NullIf {
            value: Box::new(value),
            sentinel: Box::new(sentinel),
        }))
    }

    fn parse_args(&mut self) -> Result<Vec<Expr>, ExprError> {
        if *self.peek() == Token::RParen {
            return Ok(Vec::new());
        }

        let mut args = Vec::new();
        args.push(self.parse_expr()?);

        while *self.peek() == Token::Comma {
            self.advance();
            if *self.peek() == Token::RParen {
                break;
            }
            args.push(self.parse_expr()?);
        }

        Ok(args)
    }

    fn parse_primary(&mut self) -> Result<Expr, ExprError> {
        let token = self.peek().clone();
        match token {
            Token::IntLit(value) => {
                self.advance();
                self.count_node()?;
                Ok(Expr::Literal(LiteralValue::Int(value)))
            }
            Token::FloatLit(value) => {
                self.advance();
                self.count_node()?;
                Ok(Expr::Literal(LiteralValue::Float(value)))
            }
            Token::StringLit(parts) => {
                self.advance();
                self.count_node()?;
                self.build_string_expr(parts)
            }
            Token::RawStringLit(value) => {
                self.advance();
                self.count_node()?;
                Ok(Expr::Literal(LiteralValue::String(value)))
            }
            Token::BoolLit(value) => {
                self.advance();
                self.count_node()?;
                Ok(Expr::Literal(LiteralValue::Bool(value)))
            }
            Token::NullLit => {
                self.advance();
                self.count_node()?;
                Ok(Expr::Literal(LiteralValue::Null))
            }
            Token::Ident(name) => {
                self.advance();
                self.count_node()?;
                Ok(Expr::Variable(name))
            }
            Token::LParen => {
                self.advance();
                let inner = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                Ok(inner)
            }
            Token::LBrace => self.parse_object(),
            _ => Err(ExprError::Parse {
                position: self.pos,
                message: format!("unexpected token: {token:?}"),
            }),
        }
    }

    fn parse_object(&mut self) -> Result<Expr, ExprError> {
        self.advance(); // consume '{'
        self.count_node()?;

        if *self.peek() == Token::RBrace {
            self.advance();
            return Ok(Expr::Object(Vec::new()));
        }

        let mut entries = Vec::new();
        entries.push(self.parse_object_entry()?);

        while *self.peek() == Token::Comma {
            self.advance();
            if *self.peek() == Token::RBrace {
                break;
            }
            entries.push(self.parse_object_entry()?);
        }

        self.expect(&Token::RBrace)?;
        Ok(Expr::Object(entries))
    }

    fn parse_object_entry(&mut self) -> Result<(String, Expr), ExprError> {
        let key = match self.peek().clone() {
            Token::StringLit(parts) => {
                self.advance();
                extract_plain_string(&parts)?
            }
            Token::RawStringLit(value) => {
                self.advance();
                value
            }
            other => {
                return Err(ExprError::Parse {
                    position: self.pos,
                    message: format!("expected string key in object literal, got {other:?}"),
                });
            }
        };

        self.expect(&Token::Eq)?;
        let value = self.parse_expr()?;

        Ok((key, value))
    }

    fn build_string_expr(&mut self, parts: Vec<StringPart>) -> Result<Expr, ExprError> {
        let has_interpolation = parts.iter().any(|p| matches!(p, StringPart::Expr(_)));

        if !has_interpolation {
            let text = parts
                .into_iter()
                .map(|p| match p {
                    StringPart::Literal(s) => s,
                    StringPart::Expr(_) => String::new(),
                })
                .collect::<String>();
            return Ok(Expr::Literal(LiteralValue::String(text)));
        }

        let mut segments = Vec::new();

        for part in parts {
            match part {
                StringPart::Literal(text) => {
                    if !text.is_empty() {
                        segments.push(InterpolationSegment::Text(text));
                    }
                }
                StringPart::Expr(source) => {
                    let inner_expr = parse(&source)?;
                    let wrapped = Expr::FunctionCall {
                        name: "toString".to_string(),
                        args: vec![inner_expr.result],
                    };
                    segments.push(InterpolationSegment::Expression(wrapped));
                }
            }
        }

        Ok(Expr::Interpolation(segments))
    }
}

fn extract_plain_string(parts: &[StringPart]) -> Result<String, ExprError> {
    let mut result = String::new();
    for part in parts {
        match part {
            StringPart::Literal(s) => result.push_str(s),
            StringPart::Expr(_) => {
                return Err(ExprError::Parse {
                    position: 0,
                    message: "interpolation not allowed in object keys".to_string(),
                });
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::approx_constant)]
mod tests {
    use super::*;

    #[test]
    fn parse_integer_literal() {
        let program = parse("42").unwrap();
        assert_eq!(program.statements.len(), 0);
        assert_eq!(program.result, Expr::Literal(LiteralValue::Int(42)));
    }

    #[test]
    fn parse_negative_integer() {
        let program = parse("-7").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "negate".to_string(),
                args: vec![Expr::Literal(LiteralValue::Int(7))],
            }
        );
    }

    #[test]
    fn parse_float_literal() {
        let program = parse("3.14").unwrap();
        assert_eq!(program.result, Expr::Literal(LiteralValue::Float(3.14)));
    }

    #[test]
    fn parse_string_literal() {
        let program = parse("\"hello\"").unwrap();
        assert_eq!(
            program.result,
            Expr::Literal(LiteralValue::String("hello".to_string()))
        );
    }

    #[test]
    fn parse_raw_string_literal() {
        let program = parse("'raw {string}'").unwrap();
        assert_eq!(
            program.result,
            Expr::Literal(LiteralValue::String("raw {string}".to_string()))
        );
    }

    #[test]
    fn parse_bool_true() {
        let program = parse("true").unwrap();
        assert_eq!(program.result, Expr::Literal(LiteralValue::Bool(true)));
    }

    #[test]
    fn parse_bool_false() {
        let program = parse("false").unwrap();
        assert_eq!(program.result, Expr::Literal(LiteralValue::Bool(false)));
    }

    #[test]
    fn parse_null_literal() {
        let program = parse("null").unwrap();
        assert_eq!(program.result, Expr::Literal(LiteralValue::Null));
    }

    #[test]
    fn parse_function_call_zero_args() {
        let program = parse("now()").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "now".to_string(),
                args: vec![],
            }
        );
    }

    #[test]
    fn parse_function_call_one_arg() {
        let program = parse("env(\"KEY\")").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "env".to_string(),
                args: vec![Expr::Literal(LiteralValue::String("KEY".to_string()))],
            }
        );
    }

    #[test]
    fn parse_if_conditional() {
        let program = parse("if(true, 1, 2)").unwrap();
        assert_eq!(
            program.result,
            Expr::Conditional(ConditionalExpr::If {
                condition: Box::new(Expr::Literal(LiteralValue::Bool(true))),
                then_branch: Box::new(Expr::Literal(LiteralValue::Int(1))),
                else_branch: Box::new(Expr::Literal(LiteralValue::Int(2))),
            })
        );
    }

    #[test]
    fn parse_function_call_trailing_comma() {
        let program = parse("f(1, 2,)").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "f".to_string(),
                args: vec![
                    Expr::Literal(LiteralValue::Int(1)),
                    Expr::Literal(LiteralValue::Int(2)),
                ],
            }
        );
    }

    #[test]
    fn parse_nested_function_calls() {
        let program = parse("outer(inner(1))").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "outer".to_string(),
                args: vec![Expr::FunctionCall {
                    name: "inner".to_string(),
                    args: vec![Expr::Literal(LiteralValue::Int(1))],
                }],
            }
        );
    }

    #[test]
    fn parse_addition() {
        let program = parse("1 + 2").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "add".to_string(),
                args: vec![
                    Expr::Literal(LiteralValue::Int(1)),
                    Expr::Literal(LiteralValue::Int(2)),
                ],
            }
        );
    }

    #[test]
    fn parse_arithmetic_precedence() {
        // 1 + 2 * 3 should parse as 1 + (2 * 3)
        let program = parse("1 + 2 * 3").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "add".to_string(),
                args: vec![
                    Expr::Literal(LiteralValue::Int(1)),
                    Expr::FunctionCall {
                        name: "multiply".to_string(),
                        args: vec![
                            Expr::Literal(LiteralValue::Int(2)),
                            Expr::Literal(LiteralValue::Int(3)),
                        ],
                    },
                ],
            }
        );
    }

    #[test]
    fn parse_left_associative_subtraction() {
        // 5 - 3 - 1 should parse as (5 - 3) - 1
        let program = parse("5 - 3 - 1").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "subtract".to_string(),
                args: vec![
                    Expr::FunctionCall {
                        name: "subtract".to_string(),
                        args: vec![
                            Expr::Literal(LiteralValue::Int(5)),
                            Expr::Literal(LiteralValue::Int(3)),
                        ],
                    },
                    Expr::Literal(LiteralValue::Int(1)),
                ],
            }
        );
    }

    #[test]
    fn parse_division_and_modulo() {
        let program = parse("10 / 3 % 2").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "modulo".to_string(),
                args: vec![
                    Expr::FunctionCall {
                        name: "divide".to_string(),
                        args: vec![
                            Expr::Literal(LiteralValue::Int(10)),
                            Expr::Literal(LiteralValue::Int(3)),
                        ],
                    },
                    Expr::Literal(LiteralValue::Int(2)),
                ],
            }
        );
    }

    #[test]
    fn parse_comparison_equals() {
        let program = parse("a == 10").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "equals".to_string(),
                args: vec![
                    Expr::Variable("a".to_string()),
                    Expr::Literal(LiteralValue::Int(10)),
                ],
            }
        );
    }

    #[test]
    fn parse_comparison_not_equals() {
        let program = parse("x != null").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "notEquals".to_string(),
                args: vec![
                    Expr::Variable("x".to_string()),
                    Expr::Literal(LiteralValue::Null),
                ],
            }
        );
    }

    #[test]
    fn parse_comparison_less_greater() {
        let program = parse("a < b").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "less".to_string(),
                args: vec![
                    Expr::Variable("a".to_string()),
                    Expr::Variable("b".to_string()),
                ],
            }
        );

        let program = parse("a > b").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "greater".to_string(),
                args: vec![
                    Expr::Variable("a".to_string()),
                    Expr::Variable("b".to_string()),
                ],
            }
        );
    }

    #[test]
    fn parse_comparison_less_or_equals() {
        let program = parse("x <= 100").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "lessOrEquals".to_string(),
                args: vec![
                    Expr::Variable("x".to_string()),
                    Expr::Literal(LiteralValue::Int(100)),
                ],
            }
        );
    }

    #[test]
    fn parse_comparison_greater_or_equals() {
        let program = parse("x >= 0").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "greaterOrEquals".to_string(),
                args: vec![
                    Expr::Variable("x".to_string()),
                    Expr::Literal(LiteralValue::Int(0)),
                ],
            }
        );
    }

    #[test]
    fn parse_logical_and() {
        let program = parse("a && b").unwrap();
        assert_eq!(
            program.result,
            Expr::Conditional(ConditionalExpr::And {
                left: Box::new(Expr::Variable("a".to_string())),
                right: Box::new(Expr::Variable("b".to_string())),
            })
        );
    }

    #[test]
    fn parse_logical_or() {
        let program = parse("a || b").unwrap();
        assert_eq!(
            program.result,
            Expr::Conditional(ConditionalExpr::Or {
                left: Box::new(Expr::Variable("a".to_string())),
                right: Box::new(Expr::Variable("b".to_string())),
            })
        );
    }

    #[test]
    fn parse_logical_not() {
        let program = parse("!x").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "not".to_string(),
                args: vec![Expr::Variable("x".to_string())],
            }
        );
    }

    #[test]
    fn parse_combined_logical_precedence() {
        // a || b && c should parse as a || (b && c)
        let program = parse("a || b && c").unwrap();
        assert_eq!(
            program.result,
            Expr::Conditional(ConditionalExpr::Or {
                left: Box::new(Expr::Variable("a".to_string())),
                right: Box::new(Expr::Conditional(ConditionalExpr::And {
                    left: Box::new(Expr::Variable("b".to_string())),
                    right: Box::new(Expr::Variable("c".to_string())),
                })),
            })
        );
    }

    #[test]
    fn parse_parenthesized_expression() {
        // (1 + 2) * 3
        let program = parse("(1 + 2) * 3").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "multiply".to_string(),
                args: vec![
                    Expr::FunctionCall {
                        name: "add".to_string(),
                        args: vec![
                            Expr::Literal(LiteralValue::Int(1)),
                            Expr::Literal(LiteralValue::Int(2)),
                        ],
                    },
                    Expr::Literal(LiteralValue::Int(3)),
                ],
            }
        );
    }

    #[test]
    fn parse_variable_binding_with_semicolon() {
        let program = parse("x = 10; x + 1").unwrap();
        assert_eq!(program.statements.len(), 1);
        assert_eq!(
            program.statements[0],
            Statement {
                name: "x".to_string(),
                value: Expr::Literal(LiteralValue::Int(10)),
            }
        );
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "add".to_string(),
                args: vec![
                    Expr::Variable("x".to_string()),
                    Expr::Literal(LiteralValue::Int(1)),
                ],
            }
        );
    }

    #[test]
    fn parse_multiple_variable_bindings() {
        let program = parse("x = 1; y = 2; x + y").unwrap();
        assert_eq!(program.statements.len(), 2);
        assert_eq!(program.statements[0].name, "x");
        assert_eq!(program.statements[1].name, "y");
    }

    #[test]
    fn parse_variable_binding_with_newline() {
        let program = parse("x = 10\nx + 1").unwrap();
        assert_eq!(program.statements.len(), 1);
        assert_eq!(program.statements[0].name, "x");
    }

    #[test]
    fn parse_string_interpolation() {
        let program = parse("\"hello {name}\"").unwrap();
        match &program.result {
            Expr::Interpolation(segments) => {
                assert_eq!(segments.len(), 2);
                assert_eq!(
                    segments[0],
                    InterpolationSegment::Text("hello ".to_string())
                );
                match &segments[1] {
                    InterpolationSegment::Expression(expr) => {
                        assert_eq!(
                            *expr,
                            Expr::FunctionCall {
                                name: "toString".to_string(),
                                args: vec![Expr::Variable("name".to_string())],
                            }
                        );
                    }
                    other => panic!("expected Expression segment, got {other:?}"),
                }
            }
            other => panic!("expected Interpolation, got {other:?}"),
        }
    }

    #[test]
    fn parse_string_interpolation_with_expression() {
        let program = parse("\"sum: {1 + 2}\"").unwrap();
        match &program.result {
            Expr::Interpolation(segments) => {
                assert_eq!(segments.len(), 2);
                assert_eq!(segments[0], InterpolationSegment::Text("sum: ".to_string()));
                match &segments[1] {
                    InterpolationSegment::Expression(expr) => {
                        assert_eq!(
                            *expr,
                            Expr::FunctionCall {
                                name: "toString".to_string(),
                                args: vec![Expr::FunctionCall {
                                    name: "add".to_string(),
                                    args: vec![
                                        Expr::Literal(LiteralValue::Int(1)),
                                        Expr::Literal(LiteralValue::Int(2)),
                                    ],
                                }],
                            }
                        );
                    }
                    other => panic!("expected Expression segment, got {other:?}"),
                }
            }
            other => panic!("expected Interpolation, got {other:?}"),
        }
    }

    #[test]
    fn parse_object_literal_empty() {
        let program = parse("{}").unwrap();
        assert_eq!(program.result, Expr::Object(Vec::new()));
    }

    #[test]
    fn parse_object_literal_single_field() {
        let program = parse("{\"name\" = \"Alice\"}").unwrap();
        assert_eq!(
            program.result,
            Expr::Object(vec![(
                "name".to_string(),
                Expr::Literal(LiteralValue::String("Alice".to_string()))
            )])
        );
    }

    #[test]
    fn parse_object_literal_multiple_fields() {
        let program = parse("{\"x\" = 1, \"y\" = 2}").unwrap();
        assert_eq!(
            program.result,
            Expr::Object(vec![
                ("x".to_string(), Expr::Literal(LiteralValue::Int(1))),
                ("y".to_string(), Expr::Literal(LiteralValue::Int(2))),
            ])
        );
    }

    #[test]
    fn parse_object_literal_trailing_comma() {
        let program = parse("{\"a\" = 1, \"b\" = 2,}").unwrap();
        assert_eq!(
            program.result,
            Expr::Object(vec![
                ("a".to_string(), Expr::Literal(LiteralValue::Int(1))),
                ("b".to_string(), Expr::Literal(LiteralValue::Int(2))),
            ])
        );
    }

    #[test]
    fn parse_object_with_raw_string_keys() {
        let program = parse("{'key' = 42}").unwrap();
        assert_eq!(
            program.result,
            Expr::Object(vec![(
                "key".to_string(),
                Expr::Literal(LiteralValue::Int(42))
            )])
        );
    }

    #[test]
    fn parse_variable_reference() {
        let program = parse("my_var").unwrap();
        assert_eq!(program.result, Expr::Variable("my_var".to_string()));
    }

    #[test]
    fn depth_limit_produces_error() {
        // Build a deeply nested expression: (((((...42...)))))
        let nesting = MAX_EXPR_DEPTH + 2;
        let mut input = String::new();
        for _ in 0..nesting {
            input.push('(');
        }
        input.push_str("42");
        for _ in 0..nesting {
            input.push(')');
        }

        let result = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || parse(&input))
            .expect("failed to spawn thread")
            .join()
            .expect("thread panicked");

        assert!(result.is_err());
        match result.unwrap_err() {
            ExprError::NestingTooDeep { max } => {
                assert_eq!(max, MAX_EXPR_DEPTH);
            }
            other => panic!("expected NestingTooDeep, got {other:?}"),
        }
    }

    #[test]
    fn node_limit_produces_error() {
        // Build an expression with many nodes: 1+1+1+1+...
        let mut input = String::from("1");
        for _ in 0..10_001 {
            input.push_str(" + 1");
        }

        let result = parse(&input);
        assert!(result.is_err());
        match result.unwrap_err() {
            ExprError::TooManyNodes { count: _, max } => {
                assert_eq!(max, MAX_AST_NODES);
            }
            other => panic!("expected TooManyNodes, got {other:?}"),
        }
    }

    #[test]
    fn function_arg_limit_produces_error() {
        let mut input = String::from("f(1");
        for _ in 0..MAX_FUNCTION_ARGS {
            input.push_str(", 1");
        }
        input.push(')');

        let result = parse(&input);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("too many function arguments"));
    }

    #[test]
    fn variable_limit_produces_error() {
        let mut input = String::new();
        for i in 0..=MAX_VARIABLES {
            input.push_str(&format!("v{i} = 1; "));
        }
        input.push('1');

        let result = parse(&input);
        assert!(result.is_err());
        match result.unwrap_err() {
            ExprError::TooManyVariables { count: _, max } => {
                assert_eq!(max, MAX_VARIABLES);
            }
            other => panic!("expected TooManyVariables, got {other:?}"),
        }
    }

    #[test]
    fn expression_too_long_produces_error() {
        let input = "x".repeat(MAX_EXPR_SOURCE_LEN + 1);
        let result = parse(&input);
        assert!(result.is_err());
        match result.unwrap_err() {
            ExprError::ExpressionTooLong { len, max } => {
                assert_eq!(len, MAX_EXPR_SOURCE_LEN + 1);
                assert_eq!(max, MAX_EXPR_SOURCE_LEN);
            }
            other => panic!("expected ExpressionTooLong, got {other:?}"),
        }
    }

    #[test]
    fn parse_complex_expression() {
        let program = parse("x = env(\"MODE\"); if(x == \"prod\", 100, 10)").unwrap();
        assert_eq!(program.statements.len(), 1);
        assert_eq!(program.statements[0].name, "x");
        assert_eq!(
            program.result,
            Expr::Conditional(ConditionalExpr::If {
                condition: Box::new(Expr::FunctionCall {
                    name: "equals".to_string(),
                    args: vec![
                        Expr::Variable("x".to_string()),
                        Expr::Literal(LiteralValue::String("prod".to_string())),
                    ],
                }),
                then_branch: Box::new(Expr::Literal(LiteralValue::Int(100))),
                else_branch: Box::new(Expr::Literal(LiteralValue::Int(10))),
            })
        );
    }

    #[test]
    fn parse_unary_double_negation() {
        let program = parse("--x").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "negate".to_string(),
                args: vec![Expr::FunctionCall {
                    name: "negate".to_string(),
                    args: vec![Expr::Variable("x".to_string())],
                }],
            }
        );
    }

    #[test]
    fn parse_not_equals_precedence() {
        // !a == b should parse as (!a) == b
        let program = parse("!a == b").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "equals".to_string(),
                args: vec![
                    Expr::FunctionCall {
                        name: "not".to_string(),
                        args: vec![Expr::Variable("a".to_string())],
                    },
                    Expr::Variable("b".to_string()),
                ],
            }
        );
    }

    // --- Bitwise operator tests ---

    #[test]
    fn parse_bitwise_and() {
        let program = parse("0xFF & 0x0F").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "bitAnd".to_string(),
                args: vec![
                    Expr::Literal(LiteralValue::Int(255)),
                    Expr::Literal(LiteralValue::Int(15)),
                ],
            }
        );
    }

    #[test]
    fn parse_bitwise_or() {
        let program = parse("a | b").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "bitOr".to_string(),
                args: vec![
                    Expr::Variable("a".to_string()),
                    Expr::Variable("b".to_string()),
                ],
            }
        );
    }

    #[test]
    fn parse_bitwise_xor() {
        let program = parse("a ^ b").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "bitXor".to_string(),
                args: vec![
                    Expr::Variable("a".to_string()),
                    Expr::Variable("b".to_string()),
                ],
            }
        );
    }

    #[test]
    fn parse_bitwise_not() {
        let program = parse("~0").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "bitNot".to_string(),
                args: vec![Expr::Literal(LiteralValue::Int(0))],
            }
        );
    }

    #[test]
    fn parse_shift_left() {
        let program = parse("1 << 8").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "bitShiftLeft".to_string(),
                args: vec![
                    Expr::Literal(LiteralValue::Int(1)),
                    Expr::Literal(LiteralValue::Int(8)),
                ],
            }
        );
    }

    #[test]
    fn parse_shift_right() {
        let program = parse("256 >> 4").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "bitShiftRight".to_string(),
                args: vec![
                    Expr::Literal(LiteralValue::Int(256)),
                    Expr::Literal(LiteralValue::Int(4)),
                ],
            }
        );
    }

    #[test]
    fn parse_shift_precedence_vs_addition() {
        // 1 + 2 << 3 should parse as (1 + 2) << 3 (shift is lower than add)
        let program = parse("1 + 2 << 3").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "bitShiftLeft".to_string(),
                args: vec![
                    Expr::FunctionCall {
                        name: "add".to_string(),
                        args: vec![
                            Expr::Literal(LiteralValue::Int(1)),
                            Expr::Literal(LiteralValue::Int(2)),
                        ],
                    },
                    Expr::Literal(LiteralValue::Int(3)),
                ],
            }
        );
    }

    #[test]
    fn parse_bitwise_precedence_chain() {
        // a & b | c should parse as (a & b) | c
        let program = parse("a & b | c").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "bitOr".to_string(),
                args: vec![
                    Expr::FunctionCall {
                        name: "bitAnd".to_string(),
                        args: vec![
                            Expr::Variable("a".to_string()),
                            Expr::Variable("b".to_string()),
                        ],
                    },
                    Expr::Variable("c".to_string()),
                ],
            }
        );
    }

    #[test]
    fn parse_bitwise_xor_precedence() {
        // a | b ^ c should parse as a | (b ^ c) (xor is higher than or)
        let program = parse("a | b ^ c").unwrap();
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "bitOr".to_string(),
                args: vec![
                    Expr::Variable("a".to_string()),
                    Expr::FunctionCall {
                        name: "bitXor".to_string(),
                        args: vec![
                            Expr::Variable("b".to_string()),
                            Expr::Variable("c".to_string()),
                        ],
                    },
                ],
            }
        );
    }

    // --- Newline statement separation tests ---

    #[test]
    fn parse_statement_newline_separator() {
        let program = parse("x = 42\nx + 1").unwrap();
        assert_eq!(program.statements.len(), 1);
        assert_eq!(
            program.statements[0],
            Statement {
                name: "x".to_string(),
                value: Expr::Literal(LiteralValue::Int(42)),
            }
        );
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "add".to_string(),
                args: vec![
                    Expr::Variable("x".to_string()),
                    Expr::Literal(LiteralValue::Int(1)),
                ],
            }
        );
    }

    #[test]
    fn parse_statement_semicolon_separator() {
        let program = parse("x = 42; x + 1").unwrap();
        assert_eq!(program.statements.len(), 1);
        assert_eq!(program.statements[0].name, "x");
        assert_eq!(
            program.result,
            Expr::FunctionCall {
                name: "add".to_string(),
                args: vec![
                    Expr::Variable("x".to_string()),
                    Expr::Literal(LiteralValue::Int(1)),
                ],
            }
        );
    }

    #[test]
    fn parse_statement_same_line_error() {
        // "x = 42 x + 1" on a single line without semicolon should error
        let result = parse("x = 42 x + 1");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("expected newline or ';' after statement"),
            "unexpected error: {msg}"
        );
    }
}
