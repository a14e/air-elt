use crate::error::ExprError;
use crate::token::{SpannedToken, StringPart, Token};

/// Hand-rolled lexer for the expression language.
/// Supports double-quoted strings with interpolation and single-quoted raw strings.
/// Tracks line numbers for newline-aware statement separation.
/// `#` starts a line comment that runs to the end of the line (the newline itself
/// is left in place so it still separates statements).
pub struct Lexer<'a> {
    input: &'a str,
    pos: usize,
    line: u32,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            input,
            pos: 0,
            line: 1,
        }
    }

    pub fn tokenize_as_interpolation(&mut self) -> Result<Vec<SpannedToken>, ExprError> {
        let token = self.read_interpolation_template()?;
        Ok(vec![
            SpannedToken {
                token,
                line: self.line,
            },
            SpannedToken {
                token: Token::Eof,
                line: self.line,
            },
        ])
    }

    pub fn tokenize(&mut self) -> Result<Vec<SpannedToken>, ExprError> {
        let mut tokens = Vec::new();
        loop {
            self.skip_trivia();
            if self.pos >= self.input.len() {
                tokens.push(SpannedToken {
                    token: Token::Eof,
                    line: self.line,
                });
                break;
            }
            let line = self.line;
            let token = self.next_token()?;
            tokens.push(SpannedToken { token, line });
        }
        Ok(tokens)
    }

    fn current_byte(&self) -> u8 {
        self.input.as_bytes()[self.pos]
    }

    /// Skip whitespace and `#` line comments between tokens. A comment runs to the
    /// end of the line; the terminating newline is deliberately left for the next
    /// iteration so the line counter advances and newline-based statement
    /// separation is preserved. A `#` inside a string literal never reaches here —
    /// the string is consumed as a single token in [`Self::next_token`].
    fn skip_trivia(&mut self) {
        while self.pos < self.input.len() {
            let b = self.input.as_bytes()[self.pos];
            if b == b'\n' {
                self.line += 1;
                self.pos += 1;
            } else if b == b'#' {
                while self.pos < self.input.len() && self.input.as_bytes()[self.pos] != b'\n' {
                    self.pos += 1;
                }
            } else if b.is_ascii_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn next_token(&mut self) -> Result<Token, ExprError> {
        let b = self.current_byte();
        match b {
            b'+' => {
                self.pos += 1;
                Ok(Token::Plus)
            }
            b'-' => {
                self.pos += 1;
                Ok(Token::Minus)
            }
            b'*' => {
                self.pos += 1;
                if self.pos < self.input.len() && self.input.as_bytes()[self.pos] == b'*' {
                    self.pos += 1;
                    Ok(Token::Power)
                } else {
                    Ok(Token::Star)
                }
            }
            b'/' => {
                self.pos += 1;
                Ok(Token::Slash)
            }
            b'%' => {
                self.pos += 1;
                Ok(Token::Percent)
            }
            b'(' => {
                self.pos += 1;
                Ok(Token::LParen)
            }
            b')' => {
                self.pos += 1;
                Ok(Token::RParen)
            }
            b'{' => {
                self.pos += 1;
                Ok(Token::LBrace)
            }
            b'}' => {
                self.pos += 1;
                Ok(Token::RBrace)
            }
            b'[' => {
                self.pos += 1;
                Ok(Token::LBracket)
            }
            b']' => {
                self.pos += 1;
                Ok(Token::RBracket)
            }
            b',' => {
                self.pos += 1;
                Ok(Token::Comma)
            }
            b':' => {
                self.pos += 1;
                Ok(Token::Colon)
            }
            b';' => {
                self.pos += 1;
                Ok(Token::Semicolon)
            }
            b'^' => {
                self.pos += 1;
                Ok(Token::Caret)
            }
            b'~' => {
                self.pos += 1;
                Ok(Token::Tilde)
            }
            b'!' => {
                self.pos += 1;
                if self.pos < self.input.len() && self.input.as_bytes()[self.pos] == b'=' {
                    self.pos += 1;
                    Ok(Token::NotEq)
                } else {
                    Ok(Token::Not)
                }
            }
            b'=' => {
                self.pos += 1;
                if self.pos < self.input.len() && self.input.as_bytes()[self.pos] == b'=' {
                    self.pos += 1;
                    Ok(Token::EqEq)
                } else {
                    Ok(Token::Eq)
                }
            }
            b'<' => {
                self.pos += 1;
                if self.pos < self.input.len() && self.input.as_bytes()[self.pos] == b'<' {
                    self.pos += 1;
                    Ok(Token::ShiftLeft)
                } else if self.pos < self.input.len() && self.input.as_bytes()[self.pos] == b'=' {
                    self.pos += 1;
                    Ok(Token::LtEq)
                } else {
                    Ok(Token::Lt)
                }
            }
            b'>' => {
                self.pos += 1;
                if self.pos < self.input.len() && self.input.as_bytes()[self.pos] == b'>' {
                    self.pos += 1;
                    Ok(Token::ShiftRight)
                } else if self.pos < self.input.len() && self.input.as_bytes()[self.pos] == b'=' {
                    self.pos += 1;
                    Ok(Token::GtEq)
                } else {
                    Ok(Token::Gt)
                }
            }
            b'&' => {
                self.pos += 1;
                if self.pos < self.input.len() && self.input.as_bytes()[self.pos] == b'&' {
                    self.pos += 1;
                    Ok(Token::And)
                } else {
                    Ok(Token::Ampersand)
                }
            }
            b'|' => {
                self.pos += 1;
                if self.pos < self.input.len() && self.input.as_bytes()[self.pos] == b'|' {
                    self.pos += 1;
                    Ok(Token::Or)
                } else {
                    Ok(Token::Pipe)
                }
            }
            b'"' => self.read_double_quoted_string(),
            b'\'' => self.read_single_quoted_string(),
            b'`' => self.read_field_literal(),
            b'0'..=b'9' => {
                // A compact human duration (`10s`, `1h30m`) takes precedence
                // over a bare number when a unit suffix immediately follows.
                // Pure numbers (`10`, `3.14`) and hex (`0xFF`) fall through.
                if let Some(duration) = self.try_read_human_duration() {
                    Ok(Token::DurationLit(duration))
                } else if b == b'0' {
                    self.read_number_or_hex()
                } else {
                    self.read_number()
                }
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.read_ident_or_keyword(),
            _ => Err(ExprError::Parse {
                position: self.pos,
                message: format!(
                    "unexpected character: '{}'",
                    self.input[self.pos..].chars().next().unwrap_or('?')
                ),
            }),
        }
    }

    fn read_double_quoted_string(&mut self) -> Result<Token, ExprError> {
        let start = self.pos;
        self.pos += 1;
        self.read_string_body(true, start)
    }

    fn read_interpolation_template(&mut self) -> Result<Token, ExprError> {
        self.read_string_body(false, 0)
    }

    fn read_string_body(&mut self, quoted: bool, start: usize) -> Result<Token, ExprError> {
        let mut parts: Vec<StringPart> = Vec::new();
        let mut current_literal = String::new();

        while self.pos < self.input.len() {
            let b = self.input.as_bytes()[self.pos];
            match b {
                b'"' if quoted => {
                    self.pos += 1;
                    if !current_literal.is_empty() {
                        parts.push(StringPart::Literal(current_literal));
                    }
                    return Ok(Token::StringLit(parts));
                }
                b'\\' if quoted => {
                    self.pos += 1;
                    if self.pos >= self.input.len() {
                        return Err(ExprError::UnterminatedString { position: start });
                    }
                    let escaped = match self.input.as_bytes()[self.pos] {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'\\' => '\\',
                        b'"' => '"',
                        b'{' => '{',
                        b'}' => '}',
                        other => other as char,
                    };
                    current_literal.push(escaped);
                    self.pos += 1;
                }
                b'{' => {
                    if self.pos + 1 < self.input.len()
                        && self.input.as_bytes()[self.pos + 1] == b'{'
                    {
                        current_literal.push('{');
                        self.pos += 2;
                    } else {
                        if !current_literal.is_empty() {
                            parts.push(StringPart::Literal(std::mem::take(&mut current_literal)));
                        }
                        self.pos += 1;
                        let expr_source = self.read_interpolation_body(start)?;
                        parts.push(StringPart::Expr(expr_source));
                    }
                }
                b'}' if !quoted
                    && self.pos + 1 < self.input.len()
                    && self.input.as_bytes()[self.pos + 1] == b'}' =>
                {
                    current_literal.push('}');
                    self.pos += 2;
                }
                b'\n' => {
                    self.line += 1;
                    current_literal.push('\n');
                    self.pos += 1;
                }
                _ => {
                    let ch = self.input[self.pos..].chars().next().unwrap_or('?');
                    current_literal.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }

        if quoted {
            return Err(ExprError::UnterminatedString { position: start });
        }
        if !current_literal.is_empty() {
            parts.push(StringPart::Literal(current_literal));
        }
        Ok(Token::StringLit(parts))
    }

    fn read_interpolation_body(&mut self, string_start: usize) -> Result<String, ExprError> {
        let start = self.pos;
        let mut depth = 1u32;
        while self.pos < self.input.len() {
            match self.input.as_bytes()[self.pos] {
                b'{' => {
                    depth += 1;
                    self.pos += 1;
                }
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        let body = self.input[start..self.pos].to_string();
                        self.pos += 1; // skip closing }
                        return Ok(body);
                    }
                    self.pos += 1;
                }
                b'"' => {
                    self.pos += 1;
                    // Skip nested string
                    while self.pos < self.input.len() {
                        match self.input.as_bytes()[self.pos] {
                            b'\\' => self.pos += 2,
                            b'"' => {
                                self.pos += 1;
                                break;
                            }
                            b'\n' => {
                                self.line += 1;
                                self.pos += 1;
                            }
                            _ => self.pos += 1,
                        }
                    }
                }
                b'\'' => {
                    self.pos += 1;
                    while self.pos < self.input.len() && self.input.as_bytes()[self.pos] != b'\'' {
                        if self.input.as_bytes()[self.pos] == b'\n' {
                            self.line += 1;
                        }
                        self.pos += 1;
                    }
                    if self.pos < self.input.len() {
                        self.pos += 1;
                    }
                }
                b'\n' => {
                    self.line += 1;
                    self.pos += 1;
                }
                _ => self.pos += 1,
            }
        }
        Err(ExprError::UnterminatedInterpolation {
            position: string_start,
        })
    }

    fn read_single_quoted_string(&mut self) -> Result<Token, ExprError> {
        let start = self.pos;
        self.pos += 1; // skip opening '
        let mut content = String::new();
        while self.pos < self.input.len() {
            let b = self.input.as_bytes()[self.pos];
            match b {
                b'\'' => {
                    self.pos += 1;
                    return Ok(Token::RawStringLit(content));
                }
                b'\\' => {
                    self.pos += 1;
                    if self.pos >= self.input.len() {
                        return Err(ExprError::UnterminatedString { position: start });
                    }
                    let escaped = match self.input.as_bytes()[self.pos] {
                        b'\'' => '\'',
                        b'\\' => '\\',
                        other => {
                            content.push('\\');
                            other as char
                        }
                    };
                    content.push(escaped);
                    self.pos += 1;
                }
                b'\n' => {
                    self.line += 1;
                    content.push('\n');
                    self.pos += 1;
                }
                _ => {
                    let ch = self.input[self.pos..].chars().next().unwrap_or('?');
                    content.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        Err(ExprError::UnterminatedString { position: start })
    }

    /// Read a backtick-delimited field literal `` `name` ``.
    /// The inner text is raw: no escapes and no interpolation.
    fn read_field_literal(&mut self) -> Result<Token, ExprError> {
        let start = self.pos;
        self.pos += 1; // skip opening backtick
        let mut content = String::new();
        while self.pos < self.input.len() {
            let b = self.input.as_bytes()[self.pos];
            match b {
                b'`' => {
                    self.pos += 1;
                    return Ok(Token::FieldLit(content));
                }
                b'\n' => {
                    self.line += 1;
                    content.push('\n');
                    self.pos += 1;
                }
                _ => {
                    let ch = self.input[self.pos..].chars().next().unwrap_or('?');
                    content.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        Err(ExprError::UnterminatedString { position: start })
    }

    fn read_number_or_hex(&mut self) -> Result<Token, ExprError> {
        // Check for 0x / 0X prefix (hex literal)
        if self.pos + 1 < self.input.len() {
            let next = self.input.as_bytes()[self.pos + 1];
            if next == b'x' || next == b'X' {
                return self.read_hex_number();
            }
        }
        self.read_number()
    }

    fn read_hex_number(&mut self) -> Result<Token, ExprError> {
        let start = self.pos;
        self.pos += 2; // skip 0x
        let hex_start = self.pos;
        while self.pos < self.input.len() {
            let b = self.input.as_bytes()[self.pos];
            if b.is_ascii_hexdigit() {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == hex_start {
            return Err(ExprError::Parse {
                position: start,
                message: "expected hex digits after '0x'".to_string(),
            });
        }
        let hex_text = &self.input[hex_start..self.pos];
        let value = i64::from_str_radix(hex_text, 16).map_err(|_| ExprError::Parse {
            position: start,
            message: format!("invalid hex literal: 0x{hex_text}"),
        })?;
        Ok(Token::IntLit(value))
    }

    fn read_number(&mut self) -> Result<Token, ExprError> {
        let start = self.pos;
        while self.pos < self.input.len() && self.input.as_bytes()[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if self.pos < self.input.len() && self.input.as_bytes()[self.pos] == b'.' {
            self.pos += 1;
            while self.pos < self.input.len() && self.input.as_bytes()[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
            let text = &self.input[start..self.pos];
            let value: f64 = text.parse().map_err(|_| ExprError::Parse {
                position: start,
                message: format!("invalid float: {text}"),
            })?;
            Ok(Token::FloatLit(value))
        } else {
            let text = &self.input[start..self.pos];
            let value: i64 = text.parse().map_err(|_| ExprError::Parse {
                position: start,
                message: format!("invalid integer: {text}"),
            })?;
            Ok(Token::IntLit(value))
        }
    }

    fn read_ident_or_keyword(&mut self) -> Result<Token, ExprError> {
        let start = self.pos;
        while self.pos < self.input.len() {
            let b = self.input.as_bytes()[self.pos];
            if b.is_ascii_alphanumeric() || b == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let text = &self.input[start..self.pos];
        let token = match text {
            "true" => Token::BoolLit(true),
            "false" => Token::BoolLit(false),
            "null" => Token::NullLit,
            "if" => Token::If,
            "else" => Token::Else,
            "in" => Token::In,
            _ => {
                // ISO-8601 duration literal (`PT1H30M`, `P1DT2H`, `P1W`).
                // Heuristic: only words shaped `P`/`p` + (digit | `T`/`t`)
                // are even tried, so ordinary identifiers like `Price` or
                // `Point` never hit the parser. A run that does not parse as
                // ISO falls back to a plain identifier.
                match parse_iso_duration_ident(text) {
                    Some(duration) => Token::DurationLit(duration),
                    None => Token::Ident(text.to_string()),
                }
            }
        };
        Ok(token)
    }

    /// Attempt to lex a compact human duration literal (`10s`, `1h30m`,
    /// `500ms`) at the current position. A candidate is a contiguous run of
    /// digits, `.`, and ASCII letters (no spaces) with at least one letter;
    /// it is accepted only if [`air_elt_commons::interval::parse`] accepts
    /// it. On a match `pos` advances past the literal; otherwise `pos` is
    /// left untouched so the caller can fall back to numeric/hex lexing.
    fn try_read_human_duration(&mut self) -> Option<std::time::Duration> {
        let rest = &self.input[self.pos..];
        // Greedy run of digits / `.` / ASCII letters with no spaces. Every
        // matched byte is ASCII, so the byte length is a valid char boundary.
        let run_len = rest
            .bytes()
            .take_while(|c| c.is_ascii_digit() || *c == b'.' || c.is_ascii_alphabetic())
            .count();
        let candidate = &rest[..run_len];
        // Require at least one unit letter so bare numbers / floats / hex fall
        // through to numeric lexing.
        if !candidate.bytes().any(|c| c.is_ascii_alphabetic()) {
            return None;
        }
        let duration = air_elt_commons::interval::parse(candidate).ok()?;
        self.pos += run_len;
        Some(duration)
    }
}

/// Parse an already-lexed identifier run as an ISO-8601 duration, but only
/// when it is shaped like one (`P`/`p` followed by a digit or `T`/`t`).
/// Returns `None` for ordinary identifiers so they stay identifiers.
fn parse_iso_duration_ident(text: &str) -> Option<std::time::Duration> {
    let bytes = text.as_bytes();
    let looks_iso = matches!(bytes.first(), Some(b'P' | b'p'))
        && matches!(bytes.get(1), Some(c) if c.is_ascii_digit() || *c == b'T' || *c == b't');
    if !looks_iso {
        return None;
    }
    air_elt_commons::interval::parse(text).ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::approx_constant)]
mod tests {
    use super::*;

    fn tokens(input: &str) -> Vec<Token> {
        let mut lexer = Lexer::new(input);
        lexer
            .tokenize()
            .unwrap()
            .into_iter()
            .map(|st| st.token)
            .collect()
    }

    #[test]
    fn tokenize_simple_function_call() {
        assert_eq!(
            tokens("env(\"KEY\")"),
            vec![
                Token::Ident("env".to_string()),
                Token::LParen,
                Token::StringLit(vec![StringPart::Literal("KEY".to_string())]),
                Token::RParen,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn tokenize_single_quoted_string() {
        assert_eq!(
            tokens("'hello {not interpolated}'"),
            vec![
                Token::RawStringLit("hello {not interpolated}".to_string()),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn tokenize_interpolated_string() {
        assert_eq!(
            tokens("\"hello {name}\""),
            vec![
                Token::StringLit(vec![
                    StringPart::Literal("hello ".to_string()),
                    StringPart::Expr("name".to_string()),
                ]),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn tokenize_arithmetic() {
        assert_eq!(
            tokens("1 + 2 * 3"),
            vec![
                Token::IntLit(1),
                Token::Plus,
                Token::IntLit(2),
                Token::Star,
                Token::IntLit(3),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn tokenize_comparison_operators() {
        assert_eq!(
            tokens("a >= 10 && b != null"),
            vec![
                Token::Ident("a".to_string()),
                Token::GtEq,
                Token::IntLit(10),
                Token::And,
                Token::Ident("b".to_string()),
                Token::NotEq,
                Token::NullLit,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn tokenize_variable_assignment() {
        assert_eq!(
            tokens("x = 42"),
            vec![
                Token::Ident("x".to_string()),
                Token::Eq,
                Token::IntLit(42),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn tokenize_float() {
        assert_eq!(tokens("3.14"), vec![Token::FloatLit(3.14), Token::Eof]);
    }

    #[test]
    fn line_comment_is_skipped_to_end_of_line() {
        // A trailing comment is dropped; the newline still separates statements.
        assert_eq!(
            tokens("x = 1 # assign\ny = 2 # use"),
            vec![
                Token::Ident("x".to_string()),
                Token::Eq,
                Token::IntLit(1),
                Token::Ident("y".to_string()),
                Token::Eq,
                Token::IntLit(2),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn whole_line_comment_yields_no_tokens() {
        assert_eq!(tokens("# just a comment"), vec![Token::Eof]);
        // Comment at EOF with no trailing newline.
        assert_eq!(
            tokens("a # trailing"),
            vec![Token::Ident("a".to_string()), Token::Eof]
        );
    }

    #[test]
    fn hash_inside_string_literal_is_not_a_comment() {
        // `#` within a raw or double-quoted string is a literal character.
        assert_eq!(
            tokens("'a # b'"),
            vec![Token::RawStringLit("a # b".to_string()), Token::Eof]
        );
        assert_eq!(
            tokens("\"color: #fff\""),
            vec![
                Token::StringLit(vec![StringPart::Literal("color: #fff".to_string())]),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn tokenize_bitwise_operators() {
        assert_eq!(
            tokens("a & b | c ^ d"),
            vec![
                Token::Ident("a".to_string()),
                Token::Ampersand,
                Token::Ident("b".to_string()),
                Token::Pipe,
                Token::Ident("c".to_string()),
                Token::Caret,
                Token::Ident("d".to_string()),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn tokenize_shift_operators() {
        assert_eq!(
            tokens("x << 2 >> 1"),
            vec![
                Token::Ident("x".to_string()),
                Token::ShiftLeft,
                Token::IntLit(2),
                Token::ShiftRight,
                Token::IntLit(1),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn tokenize_tilde() {
        assert_eq!(
            tokens("~x"),
            vec![Token::Tilde, Token::Ident("x".to_string()), Token::Eof,]
        );
    }

    #[test]
    fn tokenize_hex_literals() {
        assert_eq!(tokens("0xFF"), vec![Token::IntLit(255), Token::Eof,]);
        assert_eq!(tokens("0x0F"), vec![Token::IntLit(15), Token::Eof,]);
    }

    #[test]
    fn tokenize_human_duration_literals() {
        use std::time::Duration;
        assert_eq!(
            tokens("10s"),
            vec![Token::DurationLit(Duration::from_secs(10)), Token::Eof]
        );
        assert_eq!(
            tokens("500ms"),
            vec![Token::DurationLit(Duration::from_millis(500)), Token::Eof]
        );
        assert_eq!(
            tokens("1h30m"),
            vec![Token::DurationLit(Duration::from_secs(5400)), Token::Eof]
        );
    }

    #[test]
    fn tokenize_iso_duration_literals() {
        use std::time::Duration;
        assert_eq!(
            tokens("PT1H30M"),
            vec![Token::DurationLit(Duration::from_secs(5400)), Token::Eof]
        );
        assert_eq!(
            tokens("P1DT2H"),
            vec![Token::DurationLit(Duration::from_secs(93_600)), Token::Eof]
        );
    }

    #[test]
    fn duration_does_not_swallow_numbers_or_identifiers() {
        // Bare numbers stay numbers; hex stays hex.
        assert_eq!(tokens("10"), vec![Token::IntLit(10), Token::Eof]);
        assert_eq!(tokens("3.14"), vec![Token::FloatLit(3.14), Token::Eof]);
        assert_eq!(tokens("0xFF"), vec![Token::IntLit(255), Token::Eof]);
        // `P`-words that are not ISO durations remain identifiers.
        assert_eq!(
            tokens("Price"),
            vec![Token::Ident("Price".to_string()), Token::Eof]
        );
        assert_eq!(
            tokens("Point"),
            vec![Token::Ident("Point".to_string()), Token::Eof]
        );
        // Arithmetic with a spaced unit-like identifier is unaffected.
        assert_eq!(
            tokens("10 + 5"),
            vec![Token::IntLit(10), Token::Plus, Token::IntLit(5), Token::Eof]
        );
    }

    #[test]
    fn duration_fallback_when_candidate_is_not_a_valid_duration() {
        // `interval::parse` rejects zero, so `0s` is NOT a duration — it falls
        // through to numeric+identifier lexing (load-bearing: zero ttl is
        // meaningless workspace-wide).
        assert_eq!(
            tokens("0s"),
            vec![Token::IntLit(0), Token::Ident("s".to_string()), Token::Eof]
        );
        // Unknown unit → fall through cleanly, no panic.
        assert_eq!(
            tokens("10x"),
            vec![Token::IntLit(10), Token::Ident("x".to_string()), Token::Eof]
        );
    }

    #[test]
    fn duration_accepts_uppercase_unit() {
        use std::time::Duration;
        // `interval::parse` lowercases units, so `10S` lexes as a duration.
        assert_eq!(
            tokens("10S"),
            vec![Token::DurationLit(Duration::from_secs(10)), Token::Eof]
        );
    }

    #[test]
    fn duration_literal_mid_expression() {
        use std::time::Duration;
        // Guards against any `self.pos == 0` assumption in the duration scan.
        assert_eq!(
            tokens("1h + 30m"),
            vec![
                Token::DurationLit(Duration::from_secs(3600)),
                Token::Plus,
                Token::DurationLit(Duration::from_secs(1800)),
                Token::Eof
            ]
        );
    }

    #[test]
    fn tokenize_line_tracking() {
        let mut lexer = Lexer::new("a\nb");
        let spanned = lexer.tokenize().unwrap();
        assert_eq!(spanned[0].line, 1); // a
        assert_eq!(spanned[1].line, 2); // b
        assert_eq!(spanned[2].line, 2); // Eof
    }

    #[test]
    fn shift_vs_comparison() {
        // << is shift, <= is comparison
        assert_eq!(
            tokens("a << b <= c"),
            vec![
                Token::Ident("a".to_string()),
                Token::ShiftLeft,
                Token::Ident("b".to_string()),
                Token::LtEq,
                Token::Ident("c".to_string()),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn logical_vs_bitwise_and_or() {
        assert_eq!(
            tokens("a && b & c || d | e"),
            vec![
                Token::Ident("a".to_string()),
                Token::And,
                Token::Ident("b".to_string()),
                Token::Ampersand,
                Token::Ident("c".to_string()),
                Token::Or,
                Token::Ident("d".to_string()),
                Token::Pipe,
                Token::Ident("e".to_string()),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn tokenize_power_operator() {
        assert_eq!(
            tokens("2 ** 3"),
            vec![Token::IntLit(2), Token::Power, Token::IntLit(3), Token::Eof,]
        );
    }

    #[test]
    fn tokenize_field_literal_simple() {
        assert_eq!(
            tokens("`id`"),
            vec![Token::FieldLit("id".to_string()), Token::Eof,]
        );
    }

    #[test]
    fn tokenize_field_literal_with_surrounding_tokens() {
        assert_eq!(
            tokens("`a` + `b`"),
            vec![
                Token::FieldLit("a".to_string()),
                Token::Plus,
                Token::FieldLit("b".to_string()),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn tokenize_field_literal_unterminated_errors() {
        let mut lexer = Lexer::new("`id");
        let result = lexer.tokenize();
        assert!(matches!(
            result,
            Err(ExprError::UnterminatedString { position: 0 })
        ));
    }

    #[test]
    fn tokenize_if_else_keywords() {
        assert_eq!(
            tokens("if (a) 1 else 2"),
            vec![
                Token::If,
                Token::LParen,
                Token::Ident("a".to_string()),
                Token::RParen,
                Token::IntLit(1),
                Token::Else,
                Token::IntLit(2),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn keyword_prefixed_identifiers_stay_identifiers() {
        // Only the exact words `if` / `else` are keywords.
        for name in ["iffy", "elsewhere", "if_x", "_if"] {
            assert_eq!(
                tokens(name),
                vec![Token::Ident(name.to_string()), Token::Eof],
                "{name} must lex as a plain identifier"
            );
        }
    }

    #[test]
    fn tokenize_star_vs_power() {
        assert_eq!(
            tokens("a * b ** c"),
            vec![
                Token::Ident("a".to_string()),
                Token::Star,
                Token::Ident("b".to_string()),
                Token::Power,
                Token::Ident("c".to_string()),
                Token::Eof,
            ]
        );
    }
}
