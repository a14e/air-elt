use crate::error::ExprError;
use crate::token::{SpannedToken, StringPart, Token};

/// Hand-rolled lexer for the expression language.
/// Supports double-quoted strings with interpolation and single-quoted raw strings.
/// Tracks line numbers for newline-aware statement separation.
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

    pub fn tokenize(&mut self) -> Result<Vec<SpannedToken>, ExprError> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace();
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

    fn skip_whitespace(&mut self) {
        while self.pos < self.input.len() {
            let b = self.input.as_bytes()[self.pos];
            if b == b'\n' {
                self.line += 1;
                self.pos += 1;
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
            b',' => {
                self.pos += 1;
                Ok(Token::Comma)
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
            b'0' => self.read_number_or_hex(),
            b'1'..=b'9' => self.read_number(),
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
        self.pos += 1; // skip opening "
        let mut parts: Vec<StringPart> = Vec::new();
        let mut current_literal = String::new();

        while self.pos < self.input.len() {
            let b = self.input.as_bytes()[self.pos];
            match b {
                b'"' => {
                    self.pos += 1;
                    if !current_literal.is_empty() {
                        parts.push(StringPart::Literal(current_literal));
                    }
                    return Ok(Token::StringLit(parts));
                }
                b'\\' => {
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
                        self.pos += 1; // skip {
                        let expr_source = self.read_interpolation_body(start)?;
                        parts.push(StringPart::Expr(expr_source));
                    }
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
        Err(ExprError::UnterminatedString { position: start })
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
            _ => Token::Ident(text.to_string()),
        };
        Ok(token)
    }
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
