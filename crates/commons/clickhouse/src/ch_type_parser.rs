//! Parser for ClickHouse type strings as they appear in
//! `system.columns.type`.
//!
//! The CH type grammar is recursive: `Nullable(LowCardinality(String))`,
//! `Array(Tuple(String, Int32))`, `Map(String, Array(UInt64))`,
//! `AggregateFunction(quantilesTDigest(0.5, 0.99), Float64)`, etc.
//!
//! Mapping policy:
//!
//! * `Nullable(T)` — strips off; the [`ParsedType::nullable`] flag
//!   carries it. Bubbles up to `Field.nullable` on the canonical
//!   schema.
//! * `LowCardinality(T)` — transparent storage hint; we strip it and
//!   parse `T`. Round-tripping LC↔non-LC happens server-side on INSERT.
//! * `Array(T)` / `Tuple(...)` / `Map(K, V)` / `Nested(...)` — mapped
//!   onto [`DataType::Custom`] carriers ([`ChArrayType`], [`ChMapType`],
//!   [`ChTupleType`]). Each preserves the inner type(s) for faithful
//!   RowBinary encoding. The JSON pivot (cross-canonical conversion)
//!   integrates these with the existing mapping matrix.
//! * `AggregateFunction(fn, args)` / `SimpleAggregateFunction(fn, args)`
//!   — mapped to [`DataType::Custom`] carrying [`ChAggregateStateType`]
//!   from the [`crate::types::aggregate_state`] module. Opaque binary
//!   state; CH↔CH only.
//! * `IPv4` / `IPv6` — canonical CH custom types; convert to/from `Text`.
//! * `FixedString(N)` — custom type; convert to/from `Bytes(N)`.
//! * `Enum8(...)` / `Enum16(...)` — custom type; convert to/from `Text`.
//! * `DateTime` — canonical [`DataType::Timestamp`] (`u32` seconds in
//!   RowBinary). Timezone qualifier is parsed and discarded — CH
//!   stores DateTime internally as UTC; the tz is presentation-only.
//! * `Date32` / `DateTime64(N[, 'tz'])` — **rejected**: these are 4-
//!   and 8-byte fixed-width columns in RowBinary, but our canonical
//!   `Date` / `Timestamp` pivots encode as 2 and 4 bytes respectively.
//!   Letting them collapse would silently misalign every subsequent
//!   column in the row. Proper support needs an i32-days Date32
//!   encoder and an i64-ticks DateTime64 encoder (with precision
//!   metadata).
//! * `Decimal(P, S)` / `Decimal32` / `Decimal64` / `Decimal128` /
//!   `Decimal256` — `Decimal { precision, scale }`.
//! * Numeric, `String`, `UUID`, `Date`, `Date32`, `Bool`, `JSON`,
//!   `Object('json')` — canonical pivots.
//! * `Point` / `Ring` / `Polygon` / `MultiPolygon` — **rejected**: these
//!   have specific Tuple/Array-based RowBinary layouts, not text JSON.

use thiserror::Error;

use air_elt_core::types::data_type::DataType;

use crate::types::aggregate_state::ChAggregateStateType;
use crate::types::array::ChArrayType;
use crate::types::enums::{ChEnum8Type, ChEnum16Type};
use crate::types::fixed_string::ChFixedStringType;
use crate::types::int128::{ChInt128Type, ChUInt128Type};
use crate::types::int256::{ChInt256Type, ChUInt256Type};
use crate::types::map::ChMapType;
use crate::types::tuple::ChTupleType;

/// `(DataType, nullable)`. Used throughout the parser for propagating
/// nullability through composite type arguments.
type TypeAndNull = (DataType, bool);

/// Parsed shape of a CH column type string.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedType {
    pub data_type: DataType,
    pub nullable: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("empty type string")]
    Empty,
    #[error("unexpected token at position {pos}: {found:?}")]
    UnexpectedToken { pos: usize, found: String },
    #[error("unterminated parenthesis (started at position {start})")]
    UnterminatedParen { start: usize },
    #[error("unsupported type: {0}")]
    Unsupported(String),
    #[error("invalid argument for {ctor}: {reason}")]
    InvalidArg { ctor: String, reason: String },
}

/// Parse a CH `system.columns.type` string into a canonical
/// [`ParsedType`].
pub fn parse_type(input: &str) -> Result<ParsedType, ParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ParseError::Empty);
    }
    let mut parser = Parser::new(trimmed);
    let (dt, nullable) = parser.parse_outer()?;
    parser.expect_eof()?;
    Ok(ParsedType {
        data_type: dt,
        nullable,
    })
}

const MAX_PARSE_DEPTH: usize = 64;

struct Parser<'a> {
    input: &'a str,
    pos: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            pos: 0,
            depth: 0,
        }
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        if self.depth >= MAX_PARSE_DEPTH {
            return Err(ParseError::Unsupported("type nesting too deep".to_string()));
        }
        self.depth += 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn rest(&self) -> &'a str {
        &self.input[self.pos..]
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.rest().chars().next() {
            if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self, ch: char) -> bool {
        if self.peek() == Some(ch) {
            self.pos += ch.len_utf8();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, ch: char) -> Result<(), ParseError> {
        if self.bump(ch) {
            Ok(())
        } else {
            Err(ParseError::UnexpectedToken {
                pos: self.pos,
                found: self.peek().map(|c| c.to_string()).unwrap_or_default(),
            })
        }
    }

    fn read_ident(&mut self) -> String {
        self.skip_ws();
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
        self.input[start..self.pos].to_string()
    }

    fn expect_eof(&mut self) -> Result<(), ParseError> {
        self.skip_ws();
        if self.pos != self.input.len() {
            Err(ParseError::UnexpectedToken {
                pos: self.pos,
                found: self.rest().to_string(),
            })
        } else {
            Ok(())
        }
    }

    /// Parse the outermost type, unwrapping Nullable and LowCardinality.
    fn parse_outer(&mut self) -> Result<(DataType, bool), ParseError> {
        self.enter()?;
        self.skip_ws();
        let snapshot = self.pos;
        let name = self.read_ident();
        let result = match name.as_str() {
            "Nullable" => {
                self.expect('(')?;
                let (inner, _) = self.parse_outer()?;
                self.skip_ws();
                self.expect(')')?;
                Ok((inner, true))
            }
            "LowCardinality" => {
                self.expect('(')?;
                let (inner, nullable) = self.parse_outer()?;
                self.skip_ws();
                self.expect(')')?;
                Ok((inner, nullable))
            }
            _ => {
                // Restart by re-parsing the leaf-or-composite at this
                // position. We've already consumed `name`; finish the
                // leaf based on it.
                self.pos = snapshot;
                let dt = self.parse_named()?;
                Ok((dt, false))
            }
        };
        self.leave();
        result
    }

    /// Parse a single (possibly composite) named type at the current
    /// position. The caller is responsible for `Nullable` / `LowCardinality`
    /// unwrapping.
    fn parse_named(&mut self) -> Result<DataType, ParseError> {
        self.skip_ws();
        let name = self.read_ident();
        if name.is_empty() {
            return Err(ParseError::UnexpectedToken {
                pos: self.pos,
                found: self.rest().to_string(),
            });
        }
        self.skip_ws();
        let has_args = self.peek() == Some('(');
        match name.as_str() {
            "String" => Ok(DataType::Text { size: None }),
            "UUID" => Ok(DataType::Uuid),
            "Bool" | "Boolean" => Ok(DataType::Bool),
            "Date" => Ok(DataType::Date),
            // `Date32` stores `i32` days (range 1900..=2299), but our
            // RowBinary encoder treats `DataType::Date` as `u16` days.
            // Letting `Date32` collapse to `Date` would write 2 bytes
            // where the column expects 4 — silently misaligning every
            // subsequent column in the row. Reject up front; operators
            // re-declare the column as `Date` if the 1970..=2149 range
            // is enough, otherwise this needs a proper Date32 encoder.
            "Date32" => Err(ParseError::Unsupported(name)),
            "DateTime" => {
                if has_args {
                    self.skip_balanced_parens()?;
                }
                Ok(DataType::Timestamp)
            }
            // `DateTime64(N, [tz])` stores `i64` ticks at precision
            // `10^-N` seconds. Mapping it onto the canonical `Timestamp`
            // (encoded as `u32` seconds) would lose sub-second precision
            // AND write 4 bytes where the column expects 8 — same wire-
            // format misalignment story as Date32. Reject explicitly;
            // proper support needs an i64-ticks encoder with precision
            // metadata.
            "DateTime64" => Err(ParseError::Unsupported(name)),
            // CH `Int8` maps to canonical `DataType::Int8` (signed 1-byte).
            // The RowBinary encoder writes 1 byte via `i8 as u8`
            // (two's-complement bit-cast), which is exactly what CH expects.
            "Int8" => Ok(DataType::Int8),
            "Int16" => Ok(DataType::Int16),
            "Int32" => Ok(DataType::Int32),
            "Int64" => Ok(DataType::Int64),
            "Int128" => Ok(DataType::Custom(Box::new(ChInt128Type))),
            "Int256" => Ok(DataType::Custom(Box::new(ChInt256Type))),
            "UInt8" => Ok(DataType::UInt8),
            "UInt16" => Ok(DataType::UInt16),
            "UInt32" => Ok(DataType::UInt32),
            "UInt64" => Ok(DataType::UInt64),
            "UInt128" => Ok(DataType::Custom(Box::new(ChUInt128Type))),
            "UInt256" => Ok(DataType::Custom(Box::new(ChUInt256Type))),
            "Float32" => Ok(DataType::Float32),
            "Float64" => Ok(DataType::Float64),
            "JSON" | "Object" => {
                if has_args {
                    self.skip_balanced_parens()?;
                }
                Ok(DataType::Json)
            }
            "IPv4" => Ok(DataType::Ipv4),
            "IPv6" => Ok(DataType::Ipv6),
            "Decimal32" => {
                let scale = self.parse_single_uint_arg("Decimal32")?;
                Ok(DataType::Decimal {
                    precision: Some(9),
                    scale: Some(scale),
                })
            }
            "Decimal64" => {
                let scale = self.parse_single_uint_arg("Decimal64")?;
                Ok(DataType::Decimal {
                    precision: Some(18),
                    scale: Some(scale),
                })
            }
            "Decimal128" => {
                let scale = self.parse_single_uint_arg("Decimal128")?;
                Ok(DataType::Decimal {
                    precision: Some(38),
                    scale: Some(scale),
                })
            }
            "Decimal256" => {
                let scale = self.parse_single_uint_arg("Decimal256")?;
                Ok(DataType::Decimal {
                    precision: Some(76),
                    scale: Some(scale),
                })
            }
            "Decimal" => {
                let (p, s) = self.parse_two_uint_args("Decimal")?;
                Ok(DataType::Decimal {
                    precision: Some(p),
                    scale: Some(s),
                })
            }
            "FixedString" => {
                let n = self.parse_single_uint_arg("FixedString")?;
                Ok(DataType::Custom(Box::new(ChFixedStringType { size: n })))
            }
            "Enum8" => {
                let variants = self.parse_enum_variants_i8("Enum8")?;
                Ok(DataType::Custom(Box::new(ChEnum8Type { variants })))
            }
            "Enum16" => {
                let variants = self.parse_enum_variants_i16("Enum16")?;
                Ok(DataType::Custom(Box::new(ChEnum16Type { variants })))
            }
            "AggregateFunction" | "SimpleAggregateFunction" => {
                let (fn_name, args) = self.parse_aggregate_args(&name)?;
                let kind = ChAggregateStateType::kind_for_fn(&fn_name);
                Ok(DataType::Custom(Box::new(ChAggregateStateType {
                    fn_name,
                    arg_types: args,
                    simple: name == "SimpleAggregateFunction",
                    kind,
                })))
            }
            // `Array(<primitive>)` / `Array(Nullable(<primitive>))` map onto
            // the canonical `DataType::Array`, so the value travels as a
            // `Value::Array` through the conversion matrix and the RowBinary
            // encoder. Non-primitive element types (Tuple / Nested /
            // Array-of-Array / Map / aggregate / CH custom scalars) keep the
            // `Custom(ChArrayType)` carrier with its JSON pivot.
            "Array" => {
                let (inner, nullable) = self.parse_single_type_arg("Array")?;
                if is_canonical_array_element(&inner) {
                    Ok(DataType::Array {
                        element: Some(Box::new(inner)),
                        element_nullable: nullable,
                    })
                } else {
                    Ok(DataType::Custom(Box::new(ChArrayType {
                        element: inner,
                        element_nullable: nullable,
                    })))
                }
            }
            "Map" => {
                let ((key, key_nullable), (value, value_nullable)) =
                    self.parse_two_type_args("Map")?;
                Ok(DataType::Custom(Box::new(ChMapType {
                    key,
                    value,
                    key_nullable,
                    value_nullable,
                })))
            }
            "Tuple" => {
                let fields = self.parse_type_list("Tuple")?;
                Ok(DataType::Custom(Box::new(ChTupleType { fields })))
            }
            // Nested(name1 Type1, ...) is CH syntactic sugar for
            // Array(Tuple(name1 Type1, ...)).  The column names inside
            // a Nested declaration are presentation-only — RowBinary
            // encodes the same wire layout as Array(Tuple(...)).
            "Nested" => {
                let fields = self.parse_nested_fields()?;
                let tuple = DataType::Custom(Box::new(ChTupleType { fields }));
                Ok(DataType::Custom(Box::new(ChArrayType {
                    element: tuple,
                    element_nullable: false,
                })))
            }
            // Geo types are tuples/arrays under the hood with specific
            // RowBinary layouts (e.g. Point = Tuple(Float64, Float64)).
            // Text-encoded JSON is not wire-compatible — reject until
            // proper binary layout support is added.
            "Point" | "Ring" | "Polygon" | "MultiPolygon" | "LineString" | "MultiLineString" => {
                Err(ParseError::Unsupported(name))
            }
            // Generic catch-all for parametric types we don't model:
            // skip args if present and reject.
            other => Err(ParseError::Unsupported(other.to_string())),
        }
    }

    /// Consume balanced parens, ignoring quoted strings.
    fn skip_balanced_parens(&mut self) -> Result<(), ParseError> {
        self.skip_ws();
        let start = self.pos;
        if !self.bump('(') {
            return Err(ParseError::UnexpectedToken {
                pos: self.pos,
                found: self.peek().map(|c| c.to_string()).unwrap_or_default(),
            });
        }
        let mut depth: usize = 1;
        let mut in_str = false;
        while depth > 0 {
            let Some(c) = self.peek() else {
                return Err(ParseError::UnterminatedParen { start });
            };
            if in_str {
                if c == '\'' {
                    in_str = false;
                }
                self.pos += c.len_utf8();
                continue;
            }
            match c {
                '\'' => in_str = true,
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            self.pos += c.len_utf8();
        }
        Ok(())
    }

    /// Parse `(Type)` — consume `(`, parse the inner type recursively
    /// via `parse_outer` (so `Nullable`/`LowCardinality` wrappers on
    /// the element type are handled), then consume `)`. Returns the
    /// inner type with its nullability flag.
    fn parse_single_type_arg(&mut self, _ctor: &str) -> Result<(DataType, bool), ParseError> {
        self.skip_ws();
        self.expect('(')?;
        let (inner, nullable) = self.parse_outer()?;
        self.skip_ws();
        self.expect(')')?;
        Ok((inner, nullable))
    }

    /// Parse `(Type1, Type2)`. Returns each type with its nullability flag.
    fn parse_two_type_args(
        &mut self,
        _ctor: &str,
    ) -> Result<(TypeAndNull, TypeAndNull), ParseError> {
        self.skip_ws();
        self.expect('(')?;
        let first = self.parse_outer()?;
        self.skip_ws();
        self.expect(',')?;
        let second = self.parse_outer()?;
        self.skip_ws();
        self.expect(')')?;
        Ok((first, second))
    }

    /// Parse `(Type1, Type2, ...)` — comma-separated type list.
    /// Returns each type with its nullability flag.
    fn parse_type_list(&mut self, _ctor: &str) -> Result<Vec<(DataType, bool)>, ParseError> {
        self.skip_ws();
        self.expect('(')?;
        let mut types: Vec<(DataType, bool)> = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(')') {
                self.bump(')');
                break;
            }
            if !types.is_empty() {
                self.expect(',')?;
                self.skip_ws();
            }
            // CH Tuple fields may have optional names:
            //   Tuple(Int32, String)          — unnamed
            //   Tuple(a Int32, b String)      — named
            // Strategy: save position, read the first identifier, then
            // peek at what follows.
            let saved = self.pos;
            let _first = self.read_ident();
            self.skip_ws();
            let first_is_field_name = match self.peek() {
                // Followed by another ident starter → `first` was a
                // field name, the actual type follows. Note: `(` is
                // NOT included — `Nullable(`, `Array(`, etc. are
                // type names with arguments, not field names.
                Some(c) if c.is_ascii_alphanumeric() || c == '_' => true,
                _ => false,
            };
            if first_is_field_name {
                // Discard `first` (field name), parse the type.
                let (dt, nullable) = self.parse_outer()?;
                types.push((dt, nullable));
            } else {
                // `first` is a type name (not a field name). Restore
                // and parse via parse_outer so that `Nullable(...)`,
                // `LowCardinality(...)`, etc. are properly unwrapped.
                self.pos = saved;
                let (dt, nullable) = self.parse_outer()?;
                types.push((dt, nullable));
            }
        }
        Ok(types)
    }

    /// Parse Nested field declarations: `(name1 Type1, name2 Type2, ...)`.
    /// Returns field types with nullability — field names are discarded
    /// because `Nested` is sugar for `Array(Tuple(...))` and the names
    /// are presentation-only.
    fn parse_nested_fields(&mut self) -> Result<Vec<(DataType, bool)>, ParseError> {
        self.skip_ws();
        self.expect('(')?;
        let mut types: Vec<(DataType, bool)> = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(')') {
                self.bump(')');
                break;
            }
            if !types.is_empty() {
                self.expect(',')?;
                self.skip_ws();
            }
            // Consume the field name.
            let _name = self.read_ident();
            self.skip_ws();
            // Parse the field type with nullability.
            let (dt, nullable) = self.parse_outer()?;
            types.push((dt, nullable));
        }
        Ok(types)
    }

    fn parse_single_uint_arg(&mut self, ctor: &str) -> Result<u32, ParseError> {
        self.skip_ws();
        self.expect('(')?;
        self.skip_ws();
        let n = self.read_uint(ctor)?;
        self.skip_ws();
        self.expect(')')?;
        Ok(n)
    }

    fn parse_two_uint_args(&mut self, ctor: &str) -> Result<(u32, u32), ParseError> {
        self.skip_ws();
        self.expect('(')?;
        self.skip_ws();
        let a = self.read_uint(ctor)?;
        self.skip_ws();
        self.expect(',')?;
        self.skip_ws();
        let b = self.read_uint(ctor)?;
        self.skip_ws();
        self.expect(')')?;
        Ok((a, b))
    }

    fn read_uint(&mut self, ctor: &str) -> Result<u32, ParseError> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
        if self.pos == start {
            return Err(ParseError::InvalidArg {
                ctor: ctor.to_string(),
                reason: "expected unsigned integer".to_string(),
            });
        }
        self.input[start..self.pos]
            .parse::<u32>()
            .map_err(|e| ParseError::InvalidArg {
                ctor: ctor.to_string(),
                reason: e.to_string(),
            })
    }

    fn read_quoted_string(&mut self) -> Result<String, ParseError> {
        self.skip_ws();
        self.expect('\'')?;
        let start = self.pos;
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c == '\'' {
                self.pos += 1;
                return Ok(out);
            }
            if c == '\\' {
                // CH escape sequences inside single-quoted literals.
                self.pos += c.len_utf8();
                let esc = self.peek().ok_or(ParseError::UnterminatedParen { start })?;
                let decoded = match esc {
                    '\'' => '\'',
                    '\\' => '\\',
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    'b' => '\u{08}',
                    'f' => '\u{0C}',
                    '0' => '\0',
                    'a' => '\u{07}',
                    'v' => '\u{0B}',
                    // Unknown escape — pass through verbatim (CH's behaviour
                    // for unrecognised backslash sequences in identifiers).
                    other => other,
                };
                out.push(decoded);
                self.pos += esc.len_utf8();
            } else {
                out.push(c);
                self.pos += c.len_utf8();
            }
        }
        Err(ParseError::UnterminatedParen { start })
    }

    fn read_signed_int(&mut self) -> Result<i64, ParseError> {
        self.skip_ws();
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.pos += 1;
            } else {
                break;
            }
        }
        self.input[start..self.pos]
            .parse::<i64>()
            .map_err(|e| ParseError::InvalidArg {
                ctor: "Enum".to_string(),
                reason: e.to_string(),
            })
    }

    fn parse_enum_variants_i8(&mut self, ctor: &str) -> Result<Vec<(String, i8)>, ParseError> {
        self.skip_ws();
        self.expect('(')?;
        let mut out = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(')') {
                self.pos += 1;
                break;
            }
            let name = self.read_quoted_string()?;
            self.skip_ws();
            self.expect('=')?;
            let v = self.read_signed_int()?;
            let v8 = i8::try_from(v).map_err(|_| ParseError::InvalidArg {
                ctor: ctor.to_string(),
                reason: format!("value {v} out of i8 range"),
            })?;
            out.push((name, v8));
            self.skip_ws();
            if self.peek() == Some(',') {
                self.pos += 1;
            }
        }
        Ok(out)
    }

    fn parse_enum_variants_i16(&mut self, ctor: &str) -> Result<Vec<(String, i16)>, ParseError> {
        self.skip_ws();
        self.expect('(')?;
        let mut out = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(')') {
                self.pos += 1;
                break;
            }
            let name = self.read_quoted_string()?;
            self.skip_ws();
            self.expect('=')?;
            let v = self.read_signed_int()?;
            let v16 = i16::try_from(v).map_err(|_| ParseError::InvalidArg {
                ctor: ctor.to_string(),
                reason: format!("value {v} out of i16 range"),
            })?;
            out.push((name, v16));
            self.skip_ws();
            if self.peek() == Some(',') {
                self.pos += 1;
            }
        }
        Ok(out)
    }

    fn parse_aggregate_args(&mut self, _ctor: &str) -> Result<(String, Vec<String>), ParseError> {
        self.skip_ws();
        self.expect('(')?;
        // First arg: function name + optional parameter list, e.g.
        // `quantilesTDigest(0.5, 0.99)`. We extract the function name
        // and consume the parameter list opaquely (CH stores params in
        // the type string but they don't change the binary state shape
        // for parsing purposes).
        self.skip_ws();
        let fn_name = self.read_ident();
        self.skip_ws();
        if self.peek() == Some('(') {
            self.skip_balanced_parens()?;
        }
        // Remaining args: comma-separated type strings.
        let mut args: Vec<String> = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(')') {
                self.pos += 1;
                break;
            }
            self.expect(',')?;
            self.skip_ws();
            let arg = self.consume_one_arg()?;
            args.push(arg);
        }
        Ok((fn_name, args))
    }

    /// Consume one argument token up to the next comma at depth 0 or the
    /// closing paren. Returns the raw substring.
    fn consume_one_arg(&mut self) -> Result<String, ParseError> {
        let start = self.pos;
        let mut depth: usize = 0;
        let mut in_str = false;
        while let Some(c) = self.peek() {
            if in_str {
                if c == '\'' {
                    in_str = false;
                }
                self.pos += c.len_utf8();
                continue;
            }
            match c {
                '\'' => in_str = true,
                '(' => depth += 1,
                ')' if depth == 0 => break,
                ')' => depth -= 1,
                ',' if depth == 0 => break,
                _ => {}
            }
            self.pos += c.len_utf8();
        }
        Ok(self.input[start..self.pos].trim().to_string())
    }
}

/// Whether a parsed `Array` element type is a canonical scalar that the
/// conversion matrix and the RowBinary encoder handle directly as a
/// `Value::Array` member. Composite shapes (`Array` / `Tuple` / `Map`) and
/// CH custom carriers (Int128, FixedString, Enum, aggregate state, …) stay
/// on the `Custom(ChArrayType)` path with its JSON pivot, so they are
/// excluded here.
fn is_canonical_array_element(element: &DataType) -> bool {
    matches!(
        element,
        DataType::Bool
            | DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::Float32
            | DataType::Float64
            | DataType::Text { .. }
            | DataType::Bytes { .. }
            | DataType::Date
            | DataType::Timestamp
            | DataType::Uuid
            | DataType::Ipv4
            | DataType::Ipv6
            | DataType::Decimal { .. }
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn parse(s: &str) -> ParsedType {
        parse_type(s).unwrap_or_else(|e| panic!("failed to parse {s:?}: {e}"))
    }

    #[test]
    fn parses_primitives() {
        assert_eq!(parse("String").data_type, DataType::Text { size: None });
        assert_eq!(parse("UInt64").data_type, DataType::UInt64);
        assert_eq!(parse("Int8").data_type, DataType::Int8);
        assert_eq!(parse("Int32").data_type, DataType::Int32);
        assert_eq!(parse("Float64").data_type, DataType::Float64);
        assert_eq!(parse("Bool").data_type, DataType::Bool);
        assert_eq!(parse("UUID").data_type, DataType::Uuid);
        assert_eq!(parse("Date").data_type, DataType::Date);
        assert_eq!(parse("DateTime").data_type, DataType::Timestamp);
        assert_eq!(
            parse("DateTime('Europe/Moscow')").data_type,
            DataType::Timestamp
        );
        // `Date32` / `DateTime64` are intentionally rejected — see the
        // parser body for the wire-format misalignment rationale.
        assert!(matches!(
            parse_type("Date32"),
            Err(ParseError::Unsupported(_))
        ));
        assert!(matches!(
            parse_type("DateTime64(3)"),
            Err(ParseError::Unsupported(_))
        ));
        assert!(matches!(
            parse_type("DateTime64(6, 'UTC')"),
            Err(ParseError::Unsupported(_))
        ));
    }

    #[test]
    fn parses_decimal() {
        assert_eq!(
            parse("Decimal(38, 9)").data_type,
            DataType::Decimal {
                precision: Some(38),
                scale: Some(9),
            }
        );
        assert_eq!(
            parse("Decimal64(4)").data_type,
            DataType::Decimal {
                precision: Some(18),
                scale: Some(4),
            }
        );
    }

    #[test]
    fn nullable_strips_and_flags() {
        let p = parse("Nullable(String)");
        assert_eq!(p.data_type, DataType::Text { size: None });
        assert!(p.nullable);
    }

    #[test]
    fn low_cardinality_strips() {
        let p = parse("LowCardinality(String)");
        assert_eq!(p.data_type, DataType::Text { size: None });
        assert!(!p.nullable);
    }

    #[test]
    fn low_cardinality_with_nullable() {
        let p = parse("LowCardinality(Nullable(String))");
        assert_eq!(p.data_type, DataType::Text { size: None });
        assert!(p.nullable);
    }

    #[test]
    fn nullable_with_low_cardinality() {
        let p = parse("Nullable(LowCardinality(String))");
        assert_eq!(p.data_type, DataType::Text { size: None });
        assert!(p.nullable);
    }

    #[test]
    fn ipv4_ipv6() {
        let p = parse("IPv4");
        assert_eq!(p.data_type, DataType::Ipv4);
        let p = parse("IPv6");
        assert_eq!(p.data_type, DataType::Ipv6);
    }

    #[test]
    fn fixed_string() {
        let p = parse("FixedString(16)");
        match &p.data_type {
            DataType::Custom(t) => assert_eq!(t.kind(), "clickhouse.fixed_string"),
            _ => panic!("expected custom"),
        }
    }

    #[test]
    fn enum8_with_variants() {
        let p = parse("Enum8('hello' = 1, 'world' = 2)");
        match &p.data_type {
            DataType::Custom(t) => assert_eq!(t.kind(), "clickhouse.enum8"),
            _ => panic!("expected custom"),
        }
    }

    #[test]
    fn enum_variant_unescapes_apostrophe_and_backslash() {
        // `'it\'s'` → variant name `it's`; `'a\\b'` → `a\b`.
        let p = parse(r"Enum8('it\'s' = 1, 'a\\b' = 2)");
        let custom = match &p.data_type {
            DataType::Custom(t) => t,
            _ => panic!("expected custom"),
        };
        let e8 = custom
            .as_any()
            .downcast_ref::<crate::types::enums::ChEnum8Type>()
            .expect("ChEnum8Type");
        let names: Vec<&str> = e8.variants.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["it's", r"a\b"]);
    }

    #[test]
    fn enum_variant_unescapes_control_chars() {
        let p = parse(r"Enum8('line\nbreak' = 1, 'tab\there' = 2)");
        let custom = match &p.data_type {
            DataType::Custom(t) => t,
            _ => panic!("expected custom"),
        };
        let e8 = custom
            .as_any()
            .downcast_ref::<crate::types::enums::ChEnum8Type>()
            .expect("ChEnum8Type");
        assert_eq!(e8.variants[0].0, "line\nbreak");
        assert_eq!(e8.variants[1].0, "tab\there");
    }

    #[test]
    fn aggregate_function() {
        let p = parse("AggregateFunction(quantilesTDigest(0.5, 0.99), Float64)");
        match &p.data_type {
            DataType::Custom(t) => {
                assert_eq!(t.kind(), "clickhouse.aggregate.quantiles_t_digest");
            }
            _ => panic!("expected custom"),
        }
    }

    #[test]
    fn simple_aggregate_function() {
        let p = parse("SimpleAggregateFunction(sum, UInt64)");
        match &p.data_type {
            DataType::Custom(t) => {
                assert!(t.kind().starts_with("clickhouse.aggregate."));
            }
            _ => panic!("expected custom"),
        }
    }

    #[test]
    fn composites_parse_to_custom_types() {
        // Map(String, Array(UInt64)) → nested custom types
        let map = parse("Map(String, Array(UInt64))").data_type;
        match &map {
            DataType::Custom(t) => {
                assert_eq!(t.kind(), "clickhouse.map");
            }
            _ => panic!("expected Custom, got {map:?}"),
        }

        // Tuple(String, Int32, Float64)
        let tuple = parse("Tuple(String, Int32, Float64)").data_type;
        match &tuple {
            DataType::Custom(t) => {
                assert_eq!(t.kind(), "clickhouse.tuple");
            }
            _ => panic!("expected Custom, got {tuple:?}"),
        }

        // Tuple(a Int32, b String) — named fields are discarded
        let named_tuple = parse("Tuple(a Int32, b String)").data_type;
        match &named_tuple {
            DataType::Custom(t) => {
                assert_eq!(t.kind(), "clickhouse.tuple");
                assert_eq!(t.display(), "Tuple(int32, text)");
            }
            _ => panic!("expected Custom(Tuple), got {named_tuple:?}"),
        }

        // Geo types are rejected — they have specific binary layouts.
        assert!(matches!(
            parse_type("Point"),
            Err(ParseError::Unsupported(_))
        ));
        assert!(matches!(
            parse_type("MultiPolygon"),
            Err(ParseError::Unsupported(_))
        ));

        // Nested(name String, age Int32) → Array(Tuple(name String, age Int32))
        let nested = parse("Nested(name String, age Int32)").data_type;
        match &nested {
            DataType::Custom(t) => {
                assert_eq!(t.kind(), "clickhouse.array");
                assert!(t.display().contains("Tuple"), "Nested should wrap Tuple");
            }
            _ => panic!("expected Custom(Array), got {nested:?}"),
        }
    }

    #[test]
    fn nullable_inside_composites() {
        // Map(String, Nullable(Int32)) — value_nullable = true
        let map = parse("Map(String, Nullable(Int32))").data_type;
        match &map {
            DataType::Custom(t) => {
                assert_eq!(t.kind(), "clickhouse.map");
                let map_ty = t.as_any().downcast_ref::<ChMapType>().unwrap();
                assert!(!map_ty.key_nullable);
                assert!(map_ty.value_nullable);
            }
            _ => panic!("expected Custom(Map), got {map:?}"),
        }

        // Tuple(Nullable(Int32), String) — first field nullable
        let tup = parse("Tuple(Nullable(Int32), String)").data_type;
        match &tup {
            DataType::Custom(t) => {
                assert_eq!(t.kind(), "clickhouse.tuple");
                let tup_ty = t.as_any().downcast_ref::<ChTupleType>().unwrap();
                assert!(tup_ty.fields[0].1, "first field should be nullable");
                assert!(!tup_ty.fields[1].1, "second field should not be nullable");
            }
            _ => panic!("expected Custom(Tuple), got {tup:?}"),
        }

        // Tuple(a Nullable(Int32), b String) — named, first field nullable
        let named_tup = parse("Tuple(a Nullable(Int32), b String)").data_type;
        match &named_tup {
            DataType::Custom(t) => {
                assert_eq!(t.kind(), "clickhouse.tuple");
                let tup_ty = t.as_any().downcast_ref::<ChTupleType>().unwrap();
                assert!(tup_ty.fields[0].1, "named: first field should be nullable");
                assert!(!tup_ty.fields[1].1);
            }
            _ => panic!("expected Custom(Tuple), got {named_tup:?}"),
        }
    }

    #[test]
    fn array_of_primitive_maps_to_canonical() {
        // Array(Int32) → canonical DataType::Array { element: Int32 }.
        let arr = parse("Array(Int32)").data_type;
        assert_eq!(
            arr,
            DataType::Array {
                element: Some(Box::new(DataType::Int32)),
                element_nullable: false,
            }
        );

        // Array(Nullable(String)) → canonical, element_nullable = true.
        let nullable = parse("Array(Nullable(String))").data_type;
        assert_eq!(
            nullable,
            DataType::Array {
                element: Some(Box::new(DataType::Text { size: None })),
                element_nullable: true,
            }
        );

        // Array(String) → canonical String element.
        let strings = parse("Array(String)").data_type;
        assert_eq!(
            strings,
            DataType::Array {
                element: Some(Box::new(DataType::Text { size: None })),
                element_nullable: false,
            }
        );
    }

    #[test]
    fn array_of_non_primitive_keeps_custom_carrier() {
        // Array(Tuple(...)) is not a primitive element — stays on the
        // Custom(ChArrayType) JSON-pivot path.
        let nested = parse("Array(Tuple(Int32, String))").data_type;
        match &nested {
            DataType::Custom(t) => assert_eq!(t.kind(), "clickhouse.array"),
            _ => panic!("expected Custom(Array), got {nested:?}"),
        }

        // Array(Array(Int32)) — element is itself an array, not a scalar.
        let array_of_array = parse("Array(Array(Int32))").data_type;
        match &array_of_array {
            DataType::Custom(t) => assert_eq!(t.kind(), "clickhouse.array"),
            _ => panic!("expected Custom(Array), got {array_of_array:?}"),
        }
    }

    #[test]
    fn rejects_empty() {
        assert!(matches!(parse_type(""), Err(ParseError::Empty)));
    }

    #[test]
    fn rejects_unsupported() {
        // Variant that should still be rejected.
        assert!(matches!(
            parse_type("Time"),
            Err(ParseError::Unsupported(_))
        ));
    }

    #[test]
    fn int128_uint128() {
        let p = parse("Int128");
        match &p.data_type {
            DataType::Custom(t) => assert_eq!(t.kind(), "clickhouse.int128"),
            _ => panic!("expected custom"),
        }
        let p = parse("UInt128");
        match &p.data_type {
            DataType::Custom(t) => assert_eq!(t.kind(), "clickhouse.uint128"),
            _ => panic!("expected custom"),
        }
    }

    #[test]
    fn int256_uint256() {
        let p = parse("Int256");
        match &p.data_type {
            DataType::Custom(t) => assert_eq!(t.kind(), "clickhouse.int256"),
            _ => panic!("expected custom"),
        }
        let p = parse("UInt256");
        match &p.data_type {
            DataType::Custom(t) => assert_eq!(t.kind(), "clickhouse.uint256"),
            _ => panic!("expected custom"),
        }
    }

    #[test]
    fn decimal256() {
        assert_eq!(
            parse("Decimal256(10)").data_type,
            DataType::Decimal {
                precision: Some(76),
                scale: Some(10),
            }
        );
    }
}
