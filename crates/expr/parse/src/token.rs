/// Token types produced by the lexer.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Literals
    IntLit(i64),
    FloatLit(f64),
    StringLit(Vec<StringPart>),
    RawStringLit(String),
    /// Backtick-delimited field literal `` `name` `` — the raw inner text.
    FieldLit(String),
    BoolLit(bool),
    NullLit,
    /// Duration literal — compact human form (`10s`, `1h30m`, `500ms`) or
    /// ISO-8601 (`PT1H30M`). Parsed by the workspace-canonical
    /// `air_elt_commons::interval::parse`.
    DurationLit(std::time::Duration),

    // Identifiers
    Ident(String),

    // Keywords (reserved — never lexed as identifiers)
    If,
    Else,

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    And,
    Or,
    Not,

    // Power operator
    Power, // **

    // Bitwise operators
    Ampersand,  // & (bitwise AND)
    Pipe,       // | (bitwise OR)
    Caret,      // ^ (bitwise XOR)
    Tilde,      // ~ (bitwise NOT, unary)
    ShiftLeft,  // <<
    ShiftRight, // >>

    // Delimiters
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Eq, // = (for assignment and object literals)
    Semicolon,

    // End
    Eof,
}

/// A segment of a double-quoted string (which may contain interpolations).
#[derive(Debug, Clone, PartialEq)]
pub enum StringPart {
    /// Literal text content.
    Literal(String),
    /// An interpolated expression `{expr}` — stored as raw source to be parsed later.
    Expr(String),
}

/// A token paired with the line number where it appeared (1-based).
#[derive(Debug, Clone, PartialEq)]
pub struct SpannedToken {
    pub token: Token,
    pub line: u32,
}
