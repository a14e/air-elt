use air_elt_expr_types::limits::{
    MAX_AST_NODES, MAX_EXPR_DEPTH, MAX_EXPR_SOURCE_LEN, MAX_FUNCTION_ARGS, MAX_VARIABLES,
};

use crate::detect;
use crate::error::ExprError;
use crate::lexer::Lexer;
use crate::model::{
    ConditionalExpr, Expr, FieldsSelector, InterpolationSegment, LiteralValue, Program, Statement,
};
use crate::pratt_operator::{Associativity, PrattOperator};
use crate::token::{SpannedToken, StringPart, Token};

/// Expression parser. Converts expression source strings into [`Program`] ASTs.
///
/// `comptime` selects the grammar surface: compile-time config expressions
/// (`create_comptime`) reject the runtime-only `field()` / `fields()` /
/// backtick forms at parse time, since those reference a source row that does
/// not exist while patching config. Transform compute scripts use the default
/// runtime parser (`create`), where that grammar is enabled.
pub struct Parser {
    comptime: bool,
}

impl Parser {
    /// Create a runtime parser — `field()` / `fields()` / backtick are allowed.
    pub fn create() -> Self {
        Self { comptime: false }
    }

    /// Create a compile-time parser — the runtime-only `field()` / `fields()` /
    /// backtick grammar is rejected at parse time.
    pub fn create_comptime() -> Self {
        Self { comptime: true }
    }

    pub fn parse(&self, input: &str) -> Result<Program, ExprError> {
        if detect::is_expression(input) {
            return parse_inner(input, self.comptime);
        }
        if detect::has_interpolation(input) {
            return parse_interpolation_template(input, self.comptime);
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
        parse_inner(input, self.comptime)
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

fn parse_interpolation_template(input: &str, comptime: bool) -> Result<Program, ExprError> {
    if input.len() > MAX_EXPR_SOURCE_LEN {
        return Err(ExprError::ExpressionTooLong {
            len: input.len(),
            max: MAX_EXPR_SOURCE_LEN,
        });
    }
    let mut lexer = Lexer::new(input);
    let tokens = lexer.tokenize_as_interpolation()?;
    let mut state = ParseState::new(tokens, comptime);
    state.parse_program()
}

/// Parse an expression source string into a [`Program`] AST (runtime grammar).
///
/// Convenience wrapper around `Parser::create().parse_expression(input)`.
pub fn parse(input: &str) -> Result<Program, ExprError> {
    parse_inner(input, false)
}

fn parse_inner(input: &str, comptime: bool) -> Result<Program, ExprError> {
    if input.len() > MAX_EXPR_SOURCE_LEN {
        return Err(ExprError::ExpressionTooLong {
            len: input.len(),
            max: MAX_EXPR_SOURCE_LEN,
        });
    }

    let mut lexer = Lexer::new(input);
    let spanned_tokens = lexer.tokenize()?;
    let mut state = ParseState::new(spanned_tokens, comptime);
    state.parse_program()
}

struct ParseState {
    tokens: Vec<SpannedToken>,
    pos: usize,
    depth: usize,
    node_count: usize,
    variable_count: usize,
    /// When set, the runtime-only `field()` / `fields()` / backtick grammar is
    /// rejected (compile-time config context).
    comptime: bool,
}

impl ParseState {
    fn new(tokens: Vec<SpannedToken>, comptime: bool) -> Self {
        Self {
            tokens,
            pos: 0,
            depth: 0,
            node_count: 0,
            variable_count: 0,
            comptime,
        }
    }

    /// Reject the runtime-only field grammar in a compile-time context.
    fn reject_field_in_comptime(&self, form: &str) -> Result<(), ExprError> {
        if self.comptime {
            return Err(ExprError::Parse {
                position: self.pos,
                message: format!(
                    "{form} is only valid in a transform compute script, \
                     not in a compile-time config expression"
                ),
            });
        }
        Ok(())
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
        let result = self.parse_binary_expr(0);
        self.decrement_depth();
        result
    }

    /// Precedence climbing over the [`PrattOperator`] table — one loop in place
    /// of the ten-level recursive ladder it replaces. `min_binding_power` is the
    /// lowest binding power this call may consume; an operator binding looser than
    /// that belongs to an enclosing call.
    ///
    /// Each fold deepens the left-nested result by one level and holds a depth
    /// unit (via [`fold_binary_chain`](Self::fold_binary_chain)) for the duration,
    /// so the depth guard bounds the *tree* depth — a long flat chain
    /// (`a + b + c + …`) or a right-associative one (`a ** b ** …`) can never
    /// build an AST too deep to recurse over (evaluate, optimize, or even drop),
    /// exactly like parenthesized nesting. Folding the ladder into one loop also
    /// keeps the native parse stack shallow (a handful of frames per level, not
    /// ~13), so the guard returns a clean error instead of overflowing.
    fn parse_binary_expr(&mut self, min_binding_power: u8) -> Result<Expr, ExprError> {
        let mut folds = 0usize;
        let result = self.fold_binary_chain(min_binding_power, &mut folds);
        // Release the depth units this call held, on success and on error alike.
        for _ in 0..folds {
            self.decrement_depth();
        }
        result
    }

    fn fold_binary_chain(
        &mut self,
        min_binding_power: u8,
        folds: &mut usize,
    ) -> Result<Expr, ExprError> {
        let mut left = self.parse_unary_expr()?;

        while let Some(operator) = PrattOperator::for_token(self.peek()) {
            let (left_binding_power, right_binding_power) = operator.binding_powers();
            if left_binding_power < min_binding_power {
                break;
            }
            let non_associative = operator.associativity() == Associativity::NonAssociative;

            self.advance();
            *folds += 1;
            self.increment_depth()?;
            let right = self.parse_binary_expr(right_binding_power)?;
            self.count_node()?;
            left = operator.build(left, right);

            // A non-associative operator (comparison) does not chain: in the old
            // ladder a second one was left unconsumed and rejected downstream, so
            // reproduce that rejection here with a clearer message.
            if non_associative
                && PrattOperator::for_token(self.peek())
                    .is_some_and(|next| next.associativity() == Associativity::NonAssociative)
            {
                return Err(ExprError::Parse {
                    position: self.pos,
                    message: "comparison operators do not chain; add parentheses".to_string(),
                });
            }
        }

        Ok(left)
    }

    fn parse_unary_expr(&mut self) -> Result<Expr, ExprError> {
        let operator_name = match self.peek() {
            Token::Minus => "negate",
            Token::Not => "not",
            Token::Tilde => "bitNot",
            _ => return self.parse_call_expr(),
        };

        self.advance();
        // Guard the recursion so a long prefix chain (`~~~…x`) is depth-bounded
        // like every other nesting, rather than recursing unchecked.
        self.increment_depth()?;
        let operand = self.parse_unary_expr();
        self.decrement_depth();
        let operand = operand?;
        self.count_node()?;
        Ok(Expr::FunctionCall {
            name: operator_name.to_string(),
            args: vec![operand],
        })
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
                    "field" => return self.parse_field(),
                    "fields" => return self.parse_fields(),
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

    /// Parse an `if` and any directly-chained `else if`s **iteratively**,
    /// desugaring `if(c1, v1, if(c2, v2, …, default))` into a flat `multiIf`. A
    /// lone `if` (whose else is not another `if(...)`) stays an `if`.
    ///
    /// Folding the chain here, instead of recursing through each nested `if`,
    /// means a long `if/else if` ladder costs no nesting depth and produces a flat
    /// node that every downstream pass walks iteratively — so thousands of
    /// branches parse, type-check, and evaluate without deep recursion. The result
    /// is identical in meaning to the nested form (and to what the optimizer's
    /// conditional flattening would produce).
    fn parse_if_conditional(&mut self) -> Result<Expr, ExprError> {
        self.advance(); // consume "if"
        self.advance(); // consume "("
        let mut branches = Vec::new();
        // Each `if(` opened owes a closing `)`; the whole chain shares one tail.
        let mut open_parens = 1usize;

        let default = loop {
            let condition = self.parse_expr()?;
            self.expect(&Token::Comma)?;
            let value = self.parse_expr()?;
            self.expect(&Token::Comma)?;
            branches.push((condition, value));

            // An else position of `if(...)` continues the chain — fold it in.
            let chains = matches!(self.peek(), Token::Ident(name) if name.as_str() == "if")
                && *self.peek_ahead(1) == Token::LParen;
            if chains {
                self.advance(); // consume "if"
                self.advance(); // consume "("
                open_parens += 1;
                continue;
            }

            break self.parse_expr()?; // the final else branch
        };

        for _ in 0..open_parens {
            self.expect(&Token::RParen)?;
        }
        self.count_node()?;

        if branches.len() == 1 {
            let (condition, then_branch) = branches.pop().expect("one branch present");
            return Ok(Expr::Conditional(ConditionalExpr::If {
                condition: Box::new(condition),
                then_branch: Box::new(then_branch),
                else_branch: Box::new(default),
            }));
        }
        Ok(Expr::Conditional(ConditionalExpr::MultiIf {
            branches,
            default: Box::new(default),
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

        // Desugar `coalesce(a, b, c)` into the right-nested `IfNull(a, IfNull(b, c))`.
        // Each wrap deepens the tree by one, so charge it to the depth budget like
        // a binary fold — a huge argument list must not build a chain too deep to
        // recurse over (evaluate, type-check, or even drop). The held depth units
        // are released once the chain is built.
        let mut result = args.pop().expect("checked non-empty");
        let mut wraps = 0usize;
        let outcome = loop {
            let Some(arg) = args.pop() else { break Ok(()) };
            if let Err(error) = self.count_node() {
                break Err(error);
            }
            wraps += 1;
            if let Err(error) = self.increment_depth() {
                break Err(error);
            }
            result = Expr::Conditional(ConditionalExpr::IfNull {
                value: Box::new(arg),
                alternative: Box::new(result),
            });
        };
        for _ in 0..wraps {
            self.decrement_depth();
        }
        outcome?;
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

    fn parse_field(&mut self) -> Result<Expr, ExprError> {
        self.reject_field_in_comptime("field()")?;
        self.advance(); // consume "field"
        self.advance(); // consume "("
        let arg = self.parse_expr()?;
        if *self.peek() != Token::RParen {
            return Err(ExprError::Parse {
                position: self.pos,
                message: "field(...) requires exactly one argument".to_string(),
            });
        }
        self.expect(&Token::RParen)?;
        self.count_node()?;
        Ok(Expr::Field(Box::new(arg)))
    }

    fn parse_fields(&mut self) -> Result<Expr, ExprError> {
        self.reject_field_in_comptime("fields()")?;
        self.advance(); // consume "fields"
        self.advance(); // consume "("
        let selector = self.parse_fields_selector()?;
        self.expect(&Token::RParen)?;
        self.count_node()?;
        Ok(Expr::Fields(selector))
    }

    fn parse_fields_selector(&mut self) -> Result<FieldsSelector, ExprError> {
        let raw = match self.peek().clone() {
            Token::RawStringLit(value) => {
                self.advance();
                value
            }
            Token::StringLit(parts) => {
                self.advance();
                match parts.as_slice() {
                    [StringPart::Literal(text)] => text.clone(),
                    _ => return Err(self.fields_selector_error()),
                }
            }
            _ => return Err(self.fields_selector_error()),
        };

        if raw == "*" {
            return Ok(FieldsSelector::All);
        }

        let mut names = Vec::new();
        for part in raw.split(',') {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                return Err(ExprError::Parse {
                    position: self.pos,
                    message: "fields(...) selector contains an empty field name".to_string(),
                });
            }
            names.push(trimmed.to_string());
        }
        Ok(FieldsSelector::Named(names))
    }

    fn fields_selector_error(&self) -> ExprError {
        ExprError::Parse {
            position: self.pos,
            message: "fields(...) requires a string literal selector".to_string(),
        }
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
            Token::FieldLit(name) => {
                self.reject_field_in_comptime("a `backtick` field literal")?;
                self.advance();
                self.count_node()?;
                Ok(Expr::Field(Box::new(Expr::Literal(LiteralValue::String(
                    name,
                )))))
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
    fn power_is_right_associative() {
        // `2 ** 3 ** 2` parses as `power(2, power(3, 2))`, not `power(power(2,3),2)`.
        let inner = Expr::FunctionCall {
            name: "power".to_string(),
            args: vec![
                Expr::Literal(LiteralValue::Int(3)),
                Expr::Literal(LiteralValue::Int(2)),
            ],
        };
        assert_eq!(
            parse("2 ** 3 ** 2").unwrap().result,
            Expr::FunctionCall {
                name: "power".to_string(),
                args: vec![Expr::Literal(LiteralValue::Int(2)), inner],
            }
        );
    }

    #[test]
    fn comparison_binds_tighter_than_bitwise_or() {
        // A multi-level precedence gap: `1 | 2 == 3` is `1 | (2 == 3)`.
        let equality = Expr::FunctionCall {
            name: "equals".to_string(),
            args: vec![
                Expr::Literal(LiteralValue::Int(2)),
                Expr::Literal(LiteralValue::Int(3)),
            ],
        };
        assert_eq!(
            parse("1 | 2 == 3").unwrap().result,
            Expr::FunctionCall {
                name: "bitOr".to_string(),
                args: vec![Expr::Literal(LiteralValue::Int(1)), equality],
            }
        );
    }

    #[test]
    fn comparison_operators_do_not_chain() {
        // Comparisons are non-associative: `a < b < c` must be a parse error, not a
        // silent left- or right-grouping.
        for source in ["1 < 2 < 3", "1 == 2 != 3", "1 <= 2 >= 3"] {
            assert!(
                matches!(parse(source), Err(ExprError::Parse { .. })),
                "expected a parse error for non-associative `{source}`"
            );
        }
        // A comparison still composes with looser logical operators, though.
        assert!(parse("1 < 2 && 3 < 4").is_ok());
    }

    #[test]
    fn deep_nesting_errors_cleanly_on_a_small_stack() {
        // The precedence-climbing parser keeps a nesting level to a handful of
        // native frames, and EVERY deepening point (parens, nested conditionals,
        // binary/unary/coalesce chains) is charged to the depth guard — so deeply
        // nested input of any shape returns `NestingTooDeep` on a 2 MiB stack,
        // never overflowing it. (Strings are built linearly, not by re-formatting
        // a growing accumulator.)
        let parse_on_small_stack = |input: String| {
            std::thread::Builder::new()
                .stack_size(2 * 1024 * 1024)
                .spawn(move || parse(&input))
                .expect("failed to spawn thread")
                .join()
                .expect("thread panicked")
        };

        let depth = 1000;

        let parens = format!("{}42{}", "(".repeat(depth), ")".repeat(depth));

        let mut multi_ifs = String::with_capacity(depth * 18);
        for _ in 0..depth {
            multi_ifs.push_str("multiIf(true, 1, ");
        }
        multi_ifs.push('1');
        for _ in 0..depth {
            multi_ifs.push(')');
        }

        let power_chain = vec!["2"; depth].join(" ** ");

        let mut additions = String::with_capacity(depth * 4);
        additions.push('1');
        for _ in 0..depth {
            additions.push_str(" + 1");
        }

        let mut coalesce = String::with_capacity(depth * 3 + 12);
        coalesce.push_str("coalesce(1");
        for _ in 0..depth {
            coalesce.push_str(", 1");
        }
        coalesce.push(')');

        for input in [parens, multi_ifs, power_chain, additions, coalesce] {
            match parse_on_small_stack(input) {
                Err(ExprError::NestingTooDeep { max }) => assert_eq!(max, MAX_EXPR_DEPTH),
                other => panic!("expected NestingTooDeep, got {other:?}"),
            }
        }
    }

    #[test]
    fn case_ladders_are_not_depth_limited() {
        // A flat `multiIf(c1, v1, …, default)` is parsed iteratively, so a case
        // ladder with far more than MAX_EXPR_DEPTH branches parses fine.
        let mut flat = String::from("multiIf(");
        for n in 0..500 {
            if n > 0 {
                flat.push_str(", ");
            }
            flat.push_str(&format!("x == {n}, {n}"));
        }
        flat.push_str(", -1)");
        assert!(parse(&flat).is_ok(), "a 500-branch flat multiIf must parse");

        // An `if/else if` ladder folds into the SAME flat `multiIf`, so it too
        // parses far past the depth limit — the chain consumes no nesting depth.
        // Built linearly: the `if(...,` prefixes, the default, then the `)` tail.
        let cases = 1500;
        let mut chain = String::with_capacity(cases * 18);
        for n in 0..cases {
            chain.push_str(&format!("if(x == {n}, {n}, "));
        }
        chain.push('0');
        for _ in 0..cases {
            chain.push(')');
        }
        match parse(&chain) {
            Ok(Program {
                result: Expr::Conditional(ConditionalExpr::MultiIf { branches, .. }),
                ..
            }) => assert_eq!(branches.len(), cases, "the chain folds to one multiIf"),
            other => panic!("expected a flat multiIf from the if/else-if chain, got {other:?}"),
        }
    }

    #[test]
    fn node_limit_produces_error() {
        // Exceed the node cap with a flat, shallow structure (a wide `multiIf`).
        // A long binary chain cannot reach this cap — it trips the depth guard
        // first (each fold is charged to the depth budget).
        let mut input = String::from("multiIf(true");
        for _ in 0..MAX_AST_NODES {
            input.push_str(", 1");
        }
        input.push(')');

        match parse(&input) {
            Err(ExprError::TooManyNodes { count: _, max }) => assert_eq!(max, MAX_AST_NODES),
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
    fn parse_field_function_call() {
        let program = parse("field(\"x\")").unwrap();
        assert_eq!(
            program.result,
            Expr::Field(Box::new(Expr::Literal(LiteralValue::String(
                "x".to_string()
            ))))
        );
    }

    #[test]
    fn parse_field_backtick_literal() {
        let program = parse("`x`").unwrap();
        assert_eq!(
            program.result,
            Expr::Field(Box::new(Expr::Literal(LiteralValue::String(
                "x".to_string()
            ))))
        );
    }

    #[test]
    fn parse_field_nested() {
        let program = parse("field(field(\"x\"))").unwrap();
        assert_eq!(
            program.result,
            Expr::Field(Box::new(Expr::Field(Box::new(Expr::Literal(
                LiteralValue::String("x".to_string())
            )))))
        );
    }

    #[test]
    fn parse_fields_all() {
        let program = parse("fields(\"*\")").unwrap();
        assert_eq!(program.result, Expr::Fields(FieldsSelector::All));
    }

    #[test]
    fn parse_fields_named() {
        let program = parse("fields(\"a,b\")").unwrap();
        assert_eq!(
            program.result,
            Expr::Fields(FieldsSelector::Named(vec![
                "a".to_string(),
                "b".to_string()
            ]))
        );
    }

    #[test]
    fn parse_fields_non_string_errors() {
        let result = parse("fields(123)");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("fields(...) requires a string literal selector"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_field_without_paren_is_variable() {
        let program = parse("field").unwrap();
        assert_eq!(program.result, Expr::Variable("field".to_string()));
    }

    #[test]
    fn comptime_parser_rejects_field_grammar() {
        let comptime = Parser::create_comptime();
        for src in ["field(\"x\")", "fields(\"*\")", "`x`"] {
            let err = comptime
                .parse_expression(src)
                .expect_err("comptime parser must reject runtime field grammar");
            let msg = format!("{err}");
            assert!(
                msg.contains("only valid in a transform compute script"),
                "unexpected error for {src:?}: {msg}"
            );
        }
        // The runtime parser still accepts them.
        let runtime = Parser::create();
        assert!(runtime.parse_expression("field(\"x\")").is_ok());
        assert!(runtime.parse_expression("`x`").is_ok());
        assert!(runtime.parse_expression("fields(\"*\")").is_ok());
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
