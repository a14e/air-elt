use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{EvalContext, ExprFunction};

static CONCAT: ConcatFunc = ConcatFunc;
static LENGTH: LengthFunc = LengthFunc;
static SUBSTRING: SubstringFunc = SubstringFunc;
static CHAR_AT: CharAtFunc = CharAtFunc;
static UPPER: UpperFunc = UpperFunc;
static LOWER: LowerFunc = LowerFunc;
static TRIM: TrimFunc = TrimFunc;
static REPLACE: ReplaceFunc = ReplaceFunc;
static STARTS_WITH: StartsWithFunc = StartsWithFunc;
static ENDS_WITH: EndsWithFunc = EndsWithFunc;
static CONTAINS: ContainsFunc = ContainsFunc;
static INDEX_OF: IndexOfFunc = IndexOfFunc;
static FORMAT: FormatFunc = FormatFunc;
static TO_STRING: ToStringFunc = ToStringFunc;
static REVERSE: ReverseFunc = ReverseFunc;
static REPEAT: RepeatFunc = RepeatFunc;
static LEFT_PAD: LeftPadFunc = LeftPadFunc;
static RIGHT_PAD: RightPadFunc = RightPadFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&CONCAT);
    registry.register(&LENGTH);
    registry.register(&SUBSTRING);
    registry.register(&CHAR_AT);
    registry.register(&UPPER);
    registry.register(&LOWER);
    registry.register(&TRIM);
    registry.register(&REPLACE);
    registry.register(&STARTS_WITH);
    registry.register(&ENDS_WITH);
    registry.register(&CONTAINS);
    registry.register(&INDEX_OF);
    registry.register(&FORMAT);
    registry.register(&TO_STRING);
    registry.register(&REVERSE);
    registry.register(&REPEAT);
    registry.register(&LEFT_PAD);
    registry.register(&RIGHT_PAD);
}

struct ConcatFunc;

impl ExprFunction for ConcatFunc {
    fn name(&self) -> &str {
        "concat"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        None
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            nullable,
        ))
    }

    fn evaluate(&self, args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let mut iter = args.into_iter();
        let first = iter.next().expect("min_args guarantees at least 1");
        if first.is_null() {
            return Ok(Value::Null);
        }
        let mut result = match first {
            Value::Text(s) => s,
            other => format_value(&other),
        };
        for val in iter {
            if val.is_null() {
                return Ok(Value::Null);
            }
            match val {
                Value::Text(s) => result.push_str(&s),
                other => result.push_str(&format_value(&other)),
            }
        }
        Ok(Value::Text(result))
    }
}

struct LengthFunc;

impl ExprFunction for LengthFunc {
    fn name(&self) -> &str {
        "length"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_text_arg("length", &args[0].data_type)?;
        Ok(NullableExprType::new(DataType::Int64, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Text(s) => Ok(Value::Int64(s.chars().count() as i64)),
            other => Err(FuncError::TypeMismatch {
                function: "length".to_owned(),
                expected: "Text".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

struct SubstringFunc;

impl ExprFunction for SubstringFunc {
    fn name(&self) -> &str {
        "substring"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(3)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_text_arg("substring", &args[0].data_type)?;
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let len_val = if args.len() == 3 {
            Some(args.remove(2))
        } else {
            None
        };
        let start_val = args.remove(1);
        let s_val = args.remove(0);

        if s_val.is_null() || start_val.is_null() {
            return Ok(Value::Null);
        }
        if let Some(ref lv) = len_val {
            if lv.is_null() {
                return Ok(Value::Null);
            }
        }

        let mut s = match s_val {
            Value::Text(s) => s,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "substring".to_owned(),
                    expected: "Text".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        let start = match start_val {
            Value::Int64(n) => n as usize,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "substring".to_owned(),
                    expected: "Int64".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        let max_len = match len_val {
            Some(Value::Int64(n)) => Some(n as usize),
            Some(other) => {
                return Err(FuncError::TypeMismatch {
                    function: "substring".to_owned(),
                    expected: "Int64".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
            None => None,
        };

        if max_len == Some(0) {
            return Ok(Value::Text(String::new()));
        }

        let char_count = s.chars().count();
        if start >= char_count {
            return Ok(Value::Text(String::new()));
        }

        let end_char = match max_len {
            Some(len) => (start + len).min(char_count),
            None => char_count,
        };

        let byte_start = s
            .char_indices()
            .nth(start)
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        let byte_end = s
            .char_indices()
            .nth(end_char)
            .map(|(i, _)| i)
            .unwrap_or(s.len());

        if byte_start == 0 {
            s.truncate(byte_end);
            Ok(Value::Text(s))
        } else {
            s.drain(..byte_start);
            s.truncate(byte_end - byte_start);
            Ok(Value::Text(s))
        }
    }
}

struct CharAtFunc;

impl ExprFunction for CharAtFunc {
    fn name(&self) -> &str {
        "charAt"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_text_arg("charAt", &args[0].data_type)?;
        Ok(NullableExprType::nullable(DataType::Text { size: None }))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let idx_val = args.remove(1);
        let s_val = args.remove(0);
        if s_val.is_null() || idx_val.is_null() {
            return Ok(Value::Null);
        }
        let s = match s_val {
            Value::Text(s) => s,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "charAt".to_owned(),
                    expected: "Text".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        let idx = match idx_val {
            Value::Int64(n) => n as usize,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "charAt".to_owned(),
                    expected: "Int64".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        match s.chars().nth(idx) {
            Some(c) => Ok(Value::Text(c.to_string())),
            None => Ok(Value::Null),
        }
    }
}

struct UpperFunc;

impl ExprFunction for UpperFunc {
    fn name(&self) -> &str {
        "upper"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_text_arg("upper", &args[0].data_type)?;
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            args[0].nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Text(s) => Ok(Value::Text(s.to_uppercase())),
            other => Err(FuncError::TypeMismatch {
                function: "upper".to_owned(),
                expected: "Text".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

struct LowerFunc;

impl ExprFunction for LowerFunc {
    fn name(&self) -> &str {
        "lower"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_text_arg("lower", &args[0].data_type)?;
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            args[0].nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Text(s) => Ok(Value::Text(s.to_lowercase())),
            other => Err(FuncError::TypeMismatch {
                function: "lower".to_owned(),
                expected: "Text".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

struct TrimFunc;

impl ExprFunction for TrimFunc {
    fn name(&self) -> &str {
        "trim"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_text_arg("trim", &args[0].data_type)?;
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            args[0].nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Text(mut s) => {
                let trimmed_len = s.trim().len();
                if trimmed_len == s.len() {
                    Ok(Value::Text(s))
                } else {
                    let start = s.len() - s.trim_start().len();
                    s.drain(..start);
                    s.truncate(trimmed_len);
                    Ok(Value::Text(s))
                }
            }
            other => Err(FuncError::TypeMismatch {
                function: "trim".to_owned(),
                expected: "Text".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

struct ReplaceFunc;

impl ExprFunction for ReplaceFunc {
    fn name(&self) -> &str {
        "replace"
    }

    fn min_args(&self) -> usize {
        3
    }

    fn max_args(&self) -> Option<usize> {
        Some(3)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let replacement = args.remove(2);
        let pattern = args.remove(1);
        let source = args.remove(0);
        if source.is_null() || pattern.is_null() || replacement.is_null() {
            return Ok(Value::Null);
        }
        let s = extract_text(source, "replace")?;
        let pat = extract_text(pattern, "replace")?;
        let rep = extract_text(replacement, "replace")?;
        if !s.contains(pat.as_str()) {
            return Ok(Value::Text(s));
        }
        Ok(Value::Text(s.replace(&pat, &rep)))
    }
}

struct StartsWithFunc;

impl ExprFunction for StartsWithFunc {
    fn name(&self) -> &str {
        "startsWith"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Bool, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let prefix = args.remove(1);
        let source = args.remove(0);
        if source.is_null() || prefix.is_null() {
            return Ok(Value::Null);
        }
        let s = extract_text(source, "startsWith")?;
        let p = extract_text(prefix, "startsWith")?;
        Ok(Value::Bool(s.starts_with(&p)))
    }
}

struct EndsWithFunc;

impl ExprFunction for EndsWithFunc {
    fn name(&self) -> &str {
        "endsWith"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Bool, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let suffix = args.remove(1);
        let source = args.remove(0);
        if source.is_null() || suffix.is_null() {
            return Ok(Value::Null);
        }
        let s = extract_text(source, "endsWith")?;
        let p = extract_text(suffix, "endsWith")?;
        Ok(Value::Bool(s.ends_with(&p)))
    }
}

struct ContainsFunc;

impl ExprFunction for ContainsFunc {
    fn name(&self) -> &str {
        "contains"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Bool, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let needle = args.remove(1);
        let haystack = args.remove(0);
        if haystack.is_null() || needle.is_null() {
            return Ok(Value::Null);
        }
        let s = extract_text(haystack, "contains")?;
        let n = extract_text(needle, "contains")?;
        Ok(Value::Bool(s.contains(&n)))
    }
}

struct IndexOfFunc;

impl ExprFunction for IndexOfFunc {
    fn name(&self) -> &str {
        "indexOf"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Int64, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let needle = args.remove(1);
        let haystack = args.remove(0);
        if haystack.is_null() || needle.is_null() {
            return Ok(Value::Null);
        }
        let s = extract_text(haystack, "indexOf")?;
        let n = extract_text(needle, "indexOf")?;
        match s.find(&n) {
            Some(byte_pos) => {
                let char_pos = s[..byte_pos].chars().count();
                Ok(Value::Int64(char_pos as i64))
            }
            None => Ok(Value::Int64(-1)),
        }
    }
}

struct FormatFunc;

impl ExprFunction for FormatFunc {
    fn name(&self) -> &str {
        "format"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        None
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            nullable,
        ))
    }

    fn evaluate(&self, args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let mut iter = args.into_iter();
        let template = iter.next().expect("min_args guarantees at least 1");
        if template.is_null() {
            return Ok(Value::Null);
        }
        let tmpl = match template {
            Value::Text(s) => s,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "format".to_owned(),
                    expected: "Text".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        let mut result = tmpl;
        for (i, val) in iter.enumerate() {
            if val.is_null() {
                return Ok(Value::Null);
            }
            let placeholder = format!("{{{i}}}");
            let replacement = format_value(&val);
            result = result.replace(&placeholder, &replacement);
        }
        Ok(Value::Text(result))
    }
}

struct ToStringFunc;

impl ExprFunction for ToStringFunc {
    fn name(&self) -> &str {
        "toString"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            args[0].nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Text(s) => Ok(Value::Text(s)),
            other => Ok(Value::Text(format_value(&other))),
        }
    }
}

struct ReverseFunc;

impl ExprFunction for ReverseFunc {
    fn name(&self) -> &str {
        "reverse"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_text_arg("reverse", &args[0].data_type)?;
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            args[0].nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let val = args.remove(0);
        if val.is_null() {
            return Ok(Value::Null);
        }
        match val {
            Value::Text(s) => {
                let reversed: String = s.chars().rev().collect();
                Ok(Value::Text(reversed))
            }
            other => Err(FuncError::TypeMismatch {
                function: "reverse".to_owned(),
                expected: "Text".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

struct RepeatFunc;

impl ExprFunction for RepeatFunc {
    fn name(&self) -> &str {
        "repeat"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let n_val = args.remove(1);
        let s_val = args.remove(0);
        if s_val.is_null() || n_val.is_null() {
            return Ok(Value::Null);
        }
        let s = extract_text(s_val, "repeat")?;
        let n = match n_val {
            Value::Int64(n) => n,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "repeat".to_owned(),
                    expected: "Int64".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        if n < 0 {
            return Ok(Value::Text(String::new()));
        }
        Ok(Value::Text(s.repeat(n as usize)))
    }
}

struct LeftPadFunc;

impl ExprFunction for LeftPadFunc {
    fn name(&self) -> &str {
        "leftPad"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(3)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let pad_val = if args.len() == 3 {
            Some(args.remove(2))
        } else {
            None
        };
        let len_val = args.remove(1);
        let s_val = args.remove(0);

        if s_val.is_null() || len_val.is_null() {
            return Ok(Value::Null);
        }
        if let Some(ref pv) = pad_val {
            if pv.is_null() {
                return Ok(Value::Null);
            }
        }

        let s = extract_text(s_val, "leftPad")?;
        let target_len = match len_val {
            Value::Int64(n) => n,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "leftPad".to_owned(),
                    expected: "Int64".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        let pad_char = match pad_val {
            Some(v) => {
                let pad_str = extract_text(v, "leftPad")?;
                pad_str.chars().next().unwrap_or(' ')
            }
            None => ' ',
        };

        let char_count = s.chars().count();
        if target_len <= 0 || char_count >= target_len as usize {
            return Ok(Value::Text(s));
        }
        let pad_count = target_len as usize - char_count;
        let mut result = String::with_capacity(s.len() + pad_count * pad_char.len_utf8());
        for _ in 0..pad_count {
            result.push(pad_char);
        }
        result.push_str(&s);
        Ok(Value::Text(result))
    }
}

struct RightPadFunc;

impl ExprFunction for RightPadFunc {
    fn name(&self) -> &str {
        "rightPad"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(3)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let pad_val = if args.len() == 3 {
            Some(args.remove(2))
        } else {
            None
        };
        let len_val = args.remove(1);
        let s_val = args.remove(0);

        if s_val.is_null() || len_val.is_null() {
            return Ok(Value::Null);
        }
        if let Some(ref pv) = pad_val {
            if pv.is_null() {
                return Ok(Value::Null);
            }
        }

        let s = extract_text(s_val, "rightPad")?;
        let target_len = match len_val {
            Value::Int64(n) => n,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "rightPad".to_owned(),
                    expected: "Int64".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        let pad_char = match pad_val {
            Some(v) => {
                let pad_str = extract_text(v, "rightPad")?;
                pad_str.chars().next().unwrap_or(' ')
            }
            None => ' ',
        };

        let char_count = s.chars().count();
        if target_len <= 0 || char_count >= target_len as usize {
            return Ok(Value::Text(s));
        }
        let pad_count = target_len as usize - char_count;
        let mut result = String::with_capacity(s.len() + pad_count * pad_char.len_utf8());
        result.push_str(&s);
        for _ in 0..pad_count {
            result.push(pad_char);
        }
        Ok(Value::Text(result))
    }
}

fn validate_text_arg(function: &str, dt: &DataType) -> Result<(), FuncError> {
    if !matches!(dt, DataType::Text { .. }) {
        return Err(FuncError::TypeMismatch {
            function: function.to_owned(),
            expected: "Text".to_owned(),
            actual: format!("{dt}"),
        });
    }
    Ok(())
}

fn extract_text(val: Value, func_name: &str) -> Result<String, FuncError> {
    match val {
        Value::Text(s) => Ok(s),
        other => Err(FuncError::TypeMismatch {
            function: func_name.to_owned(),
            expected: "Text".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

fn format_value(val: &Value) -> String {
    match val {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Int8(n) => n.to_string(),
        Value::Int16(n) => n.to_string(),
        Value::Int32(n) => n.to_string(),
        Value::Int64(n) => n.to_string(),
        Value::UInt8(n) => n.to_string(),
        Value::UInt16(n) => n.to_string(),
        Value::UInt32(n) => n.to_string(),
        Value::UInt64(n) => n.to_string(),
        Value::Float32(n) => n.to_string(),
        Value::Float64(n) => n.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::Decimal(n) => n.to_string(),
        Value::Text(s) => s.clone(),
        Value::Bytes(b) => format!("{b:?}"),
        Value::Date(d) => d.to_string(),
        Value::Timestamp(t) => t.to_rfc3339(),
        Value::Uuid(u) => u.to_string(),
        Value::Ipv4(a) => a.to_string(),
        Value::Ipv6(a) => a.to_string(),
        Value::Json(j) => j.to_string(),
        Value::Object(entries) => {
            let map: serde_json::Map<String, serde_json::Value> = entries
                .iter()
                .map(|(k, v)| {
                    let json_v = air_elt_types::value_to_json(v).unwrap_or(serde_json::Value::Null);
                    (k.clone(), json_v)
                })
                .collect();
            serde_json::Value::Object(map).to_string()
        }
        Value::Custom(v) => v
            .to_json()
            .map_or_else(|_| format!("{v:?}"), |j| j.to_string()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_support::ctx;

    #[test]
    fn concat_basic() {
        let f = ConcatFunc;
        let result = f
            .evaluate(
                vec![
                    Value::Text("a".into()),
                    Value::Text("b".into()),
                    Value::Text("c".into()),
                ],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Text("abc".into()));
    }

    #[test]
    fn concat_null_propagation() {
        let f = ConcatFunc;
        let result = f
            .evaluate(vec![Value::Text("a".into()), Value::Null], &ctx())
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn length_utf8() {
        let f = LengthFunc;
        let result = f
            .evaluate(vec![Value::Text("hello".into())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(5));

        let result = f
            .evaluate(vec![Value::Text("\u{1F600}abc".into())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(4));
    }

    #[test]
    fn substring_basic() {
        let f = SubstringFunc;
        let result = f
            .evaluate(
                vec![
                    Value::Text("hello world".into()),
                    Value::Int64(6),
                    Value::Int64(5),
                ],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Text("world".into()));
    }

    #[test]
    fn substring_no_length() {
        let f = SubstringFunc;
        let result = f
            .evaluate(
                vec![Value::Text("hello world".into()), Value::Int64(6)],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Text("world".into()));
    }

    #[test]
    fn substring_out_of_bounds() {
        let f = SubstringFunc;
        let result = f
            .evaluate(vec![Value::Text("hi".into()), Value::Int64(10)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Text(String::new()));
    }

    #[test]
    fn char_at_basic() {
        let f = CharAtFunc;
        let result = f
            .evaluate(vec![Value::Text("hello".into()), Value::Int64(1)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Text("e".into()));
    }

    #[test]
    fn char_at_out_of_bounds() {
        let f = CharAtFunc;
        let result = f
            .evaluate(vec![Value::Text("hi".into()), Value::Int64(5)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn upper_lower() {
        let u = UpperFunc;
        let l = LowerFunc;
        assert_eq!(
            u.evaluate(vec![Value::Text("hello".into())], &ctx())
                .unwrap(),
            Value::Text("HELLO".into())
        );
        assert_eq!(
            l.evaluate(vec![Value::Text("HELLO".into())], &ctx())
                .unwrap(),
            Value::Text("hello".into())
        );
    }

    #[test]
    fn trim_whitespace() {
        let f = TrimFunc;
        let result = f
            .evaluate(vec![Value::Text("  hello  ".into())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Text("hello".into()));
    }

    #[test]
    fn replace_basic() {
        let f = ReplaceFunc;
        let result = f
            .evaluate(
                vec![
                    Value::Text("hello world".into()),
                    Value::Text("world".into()),
                    Value::Text("rust".into()),
                ],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Text("hello rust".into()));
    }

    #[test]
    fn starts_with() {
        let f = StartsWithFunc;
        let result = f
            .evaluate(
                vec![Value::Text("hello".into()), Value::Text("hel".into())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn ends_with() {
        let f = EndsWithFunc;
        let result = f
            .evaluate(
                vec![Value::Text("hello".into()), Value::Text("llo".into())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn contains_basic() {
        let f = ContainsFunc;
        let result = f
            .evaluate(
                vec![
                    Value::Text("hello world".into()),
                    Value::Text("lo wo".into()),
                ],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn index_of_found() {
        let f = IndexOfFunc;
        let result = f
            .evaluate(
                vec![Value::Text("hello".into()), Value::Text("ll".into())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Int64(2));
    }

    #[test]
    fn index_of_not_found() {
        let f = IndexOfFunc;
        let result = f
            .evaluate(
                vec![Value::Text("hello".into()), Value::Text("xyz".into())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Int64(-1));
    }

    #[test]
    fn format_basic() {
        let f = FormatFunc;
        let result = f
            .evaluate(
                vec![
                    Value::Text("{0} is {1}".into()),
                    Value::Text("rust".into()),
                    Value::Text("great".into()),
                ],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Text("rust is great".into()));
    }

    #[test]
    fn to_string_int() {
        let f = ToStringFunc;
        let result = f.evaluate(vec![Value::Int64(42)], &ctx()).unwrap();
        assert_eq!(result, Value::Text("42".into()));
    }

    #[test]
    fn reverse_basic() {
        let f = ReverseFunc;
        let result = f
            .evaluate(vec![Value::Text("hello".into())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Text("olleh".into()));
    }

    #[test]
    fn reverse_utf8() {
        let f = ReverseFunc;
        let result = f
            .evaluate(vec![Value::Text("\u{1F600}ab".into())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Text("ba\u{1F600}".into()));
    }

    #[test]
    fn reverse_null_propagation() {
        let f = ReverseFunc;
        let result = f.evaluate(vec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn repeat_basic() {
        let f = RepeatFunc;
        let result = f
            .evaluate(vec![Value::Text("ab".into()), Value::Int64(3)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Text("ababab".into()));
    }

    #[test]
    fn repeat_zero() {
        let f = RepeatFunc;
        let result = f
            .evaluate(vec![Value::Text("ab".into()), Value::Int64(0)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Text(String::new()));
    }

    #[test]
    fn repeat_negative() {
        let f = RepeatFunc;
        let result = f
            .evaluate(vec![Value::Text("ab".into()), Value::Int64(-1)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Text(String::new()));
    }

    #[test]
    fn left_pad_basic() {
        let f = LeftPadFunc;
        let result = f
            .evaluate(vec![Value::Text("hi".into()), Value::Int64(5)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Text("   hi".into()));
    }

    #[test]
    fn left_pad_custom_char() {
        let f = LeftPadFunc;
        let result = f
            .evaluate(
                vec![
                    Value::Text("hi".into()),
                    Value::Int64(5),
                    Value::Text("0".into()),
                ],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Text("000hi".into()));
    }

    #[test]
    fn left_pad_already_long() {
        let f = LeftPadFunc;
        let result = f
            .evaluate(vec![Value::Text("hello".into()), Value::Int64(3)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Text("hello".into()));
    }

    #[test]
    fn right_pad_basic() {
        let f = RightPadFunc;
        let result = f
            .evaluate(vec![Value::Text("hi".into()), Value::Int64(5)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Text("hi   ".into()));
    }

    #[test]
    fn right_pad_custom_char() {
        let f = RightPadFunc;
        let result = f
            .evaluate(
                vec![
                    Value::Text("hi".into()),
                    Value::Int64(5),
                    Value::Text(".".into()),
                ],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Text("hi...".into()));
    }

    // --- Allocation optimization tests ---

    #[test]
    fn to_string_passthrough_no_clone() {
        // toString on a Text value should return the same allocation (no clone)
        let input = Value::Text("already a string".to_owned());
        let ptr_before = match &input {
            Value::Text(s) => s.as_ptr(),
            _ => unreachable!(),
        };
        let result = TO_STRING.evaluate(vec![input], &ctx()).unwrap();
        let ptr_after = match &result {
            Value::Text(s) => s.as_ptr(),
            _ => unreachable!(),
        };
        assert_eq!(
            ptr_before, ptr_after,
            "toString should pass through Text without cloning"
        );
    }

    #[test]
    fn substring_from_start_no_new_allocation() {
        // substring(s, 0, n) should reuse the buffer (truncate in place)
        let input = Value::Text("hello world".to_owned());
        let ptr_before = match &input {
            Value::Text(s) => s.as_ptr(),
            _ => unreachable!(),
        };
        let result = SUBSTRING
            .evaluate(vec![input, Value::Int64(0), Value::Int64(5)], &ctx())
            .unwrap();
        let ptr_after = match &result {
            Value::Text(s) => s.as_ptr(),
            _ => unreachable!(),
        };
        assert_eq!(
            ptr_before, ptr_after,
            "substring from 0 should reuse buffer"
        );
        assert_eq!(result, Value::Text("hello".to_owned()));
    }

    #[test]
    fn trim_no_whitespace_reuses_buffer() {
        // trim on a string without whitespace should return same allocation
        let input = Value::Text("no_spaces".to_owned());
        let ptr_before = match &input {
            Value::Text(s) => s.as_ptr(),
            _ => unreachable!(),
        };
        let result = TRIM.evaluate(vec![input], &ctx()).unwrap();
        let ptr_after = match &result {
            Value::Text(s) => s.as_ptr(),
            _ => unreachable!(),
        };
        assert_eq!(
            ptr_before, ptr_after,
            "trim should reuse buffer when no whitespace"
        );
    }

    #[test]
    fn replace_no_match_reuses_buffer() {
        // replace when pattern not found should return same allocation
        let input = Value::Text("hello world".to_owned());
        let ptr_before = match &input {
            Value::Text(s) => s.as_ptr(),
            _ => unreachable!(),
        };
        let result = REPLACE
            .evaluate(
                vec![
                    input,
                    Value::Text("xyz".to_owned()),
                    Value::Text("abc".to_owned()),
                ],
                &ctx(),
            )
            .unwrap();
        let ptr_after = match &result {
            Value::Text(s) => s.as_ptr(),
            _ => unreachable!(),
        };
        assert_eq!(
            ptr_before, ptr_after,
            "replace should reuse buffer when pattern not found"
        );
    }

    #[test]
    fn concat_reuses_first_arg_buffer() {
        // concat should push onto the first arg's buffer
        let input = Value::Text(String::with_capacity(100));
        let ptr_before = match &input {
            Value::Text(s) => s.as_ptr(),
            _ => unreachable!(),
        };
        let result = CONCAT
            .evaluate(vec![input, Value::Text("hello".to_owned())], &ctx())
            .unwrap();
        let ptr_after = match &result {
            Value::Text(s) => s.as_ptr(),
            _ => unreachable!(),
        };
        assert_eq!(
            ptr_before, ptr_after,
            "concat should reuse first arg buffer"
        );
    }
}
