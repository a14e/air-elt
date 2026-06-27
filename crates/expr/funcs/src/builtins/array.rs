use air_elt_expr_types::limits::{MAX_ARRAY_LEN, MAX_EXPR_STRING_BYTES};
use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value, values_equal};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{ArgWindow, EvalContext, ExprFunction};

static LEN: LenFunc = LenFunc;
static ELEMENT: ElementFunc = ElementFunc;
static ARRAY_GET: ArrayGetFunc = ArrayGetFunc;
static SLICE: SliceFunc = SliceFunc;
static SPLIT: SplitFunc = SplitFunc;
static JOIN: JoinFunc = JoinFunc;
static IS_EMPTY: IsEmptyFunc = IsEmptyFunc;
static FILTER_NOT_NULL: FilterNotNullFunc = FilterNotNullFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&LEN);
    registry.register(&ELEMENT);
    registry.register(&ARRAY_GET);
    registry.register(&SLICE);
    registry.register(&SPLIT);
    registry.register(&JOIN);
    registry.register(&IS_EMPTY);
    registry.register(&FILTER_NOT_NULL);
}

/// Whether a `DataType` is a collection accepted by `len` / `isEmpty`.
fn is_collection_type(dt: &DataType) -> bool {
    matches!(
        dt,
        DataType::Text { .. }
            | DataType::Bytes { .. }
            | DataType::Array { .. }
            | DataType::Object
            | DataType::Json
    )
}

/// Runtime element count for a collection `Value`. `Json` is the only variant
/// whose scalar form is a runtime failure (array / object lengths succeed).
fn collection_len(val: &Value, func_name: &str) -> Result<i64, FuncError> {
    match val {
        Value::Text(s) => Ok(s.chars().count() as i64),
        Value::Bytes(b) => Ok(b.len() as i64),
        Value::Array(items) => Ok(items.len() as i64),
        Value::Object(entries) => Ok(entries.len() as i64),
        Value::Json(serde_json::Value::Array(arr)) => Ok(arr.len() as i64),
        Value::Json(serde_json::Value::Object(map)) => Ok(map.len() as i64),
        Value::Json(_) => Err(FuncError::EvalFailed {
            function: func_name.to_owned(),
            reason: "expected a JSON array or object".to_owned(),
        }),
        other => Err(FuncError::TypeMismatch {
            function: func_name.to_owned(),
            expected: "Text, Bytes, Array, Object or Json".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

/// Resolve a possibly-negative index against a collection length.
/// Returns the non-negative position, or `None` when out of range.
fn resolve_index(index: i64, len: usize) -> Option<usize> {
    let len = len as i64;
    let effective = if index < 0 { index + len } else { index };
    if effective < 0 || effective >= len {
        None
    } else {
        Some(effective as usize)
    }
}

/// Extract an integer index from a `Value`, accepting any signed integer
/// variant a source row may carry.
fn extract_index(val: &Value, func_name: &str) -> Result<i64, FuncError> {
    match val {
        Value::Int8(n) => Ok(i64::from(*n)),
        Value::Int16(n) => Ok(i64::from(*n)),
        Value::Int32(n) => Ok(i64::from(*n)),
        Value::Int64(n) => Ok(*n),
        Value::UInt8(n) => Ok(i64::from(*n)),
        Value::UInt16(n) => Ok(i64::from(*n)),
        Value::UInt32(n) => Ok(i64::from(*n)),
        // A UInt64 past i64::MAX cannot be a valid index — saturate so the
        // caller reports a clean out-of-range rather than rejecting the type.
        Value::UInt64(n) => Ok(i64::try_from(*n).unwrap_or(i64::MAX)),
        other => Err(FuncError::TypeMismatch {
            function: func_name.to_owned(),
            expected: "integer".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

/// Resolve a clamped Python-style slice bound. `Null` means "open" (defaults to
/// `default`); a negative bound is offset from the end; the result is clamped
/// into `0..=len`.
fn resolve_slice_bound(val: &Value, len: usize, default: usize) -> usize {
    match val {
        Value::Null => default,
        other => match extract_index(other, "slice") {
            Ok(raw) => clamp_bound(raw, len),
            // A non-integer bound is treated as the open default — `slice` is
            // total at runtime; the type-check pass already constrains bounds.
            Err(_) => default,
        },
    }
}

fn clamp_bound(raw: i64, len: usize) -> usize {
    let len = len as i64;
    let effective = if raw < 0 { raw + len } else { raw };
    effective.clamp(0, len) as usize
}

struct LenFunc;

impl ExprFunction for LenFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "len"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        if !is_collection_type(&args[0].data_type) {
            return Err(FuncError::TypeMismatch {
                function: "len".to_owned(),
                expected: "Text, Bytes, Array, Object or Json".to_owned(),
                actual: format!("{}", args[0].data_type),
            });
        }
        Ok(NullableExprType::new(DataType::Int64, args[0].nullable))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let val = args.read(0);
        if val.is_null() {
            return Ok(Value::Null);
        }
        Ok(Value::Int64(collection_len(val, "len")?))
    }
}

struct ElementFunc;

impl ExprFunction for ElementFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "element"
    }

    fn can_fail(&self) -> bool {
        true
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let container = &args[0];
        match &container.data_type {
            DataType::Array {
                element: Some(element),
                element_nullable,
            } => Ok(NullableExprType::new(
                (**element).clone(),
                *element_nullable || container.nullable,
            )),
            DataType::Array { element: None, .. } => Ok(NullableExprType::nullable(DataType::Json)),
            DataType::Text { .. } => Ok(NullableExprType::new(
                DataType::Text { size: Some(1) },
                container.nullable,
            )),
            DataType::Object | DataType::Json => Ok(NullableExprType::nullable(DataType::Json)),
            other => Err(FuncError::TypeMismatch {
                function: "element".to_owned(),
                expected: "Array, Text, Object or Json".to_owned(),
                actual: format!("{other}"),
            }),
        }
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let index_val = args.read(1);
        let container = args.read(0);
        if container.is_null() || index_val.is_null() {
            return Ok(Value::Null);
        }
        match container {
            Value::Array(items) => {
                let index = extract_index(index_val, "element")?;
                match resolve_index(index, items.len()) {
                    Some(position) => Ok(items[position].clone()),
                    None => Err(index_out_of_range("element", index, items.len())),
                }
            }
            Value::Text(text) => {
                let index = extract_index(index_val, "element")?;
                let char_count = text.chars().count();
                match resolve_index(index, char_count) {
                    Some(position) => {
                        let ch = text
                            .chars()
                            .nth(position)
                            .ok_or_else(|| index_out_of_range("element", index, char_count))?;
                        Ok(Value::Text(ch.to_string()))
                    }
                    None => Err(index_out_of_range("element", index, char_count)),
                }
            }
            Value::Object(entries) => {
                let key = match index_val {
                    Value::Text(s) => s,
                    other => {
                        return Err(FuncError::TypeMismatch {
                            function: "element".to_owned(),
                            expected: "Text key".to_owned(),
                            actual: format!("{:?}", other.data_type()),
                        });
                    }
                };
                match entries.iter().find(|(k, _)| k == key) {
                    Some((_, value)) => Ok(value.clone()),
                    None => Err(FuncError::InvalidArgument {
                        function: "element".to_owned(),
                        message: format!("object has no key '{key}'"),
                    }),
                }
            }
            Value::Json(json) => element_json(json, index_val),
            other => Err(FuncError::TypeMismatch {
                function: "element".to_owned(),
                expected: "Array, Text, Object or Json".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

/// Dynamic `element` over a JSON container: array index (negative supported) or
/// object key. The extracted node is wrapped back into a `Value::Json`.
fn element_json(json: &serde_json::Value, index_val: &Value) -> Result<Value, FuncError> {
    match json {
        serde_json::Value::Array(arr) => {
            let index = extract_index(index_val, "element")?;
            match resolve_index(index, arr.len()) {
                Some(position) => Ok(Value::Json(arr[position].clone())),
                None => Err(index_out_of_range("element", index, arr.len())),
            }
        }
        serde_json::Value::Object(map) => {
            let key = match index_val {
                Value::Text(s) => s,
                other => {
                    return Err(FuncError::TypeMismatch {
                        function: "element".to_owned(),
                        expected: "Text key".to_owned(),
                        actual: format!("{:?}", other.data_type()),
                    });
                }
            };
            match map.get(key) {
                Some(value) => Ok(Value::Json(value.clone())),
                None => Err(FuncError::InvalidArgument {
                    function: "element".to_owned(),
                    message: format!("JSON object has no key '{key}'"),
                }),
            }
        }
        other => Err(FuncError::TypeMismatch {
            function: "element".to_owned(),
            expected: "JSON array or object".to_owned(),
            actual: format!("{other}"),
        }),
    }
}

fn index_out_of_range(function: &str, index: i64, len: usize) -> FuncError {
    FuncError::InvalidArgument {
        function: function.to_owned(),
        message: format!("index {index} out of range for length {len}"),
    }
}

struct ArrayGetFunc;

impl ExprFunction for ArrayGetFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "arrayGet"
    }

    fn min_args(&self) -> usize {
        3
    }

    fn max_args(&self) -> Option<usize> {
        Some(3)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        match &args[0].data_type {
            DataType::Array {
                element: Some(element),
                element_nullable,
            } => Ok(NullableExprType::new(
                (**element).clone(),
                *element_nullable || args[0].nullable,
            )),
            DataType::Array { element: None, .. } => Ok(NullableExprType::nullable(DataType::Json)),
            other => Err(FuncError::TypeMismatch {
                function: "arrayGet".to_owned(),
                expected: "Array".to_owned(),
                actual: format!("{other}"),
            }),
        }
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let index_val = args.read(1);
        let array_val = args.read(0);
        // A null array short-circuits to Null (standard propagation). A null
        // index is treated as out-of-range and yields the soft default.
        if array_val.is_null() {
            return Ok(Value::Null);
        }
        if index_val.is_null() {
            return Ok(args.take(2));
        }
        let items = match array_val {
            Value::Array(items) => items,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "arrayGet".to_owned(),
                    expected: "Array".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        let index = extract_index(index_val, "arrayGet")?;
        match resolve_index(index, items.len()) {
            Some(position) => Ok(items[position].clone()),
            None => Ok(args.take(2)),
        }
    }
}

struct SliceFunc;

impl ExprFunction for SliceFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "slice"
    }

    fn min_args(&self) -> usize {
        3
    }

    fn max_args(&self) -> Option<usize> {
        Some(3)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args[0].nullable;
        match &args[0].data_type {
            DataType::Text { .. } | DataType::Bytes { .. } | DataType::Array { .. } => {
                Ok(NullableExprType::new(args[0].data_type.clone(), nullable))
            }
            other => Err(FuncError::TypeMismatch {
                function: "slice".to_owned(),
                expected: "Text, Bytes or Array".to_owned(),
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
        let start_val = args.read(1);
        let end_val = args.read(2);
        match args.read(0) {
            Value::Text(text) => {
                let chars: Vec<char> = text.chars().collect();
                let len = chars.len();
                let start = resolve_slice_bound(start_val, len, 0);
                let end = resolve_slice_bound(end_val, len, len);
                let sliced: String = if start < end {
                    chars[start..end].iter().collect()
                } else {
                    String::new()
                };
                Ok(Value::Text(sliced))
            }
            Value::Bytes(bytes) => {
                let len = bytes.len();
                let start = resolve_slice_bound(start_val, len, 0);
                let end = resolve_slice_bound(end_val, len, len);
                let sliced = if start < end {
                    bytes[start..end].to_vec()
                } else {
                    Vec::new()
                };
                Ok(Value::Bytes(sliced))
            }
            Value::Array(items) => {
                let len = items.len();
                let start = resolve_slice_bound(start_val, len, 0);
                let end = resolve_slice_bound(end_val, len, len);
                let sliced = if start < end {
                    items[start..end].to_vec()
                } else {
                    Vec::new()
                };
                Ok(Value::Array(sliced))
            }
            other => Err(FuncError::TypeMismatch {
                function: "slice".to_owned(),
                expected: "Text, Bytes or Array".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

struct SplitFunc;

impl ExprFunction for SplitFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "split"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::array(
            Some(DataType::Text { size: None }),
            false,
            nullable,
        ))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let string_val = args.read(0);
        let separator_val = args.read(1);
        if string_val.is_null() || separator_val.is_null() {
            return Ok(Value::Null);
        }
        let text = extract_text("split", string_val)?;
        let separator = extract_text("split", separator_val)?;
        // Bound the result at MAX_ARRAY_LEN. `take(MAX_ARRAY_LEN + 1)` caps the
        // allocation itself, so splitting a huge string (e.g. into single chars)
        // cannot build an unbounded array before the length check fires.
        let parts: Vec<Value> = if separator.is_empty() {
            text.chars()
                .take(MAX_ARRAY_LEN + 1)
                .map(|c| Value::Text(c.to_string()))
                .collect()
        } else {
            text.split(separator)
                .take(MAX_ARRAY_LEN + 1)
                .map(|part| Value::Text(part.to_owned()))
                .collect()
        };
        if parts.len() > MAX_ARRAY_LEN {
            return Err(FuncError::InvalidArgument {
                function: "split".to_owned(),
                message: format!("array length exceeds maximum {MAX_ARRAY_LEN}"),
            });
        }
        Ok(Value::Array(parts))
    }
}

struct JoinFunc;

impl ExprFunction for JoinFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "join"
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
        let array_val = args.read(0);
        let separator_val = args.read(1);
        if array_val.is_null() || separator_val.is_null() {
            return Ok(Value::Null);
        }
        let items = match array_val {
            Value::Array(items) => items,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "join".to_owned(),
                    expected: "Array".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        let separator = extract_text("join", separator_val)?;
        // Accumulate into a single buffer instead of collecting a `Vec<String>`
        // and re-joining it, and append `Text` elements directly rather than
        // cloning them through `value_to_string`. Mirrors the interpolation
        // evaluator — the canonical value→text renderer shared with `toString`,
        // including its `MAX_EXPR_STRING_BYTES` cap so a long array (e.g. a
        // `text[]` source column near `MAX_ARRAY_LEN`) cannot grow an unbounded
        // output string; every other string-producing builtin caps the same way.
        let mut rendered = String::new();
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                rendered.push_str(separator);
            }
            match item {
                Value::Text(text) => rendered.push_str(text),
                other => rendered.push_str(&air_elt_types::value_to_string(other)),
            }
            if rendered.len() > MAX_EXPR_STRING_BYTES {
                return Err(FuncError::StringTooLarge {
                    len: rendered.len(),
                    max: MAX_EXPR_STRING_BYTES,
                });
            }
        }
        Ok(Value::Text(rendered))
    }
}

struct IsEmptyFunc;

impl ExprFunction for IsEmptyFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "isEmpty"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        if !is_collection_type(&args[0].data_type) {
            return Err(FuncError::TypeMismatch {
                function: "isEmpty".to_owned(),
                expected: "Text, Bytes, Array, Object or Json".to_owned(),
                actual: format!("{}", args[0].data_type),
            });
        }
        Ok(NullableExprType::new(DataType::Bool, args[0].nullable))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let val = args.read(0);
        if val.is_null() {
            return Ok(Value::Null);
        }
        Ok(Value::Bool(collection_len(val, "isEmpty")? == 0))
    }
}

struct FilterNotNullFunc;

impl ExprFunction for FilterNotNullFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "filterNotNull"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        match &args[0].data_type {
            // Drop the `Null` members → the element type stays the same but its
            // nullability collapses to non-null. The array itself keeps its
            // outer nullability (a whole-array NULL stays NULL).
            DataType::Array { element, .. } => {
                let element = element.as_ref().map(|boxed| (**boxed).clone());
                Ok(NullableExprType::array(element, false, args[0].nullable))
            }
            other => Err(FuncError::TypeMismatch {
                function: "filterNotNull".to_owned(),
                expected: "Array".to_owned(),
                actual: format!("{other}"),
            }),
        }
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        match args.take(0) {
            Value::Null => Ok(Value::Null),
            Value::Array(items) => Ok(Value::Array(
                items.into_iter().filter(|item| !item.is_null()).collect(),
            )),
            other => Err(FuncError::TypeMismatch {
                function: "filterNotNull".to_owned(),
                expected: "Array".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

fn extract_text<'a>(func_name: &str, val: &'a Value) -> Result<&'a str, FuncError> {
    match val {
        Value::Text(s) => Ok(s),
        other => Err(FuncError::TypeMismatch {
            function: func_name.to_owned(),
            expected: "Text".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

/// Whether `array` contains an element structurally equal to `needle` via the
/// cross-numeric [`values_equal`]. Shared by `contains` / `indexOf` over arrays.
pub(crate) fn array_contains(array: &[Value], needle: &Value) -> bool {
    array.iter().any(|item| values_equal(item, needle))
}

/// Position of the first element of `array` equal to `needle` (via
/// [`values_equal`]), or `None`. Shared by `indexOf` over arrays.
pub(crate) fn array_position(array: &[Value], needle: &Value) -> Option<usize> {
    array.iter().position(|item| values_equal(item, needle))
}

/// Concatenate two arrays, enforcing [`MAX_ARRAY_LEN`]. Used by the `add`-array
/// branch; the variadic `concat` builtin enforces the same cap over its
/// running accumulator.
pub(crate) fn concat_arrays(
    function: &str,
    mut left: Vec<Value>,
    right: Vec<Value>,
) -> Result<Value, FuncError> {
    let total = left.len().saturating_add(right.len());
    if total > MAX_ARRAY_LEN {
        return Err(FuncError::InvalidArgument {
            function: function.to_owned(),
            message: format!("array length {total} exceeds maximum {MAX_ARRAY_LEN}"),
        });
    }
    left.extend(right);
    Ok(Value::Array(left))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_support::{ctx, eval};

    fn int_array(values: &[i64]) -> Value {
        Value::Array(values.iter().map(|n| Value::Int64(*n)).collect())
    }

    #[test]
    fn len_over_json_collections() {
        // The `Json` array/object arms replace the removed `jsonLength`.
        assert_eq!(
            eval(
                &LenFunc,
                smallvec::smallvec![Value::Json(serde_json::json!([1, 2, 3]))],
                &ctx()
            )
            .unwrap(),
            Value::Int64(3)
        );
        assert_eq!(
            eval(
                &LenFunc,
                smallvec::smallvec![Value::Json(serde_json::json!({"a": 1, "b": 2}))],
                &ctx()
            )
            .unwrap(),
            Value::Int64(2)
        );
    }

    #[test]
    fn slice_split_join_propagate_null() {
        assert_eq!(
            eval(
                &SliceFunc,
                smallvec::smallvec![Value::Null, Value::Int64(0), Value::Int64(1)],
                &ctx()
            )
            .unwrap(),
            Value::Null
        );
        assert_eq!(
            eval(
                &SplitFunc,
                smallvec::smallvec![Value::Null, Value::Text(",".to_owned())],
                &ctx()
            )
            .unwrap(),
            Value::Null
        );
        assert_eq!(
            eval(
                &JoinFunc,
                smallvec::smallvec![Value::Null, Value::Text(",".to_owned())],
                &ctx()
            )
            .unwrap(),
            Value::Null
        );
    }

    #[test]
    fn split_enforces_max_len() {
        // Splitting a string longer than MAX_ARRAY_LEN into single chars must
        // error rather than build an unbounded array.
        let oversized = "a".repeat(MAX_ARRAY_LEN + 1);
        let result = eval(
            &SplitFunc,
            smallvec::smallvec![Value::Text(oversized), Value::Text(String::new())],
            &ctx(),
        );
        assert!(matches!(result, Err(FuncError::InvalidArgument { .. })));
    }

    #[test]
    fn len_array_text_object() {
        assert_eq!(
            eval(&LenFunc, smallvec::smallvec![int_array(&[1, 2, 3])], &ctx()).unwrap(),
            Value::Int64(3)
        );
        assert_eq!(
            eval(
                &LenFunc,
                smallvec::smallvec![Value::Text("héllo".to_owned())],
                &ctx()
            )
            .unwrap(),
            Value::Int64(5)
        );
        let object = Value::Object(vec![("a".to_owned(), Value::Int64(1))]);
        assert_eq!(
            eval(&LenFunc, smallvec::smallvec![object], &ctx()).unwrap(),
            Value::Int64(1)
        );
    }

    #[test]
    fn len_empty_array() {
        assert_eq!(
            eval(&LenFunc, smallvec::smallvec![Value::Array(vec![])], &ctx()).unwrap(),
            Value::Int64(0)
        );
    }

    #[test]
    fn len_null_propagation() {
        assert_eq!(
            eval(&LenFunc, smallvec::smallvec![Value::Null], &ctx()).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn len_scalar_is_compile_time_type_mismatch() {
        let args = [NullableExprType::non_null(DataType::Int64)];
        assert!(matches!(
            LenFunc.resolve_type(&args),
            Err(FuncError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn len_json_scalar_runtime_error() {
        let json = Value::Json(serde_json::json!(42));
        assert!(eval(&LenFunc, smallvec::smallvec![json], &ctx()).is_err());
    }

    #[test]
    fn element_positive_and_negative_index() {
        assert_eq!(
            eval(
                &ElementFunc,
                smallvec::smallvec![int_array(&[10, 20, 30]), Value::Int64(1)],
                &ctx()
            )
            .unwrap(),
            Value::Int64(20)
        );
        assert_eq!(
            eval(
                &ElementFunc,
                smallvec::smallvec![int_array(&[10, 20, 30]), Value::Int64(-1)],
                &ctx()
            )
            .unwrap(),
            Value::Int64(30)
        );
    }

    #[test]
    fn element_out_of_bounds_errors() {
        assert!(matches!(
            eval(
                &ElementFunc,
                smallvec::smallvec![int_array(&[1, 2]), Value::Int64(5)],
                &ctx()
            ),
            Err(FuncError::InvalidArgument { .. })
        ));
        assert!(matches!(
            eval(
                &ElementFunc,
                smallvec::smallvec![int_array(&[1, 2]), Value::Int64(-5)],
                &ctx()
            ),
            Err(FuncError::InvalidArgument { .. })
        ));
    }

    #[test]
    fn element_text_returns_single_char() {
        assert_eq!(
            eval(
                &ElementFunc,
                smallvec::smallvec![Value::Text("abc".to_owned()), Value::Int64(-1)],
                &ctx()
            )
            .unwrap(),
            Value::Text("c".to_owned())
        );
    }

    #[test]
    fn element_object_missing_key_errors() {
        let object = Value::Object(vec![("a".to_owned(), Value::Int64(1))]);
        assert!(matches!(
            eval(
                &ElementFunc,
                smallvec::smallvec![object, Value::Text("missing".to_owned())],
                &ctx()
            ),
            Err(FuncError::InvalidArgument { .. })
        ));
    }

    #[test]
    fn element_json_array_and_object() {
        let json = Value::Json(serde_json::json!([7, 8, 9]));
        assert_eq!(
            eval(
                &ElementFunc,
                smallvec::smallvec![json, Value::Int64(0)],
                &ctx()
            )
            .unwrap(),
            Value::Json(serde_json::json!(7))
        );
        let json = Value::Json(serde_json::json!({"k": "v"}));
        assert_eq!(
            eval(
                &ElementFunc,
                smallvec::smallvec![json, Value::Text("k".to_owned())],
                &ctx()
            )
            .unwrap(),
            Value::Json(serde_json::json!("v"))
        );
    }

    #[test]
    fn element_resolve_type_uses_element_nullability() {
        let args = [
            NullableExprType::array(Some(DataType::Int64), true, false),
            NullableExprType::non_null(DataType::Int64),
        ];
        let resolved = ElementFunc.resolve_type(&args).unwrap();
        assert_eq!(resolved.data_type, DataType::Int64);
        assert!(resolved.nullable, "element_nullable propagates");
    }

    #[test]
    fn array_get_in_range_and_default() {
        assert_eq!(
            eval(
                &ArrayGetFunc,
                smallvec::smallvec![int_array(&[1, 2, 3]), Value::Int64(1), Value::Int64(-1)],
                &ctx()
            )
            .unwrap(),
            Value::Int64(2)
        );
        // Out of range returns the default, never errors.
        assert_eq!(
            eval(
                &ArrayGetFunc,
                smallvec::smallvec![int_array(&[1, 2, 3]), Value::Int64(9), Value::Int64(-1)],
                &ctx()
            )
            .unwrap(),
            Value::Int64(-1)
        );
        // Negative index supported.
        assert_eq!(
            eval(
                &ArrayGetFunc,
                smallvec::smallvec![int_array(&[1, 2, 3]), Value::Int64(-1), Value::Int64(0)],
                &ctx()
            )
            .unwrap(),
            Value::Int64(3)
        );
    }

    #[test]
    fn slice_clamp_open_and_negative_bounds() {
        // Clamp beyond the end.
        assert_eq!(
            eval(
                &SliceFunc,
                smallvec::smallvec![int_array(&[1, 2, 3]), Value::Int64(1), Value::Int64(99)],
                &ctx()
            )
            .unwrap(),
            int_array(&[2, 3])
        );
        // Open bounds via Null.
        assert_eq!(
            eval(
                &SliceFunc,
                smallvec::smallvec![int_array(&[1, 2, 3]), Value::Null, Value::Null],
                &ctx()
            )
            .unwrap(),
            int_array(&[1, 2, 3])
        );
        // Negative bounds.
        assert_eq!(
            eval(
                &SliceFunc,
                smallvec::smallvec![int_array(&[1, 2, 3, 4]), Value::Int64(-2), Value::Null],
                &ctx()
            )
            .unwrap(),
            int_array(&[3, 4])
        );
    }

    #[test]
    fn slice_text_by_char() {
        assert_eq!(
            eval(
                &SliceFunc,
                smallvec::smallvec![
                    Value::Text("héllo".to_owned()),
                    Value::Int64(1),
                    Value::Int64(3)
                ],
                &ctx()
            )
            .unwrap(),
            Value::Text("él".to_owned())
        );
    }

    #[test]
    fn slice_bytes_by_byte() {
        assert_eq!(
            eval(
                &SliceFunc,
                smallvec::smallvec![
                    Value::Bytes(vec![1, 2, 3, 4]),
                    Value::Int64(1),
                    Value::Int64(3)
                ],
                &ctx()
            )
            .unwrap(),
            Value::Bytes(vec![2, 3])
        );
    }

    #[test]
    fn slice_start_past_end_is_empty() {
        assert_eq!(
            eval(
                &SliceFunc,
                smallvec::smallvec![int_array(&[1, 2, 3]), Value::Int64(2), Value::Int64(1)],
                &ctx()
            )
            .unwrap(),
            Value::Array(vec![])
        );
    }

    #[test]
    fn split_join_round_trip() {
        let split = eval(
            &SplitFunc,
            smallvec::smallvec![Value::Text("a,b,c".to_owned()), Value::Text(",".to_owned())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            split,
            Value::Array(vec![
                Value::Text("a".to_owned()),
                Value::Text("b".to_owned()),
                Value::Text("c".to_owned()),
            ])
        );
        let joined = eval(
            &JoinFunc,
            smallvec::smallvec![split, Value::Text(",".to_owned())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(joined, Value::Text("a,b,c".to_owned()));
    }

    #[test]
    fn join_renders_non_text_elements() {
        let joined = eval(
            &JoinFunc,
            smallvec::smallvec![int_array(&[1, 2, 3]), Value::Text("-".to_owned())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(joined, Value::Text("1-2-3".to_owned()));
    }

    #[test]
    fn join_empty_array_is_empty_string() {
        // The degenerate path of the single-buffer loop: no elements → "".
        let joined = eval(
            &JoinFunc,
            smallvec::smallvec![Value::Array(vec![]), Value::Text(",".to_owned())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(joined, Value::Text(String::new()));
    }

    #[test]
    fn join_enforces_max_string_bytes() {
        // A single oversized element trips the same MAX_EXPR_STRING_BYTES cap
        // every other string-producing builtin enforces, instead of growing an
        // unbounded output buffer.
        let huge = Value::Text("a".repeat(MAX_EXPR_STRING_BYTES + 1));
        let result = eval(
            &JoinFunc,
            smallvec::smallvec![Value::Array(vec![huge]), Value::Text(",".to_owned())],
            &ctx(),
        );
        assert!(matches!(result, Err(FuncError::StringTooLarge { .. })));
    }

    #[test]
    fn is_empty_polymorphic() {
        assert_eq!(
            eval(
                &IsEmptyFunc,
                smallvec::smallvec![Value::Array(vec![])],
                &ctx()
            )
            .unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            eval(&IsEmptyFunc, smallvec::smallvec![int_array(&[1])], &ctx()).unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            eval(
                &IsEmptyFunc,
                smallvec::smallvec![Value::Text(String::new())],
                &ctx()
            )
            .unwrap(),
            Value::Bool(true)
        );
    }

    #[test]
    fn filter_not_null_drops_null_elements() {
        let array = Value::Array(vec![
            Value::Int64(1),
            Value::Null,
            Value::Int64(2),
            Value::Null,
        ]);
        assert_eq!(
            eval(&FilterNotNullFunc, smallvec::smallvec![array], &ctx()).unwrap(),
            int_array(&[1, 2])
        );
    }

    #[test]
    fn filter_not_null_empty_and_all_null() {
        assert_eq!(
            eval(
                &FilterNotNullFunc,
                smallvec::smallvec![Value::Array(vec![])],
                &ctx()
            )
            .unwrap(),
            Value::Array(vec![])
        );
        assert_eq!(
            eval(
                &FilterNotNullFunc,
                smallvec::smallvec![Value::Array(vec![Value::Null, Value::Null])],
                &ctx()
            )
            .unwrap(),
            Value::Array(vec![])
        );
    }

    #[test]
    fn filter_not_null_propagates_null_array() {
        assert_eq!(
            eval(&FilterNotNullFunc, smallvec::smallvec![Value::Null], &ctx()).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn filter_not_null_collapses_element_nullability() {
        // Array<Int64?> (nullable elements, nullable array) → Array<Int64>
        // (non-null elements); the array's own outer nullability is preserved.
        let args = [NullableExprType::array(Some(DataType::Int64), true, true)];
        let resolved = FilterNotNullFunc.resolve_type(&args).unwrap();
        assert_eq!(
            resolved.data_type,
            DataType::Array {
                element: Some(Box::new(DataType::Int64)),
                element_nullable: false,
            }
        );
        assert!(resolved.nullable, "outer array nullability is preserved");
    }

    #[test]
    fn filter_not_null_rejects_non_array() {
        let args = [NullableExprType::non_null(DataType::Int64)];
        assert!(matches!(
            FilterNotNullFunc.resolve_type(&args),
            Err(FuncError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn array_membership_helpers() {
        let array = vec![Value::Int64(1), Value::Int64(2), Value::Int64(3)];
        // Cross-numeric equality: Int32(2) matches Int64(2).
        assert!(array_contains(&array, &Value::Int32(2)));
        assert!(!array_contains(&array, &Value::Int64(9)));
        assert_eq!(array_position(&array, &Value::Int64(3)), Some(2));
        assert_eq!(array_position(&array, &Value::Int64(9)), None);
    }

    #[test]
    fn concat_arrays_enforces_max_len() {
        let small = concat_arrays("concat", vec![Value::Int64(1)], vec![Value::Int64(2)]).unwrap();
        assert_eq!(small, int_array(&[1, 2]));

        let left = vec![Value::Int64(0); MAX_ARRAY_LEN];
        let right = vec![Value::Int64(0); 1];
        assert!(matches!(
            concat_arrays("concat", left, right),
            Err(FuncError::InvalidArgument { .. })
        ));
    }
}
