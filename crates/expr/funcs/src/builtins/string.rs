use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{ArgWindow, EvalContext, ExprFunction};

fn text_size(dt: &DataType) -> Option<u32> {
    match dt {
        DataType::Text { size } => *size,
        _ => None,
    }
}

static CONCAT: ConcatFunc = ConcatFunc;
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
    fn is_pure(&self) -> bool {
        true
    }

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
        // Array overload: when every argument is an array, concatenate into a
        // single array (element types joined via the shared matrix helper).
        if args
            .iter()
            .all(|a| matches!(a.data_type, DataType::Array { .. }))
        {
            return concat_arrays_resolve_type(args);
        }
        // `concat` is otherwise strict: every argument must be textual. There is
        // no implicit string coercion in expressions — `concat(x, "")` is
        // therefore a string type-check on `x`, not a stringification (use
        // `toString` for that). Interpolation renders any type, but it does not
        // route through `concat`. A mix of Text and Array falls through to the
        // text validator, which rejects the Array argument as a TypeMismatch.
        for arg in args {
            validate_text_arg("concat", &arg.data_type)?;
        }
        let nullable = args.iter().any(|a| a.nullable);
        let size = args.iter().try_fold(0u32, |acc, a| match &a.data_type {
            DataType::Text { size: Some(s) } => Some(acc.saturating_add(*s)),
            _ => None,
        });
        Ok(NullableExprType::new(DataType::Text { size }, nullable))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        // Array overload: every argument is an array → concatenate (bounded by
        // MAX_ARRAY_LEN). A `null` argument propagates Null.
        if matches!(args.read(0), Value::Array(_)) {
            return concat_arrays_eval(args);
        }
        // Pre-scan by reference: nulls and type errors surface before anything
        // is taken, and the total size is known up front so the accumulator
        // grows exactly once instead of doubling through `push_str` reallocs.
        let mut total = 0usize;
        for i in 0..args.len() {
            let val = args.read(i);
            if val.is_null() {
                return Ok(Value::Null);
            }
            match val {
                Value::Text(s) => total += s.len(),
                other => return Err(concat_type_mismatch(other)),
            }
        }
        // Only the FIRST argument is taken — its `String` buffer becomes the
        // accumulator we push onto. Every later argument is read by reference:
        // `push_str` only borrows it, so taking would needlessly clone a const
        // separator (`'-'`, `' '`) or a non-last-use register on every row — the
        // hot path for `hash(concat(...))` surrogate keys.
        let mut result = match args.take(0) {
            Value::Text(s) => s,
            other => return Err(concat_type_mismatch(&other)),
        };
        result.reserve_exact(total - result.len());
        for i in 1..args.len() {
            match args.read(i) {
                Value::Text(s) => result.push_str(s),
                // Unreachable — the pre-scan validated every argument.
                other => return Err(concat_type_mismatch(other)),
            }
        }
        Ok(Value::Text(result))
    }
}

/// `resolve_type` for the all-array `concat` overload: fold the element types
/// through the shared matrix join, propagating element nullability.
fn concat_arrays_resolve_type(args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
    let nullable = args.iter().any(|a| a.nullable);
    let mut element: Option<Box<DataType>> = None;
    let mut element_nullable = false;
    for arg in args {
        if let DataType::Array {
            element: arg_element,
            element_nullable: arg_element_nullable,
        } = &arg.data_type
        {
            element_nullable = element_nullable || *arg_element_nullable;
            let joined = crate::arithmetic_utils::array_element_join(&element, arg_element)
                .map_err(|e| FuncError::TypeMismatch {
                    function: "concat".to_owned(),
                    expected: "arrays with compatible element types".to_owned(),
                    actual: e.to_string(),
                })?;
            element = joined.map(Box::new);
        }
    }
    Ok(NullableExprType {
        data_type: DataType::Array {
            element,
            element_nullable,
        },
        nullable,
        int_bound: None,
    })
}

/// `evaluate` for the all-array `concat` overload: concatenate every array
/// argument, propagating Null and enforcing `MAX_ARRAY_LEN`.
fn concat_arrays_eval(args: &mut dyn ArgWindow) -> Result<Value, FuncError> {
    for i in 0..args.len() {
        if args.read(i).is_null() {
            return Ok(Value::Null);
        }
    }
    let mut result: Vec<Value> = match args.take(0) {
        Value::Array(items) => items,
        other => return Err(concat_array_mismatch(&other)),
    };
    for i in 1..args.len() {
        match args.take(i) {
            Value::Array(items) => {
                let total = result.len().saturating_add(items.len());
                if total > air_elt_expr_types::limits::MAX_ARRAY_LEN {
                    return Err(FuncError::InvalidArgument {
                        function: "concat".to_owned(),
                        message: format!(
                            "array length {total} exceeds maximum {}",
                            air_elt_expr_types::limits::MAX_ARRAY_LEN
                        ),
                    });
                }
                result.extend(items);
            }
            other => return Err(concat_array_mismatch(&other)),
        }
    }
    Ok(Value::Array(result))
}

fn concat_array_mismatch(value: &Value) -> FuncError {
    FuncError::TypeMismatch {
        function: "concat".to_owned(),
        expected: "Array".to_owned(),
        actual: format!("{:?}", value.data_type()),
    }
}

/// The strict-`concat` type error for a non-text, non-null argument.
fn concat_type_mismatch(value: &Value) -> FuncError {
    FuncError::TypeMismatch {
        function: "concat".to_owned(),
        expected: "Text".to_owned(),
        actual: format!("{:?}", value.data_type()),
    }
}

struct CharAtFunc;

impl ExprFunction for CharAtFunc {
    fn is_pure(&self) -> bool {
        true
    }

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
        Ok(NullableExprType::nullable(DataType::Text { size: Some(1) }))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        if args.read(0).is_null() || args.read(1).is_null() {
            return Ok(Value::Null);
        }
        let idx = match args.read(1) {
            Value::Int64(n) => *n as usize,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "charAt".to_owned(),
                    expected: "Int64".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        let s = extract_text_ref(args.read(0), "charAt")?;
        match s.chars().nth(idx) {
            Some(c) => Ok(Value::Text(c.to_string())),
            None => Ok(Value::Null),
        }
    }
}

struct UpperFunc;

impl ExprFunction for UpperFunc {
    fn is_pure(&self) -> bool {
        true
    }

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
        let size = text_size(&args[0].data_type);
        Ok(NullableExprType::new(
            DataType::Text { size },
            args[0].nullable,
        ))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.take(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            // ASCII upper-casing is length-preserving, so the owned buffer is
            // mutated in place (no allocation) on the common path. Unicode
            // case-mapping can change length (`ß` → `SS`), so it must allocate.
            Value::Text(mut s) => {
                if s.is_ascii() {
                    s.make_ascii_uppercase();
                    Ok(Value::Text(s))
                } else {
                    Ok(Value::Text(s.to_uppercase()))
                }
            }
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
    fn is_pure(&self) -> bool {
        true
    }

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
        let size = text_size(&args[0].data_type);
        Ok(NullableExprType::new(
            DataType::Text { size },
            args[0].nullable,
        ))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.take(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            // ASCII lower-casing is length-preserving — mutate in place; Unicode
            // case-mapping can change length, so it must allocate.
            Value::Text(mut s) => {
                if s.is_ascii() {
                    s.make_ascii_lowercase();
                    Ok(Value::Text(s))
                } else {
                    Ok(Value::Text(s.to_lowercase()))
                }
            }
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
    fn is_pure(&self) -> bool {
        true
    }

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
        let size = text_size(&args[0].data_type);
        Ok(NullableExprType::new(
            DataType::Text { size },
            args[0].nullable,
        ))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.take(0);
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
    fn is_pure(&self) -> bool {
        true
    }

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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        // Only the source string is taken — its buffer is returned unchanged on
        // the no-match fast path. Pattern and replacement are read by reference:
        // `str::replace` only borrows them, so taking would clone a const needle
        // / replacement (`replace(col, 'x', 'y')`) on every row.
        let source = args.take(0);
        if source.is_null() {
            return Ok(Value::Null);
        }
        let pattern = args.read(1);
        let replacement = args.read(2);
        if pattern.is_null() || replacement.is_null() {
            return Ok(Value::Null);
        }
        let s = extract_text(source, "replace")?;
        let pat = extract_text_ref(pattern, "replace")?;
        let rep = extract_text_ref(replacement, "replace")?;
        if !s.contains(pat) {
            return Ok(Value::Text(s));
        }
        Ok(Value::Text(s.replace(pat, rep)))
    }
}

struct StartsWithFunc;

impl ExprFunction for StartsWithFunc {
    fn is_pure(&self) -> bool {
        true
    }

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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let source = args.read(0);
        let prefix = args.read(1);
        if source.is_null() || prefix.is_null() {
            return Ok(Value::Null);
        }
        let s = extract_text_ref(source, "startsWith")?;
        let p = extract_text_ref(prefix, "startsWith")?;
        Ok(Value::Bool(s.starts_with(p)))
    }
}

struct EndsWithFunc;

impl ExprFunction for EndsWithFunc {
    fn is_pure(&self) -> bool {
        true
    }

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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let source = args.read(0);
        let suffix = args.read(1);
        if source.is_null() || suffix.is_null() {
            return Ok(Value::Null);
        }
        let s = extract_text_ref(source, "endsWith")?;
        let p = extract_text_ref(suffix, "endsWith")?;
        Ok(Value::Bool(s.ends_with(p)))
    }
}

struct ContainsFunc;

impl ExprFunction for ContainsFunc {
    fn is_pure(&self) -> bool {
        true
    }

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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let needle = args.read(1);
        let haystack = args.read(0);
        if haystack.is_null() || needle.is_null() {
            return Ok(Value::Null);
        }
        // Polymorphic membership / substring / has-key test, matching the
        // `x in y` desugaring (`contains(y, x)`).
        match haystack {
            Value::Text(s) => {
                let n = extract_text_ref(needle, "contains")?;
                Ok(Value::Bool(s.contains(n)))
            }
            Value::Array(items) => Ok(Value::Bool(crate::builtins::array::array_contains(
                items, needle,
            ))),
            Value::Object(entries) => {
                let key = extract_text_ref(needle, "contains")?;
                Ok(Value::Bool(entries.iter().any(|(k, _)| k == key)))
            }
            Value::Json(serde_json::Value::Array(arr)) => {
                let target =
                    air_elt_types::value_to_json(needle).map_err(|e| FuncError::EvalFailed {
                        function: "contains".to_owned(),
                        reason: e.to_string(),
                    })?;
                Ok(Value::Bool(arr.contains(&target)))
            }
            Value::Json(serde_json::Value::Object(map)) => {
                let key = extract_text_ref(needle, "contains")?;
                Ok(Value::Bool(map.contains_key(key)))
            }
            other => Err(FuncError::TypeMismatch {
                function: "contains".to_owned(),
                expected: "Text, Array, Object or Json".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

struct IndexOfFunc;

impl ExprFunction for IndexOfFunc {
    fn is_pure(&self) -> bool {
        true
    }

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
        let int_bound =
            text_size(&args[0].data_type).map(|s| 64 - (s as u64).leading_zeros() as u8);
        Ok(NullableExprType {
            data_type: DataType::Int64,
            nullable,
            int_bound,
        })
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let needle = args.read(1);
        let haystack = args.read(0);
        if haystack.is_null() || needle.is_null() {
            return Ok(Value::Null);
        }
        // Array overload: 0-based position of the first element equal to the
        // needle (via the cross-numeric `values_equal`), else -1. Text keeps
        // the existing substring char-offset behaviour.
        match haystack {
            Value::Array(items) => match crate::builtins::array::array_position(items, needle) {
                Some(position) => Ok(Value::Int64(position as i64)),
                None => Ok(Value::Int64(-1)),
            },
            Value::Text(s) => {
                let n = extract_text_ref(needle, "indexOf")?;
                match s.find(n) {
                    Some(byte_pos) => {
                        let char_pos = s[..byte_pos].chars().count();
                        Ok(Value::Int64(char_pos as i64))
                    }
                    None => Ok(Value::Int64(-1)),
                }
            }
            other => Err(FuncError::TypeMismatch {
                function: "indexOf".to_owned(),
                expected: "Text or Array".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

struct FormatFunc;

impl ExprFunction for FormatFunc {
    fn is_pure(&self) -> bool {
        true
    }

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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let template = args.take(0);
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
        for i in 1..args.len() {
            let val = args.read(i);
            if val.is_null() {
                return Ok(Value::Null);
            }
            let placeholder = format!("{{{}}}", i - 1);
            let replacement = format_value(val);
            result = result.replace(&placeholder, &replacement);
        }
        Ok(Value::Text(result))
    }
}

struct ToStringFunc;

impl ExprFunction for ToStringFunc {
    fn is_pure(&self) -> bool {
        true
    }

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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        // `Text` is moved through unchanged (identity, zero-copy). Any other
        // type is rendered by reference — owning it buys nothing, and reading
        // avoids cloning a heap value (BigInt/Decimal) just to format it.
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        if !matches!(a, Value::Text(_)) {
            return Ok(Value::Text(format_value(a)));
        }
        match args.take(0) {
            Value::Text(s) => Ok(Value::Text(s)),
            other => Ok(Value::Text(format_value(&other))),
        }
    }
}

struct ReverseFunc;

impl ExprFunction for ReverseFunc {
    fn is_pure(&self) -> bool {
        true
    }

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
        // Polymorphic: returns the same type as the input. Text reverses chars,
        // Array reverses elements.
        match &args[0].data_type {
            DataType::Text { .. } | DataType::Array { .. } => Ok(NullableExprType::new(
                args[0].data_type.clone(),
                args[0].nullable,
            )),
            other => Err(FuncError::TypeMismatch {
                function: "reverse".to_owned(),
                expected: "Text or Array".to_owned(),
                actual: format!("{other}"),
            }),
        }
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        if args.read(0).is_null() {
            return Ok(Value::Null);
        }
        match args.take(0) {
            Value::Text(s) => {
                let reversed: String = s.chars().rev().collect();
                Ok(Value::Text(reversed))
            }
            Value::Array(mut items) => {
                items.reverse();
                Ok(Value::Array(items))
            }
            other => Err(FuncError::TypeMismatch {
                function: "reverse".to_owned(),
                expected: "Text or Array".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

struct RepeatFunc;

impl ExprFunction for RepeatFunc {
    fn is_pure(&self) -> bool {
        true
    }

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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        if args.read(0).is_null() || args.read(1).is_null() {
            return Ok(Value::Null);
        }
        let n = match args.read(1) {
            Value::Int64(n) => *n,
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
        let s = extract_text_ref(args.read(0), "repeat")?;
        let total_len = s.len().saturating_mul(n as usize);
        if total_len > air_elt_expr_types::limits::MAX_EXPR_STRING_BYTES {
            return Err(FuncError::StringTooLarge {
                len: total_len,
                max: air_elt_expr_types::limits::MAX_EXPR_STRING_BYTES,
            });
        }
        Ok(Value::Text(s.repeat(n as usize)))
    }
}

struct LeftPadFunc;

impl ExprFunction for LeftPadFunc {
    fn is_pure(&self) -> bool {
        true
    }

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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let has_pad = args.len() == 3;

        if args.read(0).is_null() || args.read(1).is_null() {
            return Ok(Value::Null);
        }
        if has_pad && args.read(2).is_null() {
            return Ok(Value::Null);
        }

        let target_len = match args.read(1) {
            Value::Int64(n) => *n,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "leftPad".to_owned(),
                    expected: "Int64".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        let pad_char = if has_pad {
            let pad_str = extract_text_ref(args.read(2), "leftPad")?;
            pad_str.chars().next().unwrap_or(' ')
        } else {
            ' '
        };

        let s = extract_text_ref(args.read(0), "leftPad")?;
        let char_count = s.chars().count();
        if target_len <= 0 || char_count >= target_len as usize {
            return Ok(Value::Text(s.to_owned()));
        }
        let pad_count = target_len as usize - char_count;
        let total_len = s.len() + pad_count * pad_char.len_utf8();
        if total_len > air_elt_expr_types::limits::MAX_EXPR_STRING_BYTES {
            return Err(FuncError::StringTooLarge {
                len: total_len,
                max: air_elt_expr_types::limits::MAX_EXPR_STRING_BYTES,
            });
        }
        let mut result = String::with_capacity(total_len);
        for _ in 0..pad_count {
            result.push(pad_char);
        }
        result.push_str(s);
        Ok(Value::Text(result))
    }
}

struct RightPadFunc;

impl ExprFunction for RightPadFunc {
    fn is_pure(&self) -> bool {
        true
    }

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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let has_pad = args.len() == 3;

        if args.read(0).is_null() || args.read(1).is_null() {
            return Ok(Value::Null);
        }
        if has_pad && args.read(2).is_null() {
            return Ok(Value::Null);
        }

        let target_len = match args.read(1) {
            Value::Int64(n) => *n,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "rightPad".to_owned(),
                    expected: "Int64".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        let pad_char = if has_pad {
            let pad_str = extract_text_ref(args.read(2), "rightPad")?;
            pad_str.chars().next().unwrap_or(' ')
        } else {
            ' '
        };

        let s = extract_text_ref(args.read(0), "rightPad")?;
        let char_count = s.chars().count();
        if target_len <= 0 || char_count >= target_len as usize {
            return Ok(Value::Text(s.to_owned()));
        }
        let pad_count = target_len as usize - char_count;
        let total_len = s.len() + pad_count * pad_char.len_utf8();
        if total_len > air_elt_expr_types::limits::MAX_EXPR_STRING_BYTES {
            return Err(FuncError::StringTooLarge {
                len: total_len,
                max: air_elt_expr_types::limits::MAX_EXPR_STRING_BYTES,
            });
        }
        let mut result = String::with_capacity(total_len);
        result.push_str(s);
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

/// Borrow the text out of a [`Value`] without consuming it — the zero-copy
/// counterpart to [`extract_text`] used by inspect-only functions that reach
/// their arguments through [`ArgWindow::read`].
fn extract_text_ref<'a>(val: &'a Value, func_name: &str) -> Result<&'a str, FuncError> {
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
        Value::Array(items) => {
            // Mirrors the `Object` arm and `air_elt_types::value_to_string`:
            // render as a JSON array, best-effort per element.
            let array: Vec<serde_json::Value> = items
                .iter()
                .map(|item| air_elt_types::value_to_json(item).unwrap_or(serde_json::Value::Null))
                .collect();
            serde_json::Value::Array(array).to_string()
        }
        Value::Custom(v) => v
            .to_json()
            .map_or_else(|_| format!("{v:?}"), |j| j.to_string()),
        // `Duration`'s `Debug` single-unit human form (`10s`, `1.5s`), kept
        // identical to `air_elt_types::value_to_string` for Interval. Compound
        // inputs normalise (`1h30m` → `5400s`); not a round-trip of the
        // authored literal.
        Value::Interval(d) => format!("{d:?}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_support::{ctx, eval};

    #[test]
    fn format_value_renders_interval_as_duration_debug_form() {
        // Kept byte-identical to `air_elt_types::convert::value_to_string`'s
        // Interval arm (the consistency contract noted at both sites): the
        // single-unit `Duration` Debug form, not a round-trip of the
        // authored literal.
        use std::time::Duration;
        assert_eq!(
            format_value(&Value::Interval(Duration::from_secs(10))),
            "10s"
        );
        assert_eq!(
            format_value(&Value::Interval(Duration::from_millis(500))),
            "500ms"
        );
    }

    #[test]
    fn concat_basic() {
        let f = ConcatFunc;
        let result = eval(
            &f,
            smallvec::smallvec![
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
    fn concat_spills_past_inline_capacity() {
        // Six arguments exceed `FuncArgVec`'s inline capacity of four, forcing
        // the heap spill; the by-value argument consumption must still yield the
        // full concatenation, proving the spilled buffer behaves like the inline
        // one.
        let f = ConcatFunc;
        let result = eval(
            &f,
            smallvec::smallvec![
                Value::Text("a".into()),
                Value::Text("b".into()),
                Value::Text("c".into()),
                Value::Text("d".into()),
                Value::Text("e".into()),
                Value::Text("f".into()),
            ],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Text("abcdef".into()));
    }

    #[test]
    fn concat_null_propagation() {
        let f = ConcatFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Text("a".into()), Value::Null],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn concat_rejects_non_text_value() {
        // Strict concat: a non-text, non-null argument is a TypeMismatch, not a
        // silent stringification.
        let f = ConcatFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Text("a".into()), Value::Int64(5)],
            &ctx(),
        );
        assert!(matches!(result, Err(FuncError::TypeMismatch { .. })));
        let leading = eval(
            &f,
            smallvec::smallvec![Value::Int64(5), Value::Text("a".into())],
            &ctx(),
        );
        assert!(matches!(leading, Err(FuncError::TypeMismatch { .. })));
    }

    #[test]
    fn concat_resolve_type_rejects_non_text() {
        let f = ConcatFunc;
        let args = [
            NullableExprType::new(DataType::Text { size: None }, false),
            NullableExprType::new(DataType::Int64, false),
        ];
        assert!(matches!(
            f.resolve_type(&args),
            Err(FuncError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn char_at_basic() {
        let f = CharAtFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Text("hello".into()), Value::Int64(1)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Text("e".into()));
    }

    #[test]
    fn char_at_out_of_bounds() {
        let f = CharAtFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Text("hi".into()), Value::Int64(5)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn upper_lower() {
        let u = UpperFunc;
        let l = LowerFunc;
        assert_eq!(
            eval(&u, smallvec::smallvec![Value::Text("hello".into())], &ctx()).unwrap(),
            Value::Text("HELLO".into())
        );
        assert_eq!(
            eval(&l, smallvec::smallvec![Value::Text("HELLO".into())], &ctx()).unwrap(),
            Value::Text("hello".into())
        );
    }

    #[test]
    fn trim_whitespace() {
        let f = TrimFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Text("  hello  ".into())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Text("hello".into()));
    }

    #[test]
    fn replace_basic() {
        let f = ReplaceFunc;
        let result = eval(
            &f,
            smallvec::smallvec![
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
        let result = eval(
            &f,
            smallvec::smallvec![Value::Text("hello".into()), Value::Text("hel".into())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn ends_with() {
        let f = EndsWithFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Text("hello".into()), Value::Text("llo".into())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn contains_basic() {
        let f = ContainsFunc;
        let result = eval(
            &f,
            smallvec::smallvec![
                Value::Text("hello world".into()),
                Value::Text("lo wo".into()),
            ],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn contains_array_membership() {
        // `x in arr` desugars to `contains(arr, x)`. Cross-numeric equality:
        // Int32(2) matches Int64(2).
        let f = ContainsFunc;
        let array = Value::Array(vec![Value::Int64(1), Value::Int64(2), Value::Int64(3)]);
        assert_eq!(
            eval(
                &f,
                smallvec::smallvec![array.clone(), Value::Int32(2)],
                &ctx()
            )
            .unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            eval(&f, smallvec::smallvec![array, Value::Int64(9)], &ctx()).unwrap(),
            Value::Bool(false)
        );
    }

    #[test]
    fn contains_object_has_key() {
        let f = ContainsFunc;
        let object = Value::Object(vec![("a".to_owned(), Value::Int64(1))]);
        assert_eq!(
            eval(
                &f,
                smallvec::smallvec![object.clone(), Value::Text("a".to_owned())],
                &ctx()
            )
            .unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            eval(
                &f,
                smallvec::smallvec![object, Value::Text("missing".to_owned())],
                &ctx()
            )
            .unwrap(),
            Value::Bool(false)
        );
    }

    #[test]
    fn index_of_array_overload() {
        let f = IndexOfFunc;
        let array = Value::Array(vec![Value::Int64(10), Value::Int64(20), Value::Int64(30)]);
        assert_eq!(
            eval(
                &f,
                smallvec::smallvec![array.clone(), Value::Int64(20)],
                &ctx()
            )
            .unwrap(),
            Value::Int64(1)
        );
        assert_eq!(
            eval(&f, smallvec::smallvec![array, Value::Int64(99)], &ctx()).unwrap(),
            Value::Int64(-1)
        );
    }

    #[test]
    fn reverse_array() {
        let f = ReverseFunc;
        let array = Value::Array(vec![Value::Int64(1), Value::Int64(2), Value::Int64(3)]);
        assert_eq!(
            eval(&f, smallvec::smallvec![array], &ctx()).unwrap(),
            Value::Array(vec![Value::Int64(3), Value::Int64(2), Value::Int64(1)])
        );
    }

    #[test]
    fn concat_array_overload() {
        let f = ConcatFunc;
        let result = eval(
            &f,
            smallvec::smallvec![
                Value::Array(vec![Value::Int64(1), Value::Int64(2)]),
                Value::Array(vec![Value::Int64(3)]),
            ],
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            result,
            Value::Array(vec![Value::Int64(1), Value::Int64(2), Value::Int64(3)])
        );
    }

    #[test]
    fn concat_array_element_type_join() {
        // Int32 array + Int64 array → element type Int64 via the matrix join.
        let f = ConcatFunc;
        let args = [
            NullableExprType::array(Some(DataType::Int32), false, false),
            NullableExprType::array(Some(DataType::Int64), false, false),
        ];
        let resolved = f.resolve_type(&args).unwrap();
        assert_eq!(
            resolved.data_type,
            DataType::Array {
                element: Some(Box::new(DataType::Int64)),
                element_nullable: false,
            }
        );
    }

    #[test]
    fn concat_array_incompatible_element_type_mismatch() {
        let f = ConcatFunc;
        let args = [
            NullableExprType::array(Some(DataType::Int64), false, false),
            NullableExprType::array(Some(DataType::Date), false, false),
        ];
        assert!(matches!(
            f.resolve_type(&args),
            Err(FuncError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn concat_mixed_text_array_type_mismatch() {
        let f = ConcatFunc;
        let args = [
            NullableExprType::new(DataType::Text { size: None }, false),
            NullableExprType::array(Some(DataType::Int64), false, false),
        ];
        assert!(matches!(
            f.resolve_type(&args),
            Err(FuncError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn index_of_found() {
        let f = IndexOfFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Text("hello".into()), Value::Text("ll".into())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Int64(2));
    }

    #[test]
    fn index_of_not_found() {
        let f = IndexOfFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Text("hello".into()), Value::Text("xyz".into())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Int64(-1));
    }

    #[test]
    fn format_basic() {
        let f = FormatFunc;
        let result = eval(
            &f,
            smallvec::smallvec![
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
        let result = eval(&f, smallvec::smallvec![Value::Int64(42)], &ctx()).unwrap();
        assert_eq!(result, Value::Text("42".into()));
    }

    #[test]
    fn reverse_basic() {
        let f = ReverseFunc;
        let result = eval(&f, smallvec::smallvec![Value::Text("hello".into())], &ctx()).unwrap();
        assert_eq!(result, Value::Text("olleh".into()));
    }

    #[test]
    fn reverse_utf8() {
        let f = ReverseFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Text("\u{1F600}ab".into())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Text("ba\u{1F600}".into()));
    }

    #[test]
    fn reverse_null_propagation() {
        let f = ReverseFunc;
        let result = eval(&f, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn repeat_basic() {
        let f = RepeatFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Text("ab".into()), Value::Int64(3)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Text("ababab".into()));
    }

    #[test]
    fn repeat_zero() {
        let f = RepeatFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Text("ab".into()), Value::Int64(0)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Text(String::new()));
    }

    #[test]
    fn repeat_negative() {
        let f = RepeatFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Text("ab".into()), Value::Int64(-1)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Text(String::new()));
    }

    #[test]
    fn left_pad_basic() {
        let f = LeftPadFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Text("hi".into()), Value::Int64(5)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Text("   hi".into()));
    }

    #[test]
    fn left_pad_custom_char() {
        let f = LeftPadFunc;
        let result = eval(
            &f,
            smallvec::smallvec![
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
        let result = eval(
            &f,
            smallvec::smallvec![Value::Text("hello".into()), Value::Int64(3)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Text("hello".into()));
    }

    #[test]
    fn right_pad_basic() {
        let f = RightPadFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Text("hi".into()), Value::Int64(5)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Text("hi   ".into()));
    }

    #[test]
    fn right_pad_custom_char() {
        let f = RightPadFunc;
        let result = eval(
            &f,
            smallvec::smallvec![
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
        let result = eval(&TO_STRING, smallvec::smallvec![input], &ctx()).unwrap();
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
    fn trim_no_whitespace_reuses_buffer() {
        // trim on a string without whitespace should return same allocation
        let input = Value::Text("no_spaces".to_owned());
        let ptr_before = match &input {
            Value::Text(s) => s.as_ptr(),
            _ => unreachable!(),
        };
        let result = eval(&TRIM, smallvec::smallvec![input], &ctx()).unwrap();
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
        let result = eval(
            &REPLACE,
            smallvec::smallvec![
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
        let result = eval(
            &CONCAT,
            smallvec::smallvec![input, Value::Text("hello".to_owned())],
            &ctx(),
        )
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
