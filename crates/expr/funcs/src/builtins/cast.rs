use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use num_bigint::BigInt;

use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::convert::{ConversionContext, ConvertError};
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{EvalContext, ExprFunction};

static CAST_TO_STRING: CastToStringFunc = CastToStringFunc;
static CAST_TO_INT8: CastToInt8Func = CastToInt8Func;
static CAST_TO_INT16: CastToInt16Func = CastToInt16Func;
static CAST_TO_INT32: CastToInt32Func = CastToInt32Func;
static CAST_TO_INT64: CastToInt64Func = CastToInt64Func;
static CAST_TO_UINT8: CastToUInt8Func = CastToUInt8Func;
static CAST_TO_UINT16: CastToUInt16Func = CastToUInt16Func;
static CAST_TO_UINT32: CastToUInt32Func = CastToUInt32Func;
static CAST_TO_UINT64: CastToUInt64Func = CastToUInt64Func;
static CAST_TO_FLOAT32: CastToFloat32Func = CastToFloat32Func;
static CAST_TO_FLOAT64: CastToFloat64Func = CastToFloat64Func;
static CAST_TO_BOOL: CastToBoolFunc = CastToBoolFunc;
static CAST_TO_DATE: CastToDateFunc = CastToDateFunc;
static CAST_TO_TIMESTAMP: CastToTimestampFunc = CastToTimestampFunc;
static CAST_TO_UUID: CastToUuidFunc = CastToUuidFunc;
static CAST_TO_BIGINT: CastToBigIntFunc = CastToBigIntFunc;
static CAST_TO_DECIMAL: CastToDecimalFunc = CastToDecimalFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&CAST_TO_STRING);
    registry.register(&CAST_TO_INT8);
    registry.register(&CAST_TO_INT16);
    registry.register(&CAST_TO_INT32);
    registry.register(&CAST_TO_INT64);
    registry.register(&CAST_TO_UINT8);
    registry.register(&CAST_TO_UINT16);
    registry.register(&CAST_TO_UINT32);
    registry.register(&CAST_TO_UINT64);
    registry.register(&CAST_TO_FLOAT32);
    registry.register(&CAST_TO_FLOAT64);
    registry.register(&CAST_TO_BOOL);
    registry.register(&CAST_TO_DATE);
    registry.register(&CAST_TO_TIMESTAMP);
    registry.register(&CAST_TO_UUID);
    registry.register(&CAST_TO_BIGINT);
    registry.register(&CAST_TO_DECIMAL);
}

// ---------------------------------------------------------------------------
// Shared conversion context for all cast functions.
// Cast is an explicit user request, so truncation is always allowed.
// ---------------------------------------------------------------------------

fn cast_context() -> ConversionContext {
    ConversionContext {
        truncate: true,
        default: None,
    }
}

/// Map a `ConvertError` into `FuncError::EvalFailed` with the cast function name.
fn convert_error_to_func_error(function: &str, error: ConvertError) -> FuncError {
    FuncError::EvalFailed {
        function: function.to_owned(),
        reason: error.to_string(),
    }
}

// ---------------------------------------------------------------------------
// toStringCast
// ---------------------------------------------------------------------------

struct CastToStringFunc;

impl ExprFunction for CastToStringFunc {
    fn name(&self) -> &str {
        "toStringCast"
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
            other => Ok(Value::Text(value_to_string(&other))),
        }
    }
}

// ---------------------------------------------------------------------------
// toInt8
// ---------------------------------------------------------------------------

struct CastToInt8Func;

impl ExprFunction for CastToInt8Func {
    fn name(&self) -> &str {
        "toInt8"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Int8, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let n = to_i64_via_convert("toInt8", a)?;
        let narrow = i8::try_from(n).map_err(|_| FuncError::EvalFailed {
            function: "toInt8".to_owned(),
            reason: format!("value {n} out of Int8 range (-128..127)"),
        })?;
        Ok(Value::Int8(narrow))
    }
}

// ---------------------------------------------------------------------------
// toInt16
// ---------------------------------------------------------------------------

struct CastToInt16Func;

impl ExprFunction for CastToInt16Func {
    fn name(&self) -> &str {
        "toInt16"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Int16, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let n = to_i64_via_convert("toInt16", a)?;
        let narrow = i16::try_from(n).map_err(|_| FuncError::EvalFailed {
            function: "toInt16".to_owned(),
            reason: format!("value {n} out of Int16 range (-32768..32767)"),
        })?;
        Ok(Value::Int16(narrow))
    }
}

// ---------------------------------------------------------------------------
// toInt32
// ---------------------------------------------------------------------------

struct CastToInt32Func;

impl ExprFunction for CastToInt32Func {
    fn name(&self) -> &str {
        "toInt32"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Int32, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let n = to_i64_via_convert("toInt32", a)?;
        let narrow = i32::try_from(n).map_err(|_| FuncError::EvalFailed {
            function: "toInt32".to_owned(),
            reason: format!("value {n} out of Int32 range"),
        })?;
        Ok(Value::Int32(narrow))
    }
}

// ---------------------------------------------------------------------------
// toInt64
// ---------------------------------------------------------------------------

struct CastToInt64Func;

impl ExprFunction for CastToInt64Func {
    fn name(&self) -> &str {
        "toInt64"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Int64, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let n = to_i64_via_convert("toInt64", a)?;
        Ok(Value::Int64(n))
    }
}

// ---------------------------------------------------------------------------
// toUInt8
// ---------------------------------------------------------------------------

struct CastToUInt8Func;

impl ExprFunction for CastToUInt8Func {
    fn name(&self) -> &str {
        "toUInt8"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::UInt8, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let n = to_i64_via_convert("toUInt8", a)?;
        if n < 0 {
            return Err(FuncError::EvalFailed {
                function: "toUInt8".to_owned(),
                reason: format!("negative value {n} cannot be converted to UInt8"),
            });
        }
        let narrow = u8::try_from(n).map_err(|_| FuncError::EvalFailed {
            function: "toUInt8".to_owned(),
            reason: format!("value {n} out of UInt8 range (0..255)"),
        })?;
        Ok(Value::UInt8(narrow))
    }
}

// ---------------------------------------------------------------------------
// toUInt16
// ---------------------------------------------------------------------------

struct CastToUInt16Func;

impl ExprFunction for CastToUInt16Func {
    fn name(&self) -> &str {
        "toUInt16"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::UInt16, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let n = to_i64_via_convert("toUInt16", a)?;
        if n < 0 {
            return Err(FuncError::EvalFailed {
                function: "toUInt16".to_owned(),
                reason: format!("negative value {n} cannot be converted to UInt16"),
            });
        }
        let narrow = u16::try_from(n).map_err(|_| FuncError::EvalFailed {
            function: "toUInt16".to_owned(),
            reason: format!("value {n} out of UInt16 range (0..65535)"),
        })?;
        Ok(Value::UInt16(narrow))
    }
}

// ---------------------------------------------------------------------------
// toUInt32
// ---------------------------------------------------------------------------

struct CastToUInt32Func;

impl ExprFunction for CastToUInt32Func {
    fn name(&self) -> &str {
        "toUInt32"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::UInt32, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let n = to_i64_via_convert("toUInt32", a)?;
        if n < 0 {
            return Err(FuncError::EvalFailed {
                function: "toUInt32".to_owned(),
                reason: format!("negative value {n} cannot be converted to UInt32"),
            });
        }
        let narrow = u32::try_from(n).map_err(|_| FuncError::EvalFailed {
            function: "toUInt32".to_owned(),
            reason: format!("value {n} out of UInt32 range (0..4294967295)"),
        })?;
        Ok(Value::UInt32(narrow))
    }
}

// ---------------------------------------------------------------------------
// toUInt64
// ---------------------------------------------------------------------------

struct CastToUInt64Func;

impl ExprFunction for CastToUInt64Func {
    fn name(&self) -> &str {
        "toUInt64"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::UInt64, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::UInt64(n) => Ok(Value::UInt64(n)),
            Value::Int64(n) => {
                if n < 0 {
                    return Err(FuncError::EvalFailed {
                        function: "toUInt64".to_owned(),
                        reason: format!("negative value {n} cannot be converted to UInt64"),
                    });
                }
                Ok(Value::UInt64(n as u64))
            }
            Value::Float64(n) => {
                if n < 0.0 {
                    return Err(FuncError::EvalFailed {
                        function: "toUInt64".to_owned(),
                        reason: format!("negative value {n} cannot be converted to UInt64"),
                    });
                }
                // Delegate Float64->UInt64 to the type conversion system (truncates toward zero).
                air_elt_types::convert::convert(
                    Value::Float64(n),
                    &DataType::Float64,
                    &DataType::UInt64,
                    &cast_context(),
                )
                .map_err(|e| convert_error_to_func_error("toUInt64", e))
            }
            Value::Bool(b) => {
                // Delegate Bool->UInt64 to the type conversion system.
                air_elt_types::convert::convert(
                    Value::Bool(b),
                    &DataType::Bool,
                    &DataType::UInt64,
                    &cast_context(),
                )
                .map_err(|e| convert_error_to_func_error("toUInt64", e))
            }
            Value::Text(s) => {
                let n: u64 = s.trim().parse().map_err(|_| FuncError::EvalFailed {
                    function: "toUInt64".to_owned(),
                    reason: format!("cannot parse {s:?} as UInt64"),
                })?;
                Ok(Value::UInt64(n))
            }
            other => Err(FuncError::TypeMismatch {
                function: "toUInt64".to_owned(),
                expected: "Text, Float64, Bool, Int64, or UInt64".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// toFloat32
// ---------------------------------------------------------------------------

struct CastToFloat32Func;

impl ExprFunction for CastToFloat32Func {
    fn name(&self) -> &str {
        "toFloat32"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Float32, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Float32(n) => Ok(Value::Float32(n)),
            Value::Float64(n) => {
                let src_type = DataType::Float64;
                air_elt_types::convert::convert(
                    Value::Float64(n),
                    &src_type,
                    &DataType::Float32,
                    &cast_context(),
                )
                .map_err(|e| convert_error_to_func_error("toFloat32", e))
            }
            Value::Int64(n) => Ok(Value::Float32(n as f32)),
            Value::Text(s) => {
                let n: f32 = s.trim().parse().map_err(|_| FuncError::EvalFailed {
                    function: "toFloat32".to_owned(),
                    reason: format!("cannot parse {s:?} as Float32"),
                })?;
                Ok(Value::Float32(n))
            }
            other => Err(FuncError::TypeMismatch {
                function: "toFloat32".to_owned(),
                expected: "Text, Int64, Float64, or Float32".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// toFloat64
// ---------------------------------------------------------------------------

struct CastToFloat64Func;

impl ExprFunction for CastToFloat64Func {
    fn name(&self) -> &str {
        "toFloat64"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Float64, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Float64(n) => Ok(Value::Float64(n)),
            Value::Int64(n) => Ok(Value::Float64(n as f64)),
            Value::Text(s) => {
                let n: f64 = s.trim().parse().map_err(|_| FuncError::EvalFailed {
                    function: "toFloat64".to_owned(),
                    reason: format!("cannot parse {s:?} as Float64"),
                })?;
                Ok(Value::Float64(n))
            }
            other => Err(FuncError::TypeMismatch {
                function: "toFloat64".to_owned(),
                expected: "Text, Int64, or Float64".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// toBool
// ---------------------------------------------------------------------------

struct CastToBoolFunc;

impl ExprFunction for CastToBoolFunc {
    fn name(&self) -> &str {
        "toBool"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Bool, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Bool(b) => Ok(Value::Bool(b)),
            Value::Int64(n) => Ok(Value::Bool(n != 0)),
            Value::Text(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    return Ok(Value::Bool(false));
                }
                let result = air_elt_commons::bool_flag::parse(trimmed).unwrap_or(true);
                Ok(Value::Bool(result))
            }
            other => Err(FuncError::TypeMismatch {
                function: "toBool".to_owned(),
                expected: "Text, Int64, or Bool".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// toDate
// ---------------------------------------------------------------------------

struct CastToDateFunc;

impl ExprFunction for CastToDateFunc {
    fn name(&self) -> &str {
        "toDate"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Date, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Date(d) => Ok(Value::Date(d)),
            Value::Timestamp(ts) => {
                // Delegate Timestamp->Date to the type conversion system.
                air_elt_types::convert::convert(
                    Value::Timestamp(ts),
                    &DataType::Timestamp,
                    &DataType::Date,
                    &cast_context(),
                )
                .map_err(|e| convert_error_to_func_error("toDate", e))
            }
            Value::Text(s) => {
                let date = NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").map_err(|e| {
                    FuncError::EvalFailed {
                        function: "toDate".to_owned(),
                        reason: format!("cannot parse {s:?} as Date (YYYY-MM-DD): {e}"),
                    }
                })?;
                Ok(Value::Date(date))
            }
            other => Err(FuncError::TypeMismatch {
                function: "toDate".to_owned(),
                expected: "Text, Timestamp, or Date".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// toTimestamp
// ---------------------------------------------------------------------------

struct CastToTimestampFunc;

impl ExprFunction for CastToTimestampFunc {
    fn name(&self) -> &str {
        "toTimestamp"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Timestamp, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Timestamp(ts) => Ok(Value::Timestamp(ts)),
            Value::Int64(secs) => {
                let ts =
                    Utc.timestamp_opt(secs, 0)
                        .single()
                        .ok_or_else(|| FuncError::EvalFailed {
                            function: "toTimestamp".to_owned(),
                            reason: format!("unix seconds {secs} out of range"),
                        })?;
                Ok(Value::Timestamp(ts))
            }
            Value::Date(d) => {
                let ts = d
                    .and_hms_opt(0, 0, 0)
                    .ok_or_else(|| FuncError::EvalFailed {
                        function: "toTimestamp".to_owned(),
                        reason: "failed to build midnight timestamp from date".to_owned(),
                    })?;
                let utc: DateTime<Utc> = Utc.from_utc_datetime(&ts);
                Ok(Value::Timestamp(utc))
            }
            Value::Text(s) => {
                let ts = DateTime::parse_from_rfc3339(s.trim())
                    .map(|dt| dt.with_timezone(&Utc))
                    .map_err(|e| FuncError::EvalFailed {
                        function: "toTimestamp".to_owned(),
                        reason: format!("cannot parse {s:?} as RFC3339 timestamp: {e}"),
                    })?;
                Ok(Value::Timestamp(ts))
            }
            other => Err(FuncError::TypeMismatch {
                function: "toTimestamp".to_owned(),
                expected: "Text, Int64, Date, or Timestamp".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// toUuid
// ---------------------------------------------------------------------------

struct CastToUuidFunc;

impl ExprFunction for CastToUuidFunc {
    fn name(&self) -> &str {
        "toUuid"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Uuid, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let src_type = a.data_type().unwrap_or(DataType::Text { size: None });
        air_elt_types::convert::convert(a, &src_type, &DataType::Uuid, &cast_context()).map_err(
            |e| match e {
                ConvertError::Unsupported { .. } => FuncError::TypeMismatch {
                    function: "toUuid".to_owned(),
                    expected: "Text or Uuid".to_owned(),
                    actual: format!("{src_type:?}"),
                },
                other => convert_error_to_func_error("toUuid", other),
            },
        )
    }
}

// ---------------------------------------------------------------------------
// toBigInt
// ---------------------------------------------------------------------------

struct CastToBigIntFunc;

impl ExprFunction for CastToBigIntFunc {
    fn name(&self) -> &str {
        "toBigInt"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(
            DataType::BigInt { width: None },
            args[0].nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Text(s) => {
                let n: BigInt = s.trim().parse().map_err(|_| FuncError::EvalFailed {
                    function: "toBigInt".to_owned(),
                    reason: format!("cannot parse {s:?} as BigInt"),
                })?;
                Ok(Value::BigInt(n))
            }
            other => {
                let src_type = other.data_type().unwrap_or(DataType::Text { size: None });
                let target = DataType::BigInt { width: None };
                let result =
                    air_elt_types::convert::convert(other, &src_type, &target, &cast_context());
                match result {
                    Ok(v) => Ok(v),
                    Err(ConvertError::Unsupported { .. }) => Err(FuncError::TypeMismatch {
                        function: "toBigInt".to_owned(),
                        expected: "Text, Int64, or BigInt".to_owned(),
                        actual: format!("{src_type:?}"),
                    }),
                    Err(e) => Err(convert_error_to_func_error("toBigInt", e)),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// toDecimal
// ---------------------------------------------------------------------------

struct CastToDecimalFunc;

impl ExprFunction for CastToDecimalFunc {
    fn name(&self) -> &str {
        "toDecimal"
    }

    fn min_args(&self) -> usize {
        3
    }

    fn max_args(&self) -> Option<usize> {
        Some(3)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(
            DataType::Decimal {
                precision: None,
                scale: None,
            },
            args[0].nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let scale_val = args.remove(2);
        let precision_val = args.remove(1);
        let a = args.remove(0);

        if a.is_null() {
            return Ok(Value::Null);
        }

        let precision = extract_u32("toDecimal", "precision", &precision_val)?;
        let scale = extract_u32("toDecimal", "scale", &scale_val)?;

        if scale > precision {
            return Err(FuncError::EvalFailed {
                function: "toDecimal".to_owned(),
                reason: format!("scale ({scale}) cannot exceed precision ({precision})"),
            });
        }

        let decimal = match a {
            Value::Decimal(d) => d,
            Value::Int64(n) => BigDecimal::from(n),
            Value::Float64(n) => {
                use std::str::FromStr;
                // Convert through string to avoid floating-point representation artifacts
                BigDecimal::from_str(&format!("{n}")).map_err(|e| FuncError::EvalFailed {
                    function: "toDecimal".to_owned(),
                    reason: format!("cannot convert float {n} to Decimal: {e}"),
                })?
            }
            Value::Text(s) => {
                use std::str::FromStr;
                BigDecimal::from_str(s.trim()).map_err(|_| FuncError::EvalFailed {
                    function: "toDecimal".to_owned(),
                    reason: format!("cannot parse {s:?} as Decimal"),
                })?
            }
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "toDecimal".to_owned(),
                    expected: "Text, Int64, Float64, or Decimal".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };

        // Apply scale rounding
        let scaled = decimal.with_scale(i64::from(scale));

        // Validate that integer digits fit within (precision - scale)
        let max_integer_digits = precision - scale;
        let integer_part = scaled.to_string();
        let integer_digits = integer_part
            .split('.')
            .next()
            .map(|s| s.trim_start_matches('-').trim_start_matches('0').len())
            .unwrap_or(0);

        if integer_digits > max_integer_digits as usize {
            return Err(FuncError::EvalFailed {
                function: "toDecimal".to_owned(),
                reason: format!(
                    "value exceeds precision({precision}, {scale}): {integer_digits} integer digits > max {max_integer_digits}"
                ),
            });
        }

        Ok(Value::Decimal(scaled))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a value to i64 using the type conversion system where possible,
/// falling back to text parsing for string inputs.
///
/// This delegates to `air_elt_types::convert::convert` for numeric and boolean
/// inputs (Float→Int64 with truncation, Bool→Int64, identity, etc.), and
/// handles Text→i64 parsing directly since the conversion system does not
/// support that path.
fn to_i64_via_convert(function: &str, value: Value) -> Result<i64, FuncError> {
    match value {
        Value::Int64(n) => Ok(n),
        Value::Text(s) => {
            let n: i64 = s.trim().parse().map_err(|_| FuncError::EvalFailed {
                function: function.to_owned(),
                reason: format!("cannot parse {s:?} as integer"),
            })?;
            Ok(n)
        }
        other => {
            let src_type = other.data_type().unwrap_or(DataType::Text { size: None });
            let result = air_elt_types::convert::convert(
                other,
                &src_type,
                &DataType::Int64,
                &cast_context(),
            );
            match result {
                Ok(Value::Int64(n)) => Ok(n),
                Ok(_) => Err(FuncError::EvalFailed {
                    function: function.to_owned(),
                    reason: "unexpected conversion result".to_owned(),
                }),
                Err(ConvertError::Unsupported { .. }) => Err(FuncError::TypeMismatch {
                    function: function.to_owned(),
                    expected: "numeric, Bool, or Text".to_owned(),
                    actual: format!("{src_type:?}"),
                }),
                Err(e) => Err(convert_error_to_func_error(function, e)),
            }
        }
    }
}

/// Extract a u32 parameter (precision or scale) from a Value.
fn extract_u32(function: &str, param_name: &str, value: &Value) -> Result<u32, FuncError> {
    match value {
        Value::Int64(n) => {
            if *n < 0 {
                return Err(FuncError::EvalFailed {
                    function: function.to_owned(),
                    reason: format!("{param_name} must be non-negative, got {n}"),
                });
            }
            u32::try_from(*n).map_err(|_| FuncError::EvalFailed {
                function: function.to_owned(),
                reason: format!("{param_name} value {n} too large"),
            })
        }
        other => Err(FuncError::TypeMismatch {
            function: function.to_owned(),
            expected: format!("Int64 for {param_name}"),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

fn value_to_string(val: &Value) -> String {
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
        Value::Custom(_) => "<custom>".to_owned(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use chrono::{NaiveDate, TimeZone, Utc};

    use super::*;
    use crate::signature::EvalContext;
    use std::path::PathBuf;

    fn ctx() -> EvalContext {
        EvalContext {
            env_resolver: Arc::new(crate::test_support::EmptyEnv),
            file_resolver: Arc::new(crate::test_support::NoopFiles),
            now: chrono::Utc::now(),
            base_dir: PathBuf::new(),
        }
    }

    #[test]
    fn to_string_from_int() {
        let f = CastToStringFunc;
        let result = f.evaluate(vec![Value::Int64(42)], &ctx()).unwrap();
        assert_eq!(result, Value::Text("42".into()));
    }

    #[test]
    fn to_string_from_bool() {
        let f = CastToStringFunc;
        let result = f.evaluate(vec![Value::Bool(true)], &ctx()).unwrap();
        assert_eq!(result, Value::Text("true".into()));
    }

    #[test]
    fn to_int64_from_text() {
        let f = CastToInt64Func;
        let result = f.evaluate(vec![Value::Text("123".into())], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(123));
    }

    #[test]
    fn to_int64_from_float() {
        let f = CastToInt64Func;
        let result = f.evaluate(vec![Value::Float64(3.7)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(3));
    }

    #[test]
    fn to_int64_invalid_text() {
        let f = CastToInt64Func;
        let result = f.evaluate(vec![Value::Text("abc".into())], &ctx());
        assert!(result.is_err());
    }

    #[test]
    fn to_float64_from_int() {
        let f = CastToFloat64Func;
        let result = f.evaluate(vec![Value::Int64(5)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(5.0));
    }

    #[test]
    fn to_float64_from_text() {
        let f = CastToFloat64Func;
        let result = f.evaluate(vec![Value::Text("1.5".into())], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(1.5));
    }

    #[test]
    fn to_bool_from_int_zero() {
        let f = CastToBoolFunc;
        let result = f.evaluate(vec![Value::Int64(0)], &ctx()).unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn to_bool_from_int_nonzero() {
        let f = CastToBoolFunc;
        let result = f.evaluate(vec![Value::Int64(42)], &ctx()).unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn to_bool_from_text_false_variants() {
        let f = CastToBoolFunc;
        for s in ["false", "f", "no", "0", ""] {
            let result = f.evaluate(vec![Value::Text(s.into())], &ctx()).unwrap();
            assert_eq!(result, Value::Bool(false), "expected false for {s:?}");
        }
    }

    #[test]
    fn to_bool_from_text_true() {
        let f = CastToBoolFunc;
        let result = f.evaluate(vec![Value::Text("yes".into())], &ctx()).unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn null_propagation() {
        let f = CastToInt64Func;
        let result = f.evaluate(vec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    // --- toInt8 ---

    #[test]
    fn to_int8_valid() {
        let f = CastToInt8Func;
        let result = f.evaluate(vec![Value::Int64(127)], &ctx()).unwrap();
        assert_eq!(result, Value::Int8(127));
    }

    #[test]
    fn to_int8_negative() {
        let f = CastToInt8Func;
        let result = f.evaluate(vec![Value::Int64(-128)], &ctx()).unwrap();
        assert_eq!(result, Value::Int8(-128));
    }

    #[test]
    fn to_int8_overflow() {
        let f = CastToInt8Func;
        let result = f.evaluate(vec![Value::Int64(128)], &ctx());
        assert!(result.is_err());
    }

    #[test]
    fn to_int8_null() {
        let f = CastToInt8Func;
        let result = f.evaluate(vec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    // --- toInt16 ---

    #[test]
    fn to_int16_valid() {
        let f = CastToInt16Func;
        let result = f.evaluate(vec![Value::Int64(32767)], &ctx()).unwrap();
        assert_eq!(result, Value::Int16(32767));
    }

    #[test]
    fn to_int16_overflow() {
        let f = CastToInt16Func;
        let result = f.evaluate(vec![Value::Int64(32768)], &ctx());
        assert!(result.is_err());
    }

    // --- toInt32 ---

    #[test]
    fn to_int32_valid() {
        let f = CastToInt32Func;
        let result = f
            .evaluate(vec![Value::Text("100000".into())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int32(100000));
    }

    #[test]
    fn to_int32_overflow() {
        let f = CastToInt32Func;
        let result = f.evaluate(vec![Value::Int64(i64::from(i32::MAX) + 1)], &ctx());
        assert!(result.is_err());
    }

    // --- toUInt8 ---

    #[test]
    fn to_uint8_valid() {
        let f = CastToUInt8Func;
        let result = f.evaluate(vec![Value::Int64(255)], &ctx()).unwrap();
        assert_eq!(result, Value::UInt8(255));
    }

    #[test]
    fn to_uint8_negative() {
        let f = CastToUInt8Func;
        let result = f.evaluate(vec![Value::Int64(-1)], &ctx());
        assert!(result.is_err());
    }

    #[test]
    fn to_uint8_overflow() {
        let f = CastToUInt8Func;
        let result = f.evaluate(vec![Value::Int64(256)], &ctx());
        assert!(result.is_err());
    }

    // --- toUInt16 ---

    #[test]
    fn to_uint16_valid() {
        let f = CastToUInt16Func;
        let result = f.evaluate(vec![Value::Int64(65535)], &ctx()).unwrap();
        assert_eq!(result, Value::UInt16(65535));
    }

    // --- toUInt32 ---

    #[test]
    fn to_uint32_valid() {
        let f = CastToUInt32Func;
        let result = f
            .evaluate(vec![Value::Int64(4_294_967_295)], &ctx())
            .unwrap();
        assert_eq!(result, Value::UInt32(4_294_967_295));
    }

    // --- toUInt64 ---

    #[test]
    fn to_uint64_from_text() {
        let f = CastToUInt64Func;
        let result = f
            .evaluate(vec![Value::Text("18446744073709551615".into())], &ctx())
            .unwrap();
        assert_eq!(result, Value::UInt64(u64::MAX));
    }

    #[test]
    fn to_uint64_negative() {
        let f = CastToUInt64Func;
        let result = f.evaluate(vec![Value::Int64(-1)], &ctx());
        assert!(result.is_err());
    }

    // --- toFloat32 ---

    #[test]
    fn to_float32_from_float64() {
        let f = CastToFloat32Func;
        let result = f.evaluate(vec![Value::Float64(1.5)], &ctx()).unwrap();
        assert_eq!(result, Value::Float32(1.5));
    }

    #[test]
    #[allow(clippy::approx_constant)]
    fn to_float32_from_text() {
        let f = CastToFloat32Func;
        let result = f
            .evaluate(vec![Value::Text("3.14".into())], &ctx())
            .unwrap();
        // f32 precision — 3.14 is intentional, not std::f32::consts::PI.
        assert!(
            (match result {
                Value::Float32(n) => n,
                _ => panic!("expected Float32"),
            } - 3.14_f32)
                .abs()
                < 0.001
        );
    }

    // --- toDate ---

    #[test]
    fn to_date_from_text() {
        let f = CastToDateFunc;
        let result = f
            .evaluate(vec![Value::Text("2024-03-15".into())], &ctx())
            .unwrap();
        let expected = NaiveDate::from_ymd_opt(2024, 3, 15).unwrap();
        assert_eq!(result, Value::Date(expected));
    }

    #[test]
    fn to_date_from_timestamp() {
        let f = CastToDateFunc;
        let ts = Utc.with_ymd_and_hms(2024, 6, 15, 10, 30, 0).unwrap();
        let result = f.evaluate(vec![Value::Timestamp(ts)], &ctx()).unwrap();
        let expected = NaiveDate::from_ymd_opt(2024, 6, 15).unwrap();
        assert_eq!(result, Value::Date(expected));
    }

    #[test]
    fn to_date_invalid() {
        let f = CastToDateFunc;
        let result = f.evaluate(vec![Value::Text("not-a-date".into())], &ctx());
        assert!(result.is_err());
    }

    #[test]
    fn to_date_null() {
        let f = CastToDateFunc;
        let result = f.evaluate(vec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    // --- toTimestamp ---

    #[test]
    fn to_timestamp_from_int() {
        let f = CastToTimestampFunc;
        let result = f
            .evaluate(vec![Value::Int64(1_700_000_000)], &ctx())
            .unwrap();
        match result {
            Value::Timestamp(ts) => assert_eq!(ts.timestamp(), 1_700_000_000),
            other => panic!("expected Timestamp, got {other:?}"),
        }
    }

    #[test]
    fn to_timestamp_from_rfc3339() {
        let f = CastToTimestampFunc;
        let result = f
            .evaluate(vec![Value::Text("2024-01-15T12:30:00Z".into())], &ctx())
            .unwrap();
        match result {
            Value::Timestamp(ts) => {
                assert_eq!(ts.year(), 2024);
                assert_eq!(ts.month(), 1);
            }
            other => panic!("expected Timestamp, got {other:?}"),
        }
    }

    #[test]
    fn to_timestamp_from_date() {
        let f = CastToTimestampFunc;
        let date = NaiveDate::from_ymd_opt(2024, 6, 15).unwrap();
        let result = f.evaluate(vec![Value::Date(date)], &ctx()).unwrap();
        match result {
            Value::Timestamp(ts) => {
                assert_eq!(ts.date_naive(), date);
            }
            other => panic!("expected Timestamp, got {other:?}"),
        }
    }

    #[test]
    fn to_timestamp_null() {
        let f = CastToTimestampFunc;
        let result = f.evaluate(vec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    // --- toUuid ---

    #[test]
    fn to_uuid_from_text() {
        let f = CastToUuidFunc;
        let result = f
            .evaluate(
                vec![Value::Text("550e8400-e29b-41d4-a716-446655440000".into())],
                &ctx(),
            )
            .unwrap();
        match result {
            Value::Uuid(id) => {
                assert_eq!(id.to_string(), "550e8400-e29b-41d4-a716-446655440000");
            }
            other => panic!("expected Uuid, got {other:?}"),
        }
    }

    #[test]
    fn to_uuid_invalid() {
        let f = CastToUuidFunc;
        let result = f.evaluate(vec![Value::Text("not-a-uuid".into())], &ctx());
        assert!(result.is_err());
    }

    #[test]
    fn to_uuid_null() {
        let f = CastToUuidFunc;
        let result = f.evaluate(vec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    // --- toBigInt ---

    #[test]
    fn to_bigint_from_int64() {
        let f = CastToBigIntFunc;
        let result = f.evaluate(vec![Value::Int64(999_999)], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(999_999)));
    }

    #[test]
    fn to_bigint_from_text() {
        let f = CastToBigIntFunc;
        let result = f
            .evaluate(
                vec![Value::Text("123456789012345678901234567890".into())],
                &ctx(),
            )
            .unwrap();
        let expected: BigInt = "123456789012345678901234567890".parse().unwrap();
        assert_eq!(result, Value::BigInt(expected));
    }

    // --- toDecimal ---

    #[test]
    fn to_decimal_from_text() {
        let f = CastToDecimalFunc;
        let result = f
            .evaluate(
                vec![
                    Value::Text("123.45".into()),
                    Value::Int64(5),
                    Value::Int64(2),
                ],
                &ctx(),
            )
            .unwrap();
        match result {
            Value::Decimal(d) => {
                assert_eq!(d.to_string(), "123.45");
            }
            other => panic!("expected Decimal, got {other:?}"),
        }
    }

    #[test]
    fn to_decimal_overflow() {
        let f = CastToDecimalFunc;
        // precision=3, scale=1 means max 2 integer digits; 999 has 3
        let result = f.evaluate(
            vec![
                Value::Text("999.9".into()),
                Value::Int64(3),
                Value::Int64(1),
            ],
            &ctx(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn to_decimal_null() {
        let f = CastToDecimalFunc;
        let result = f
            .evaluate(vec![Value::Null, Value::Int64(10), Value::Int64(2)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    use chrono::Datelike;

    // --- Allocation optimization tests ---

    #[test]
    fn to_string_cast_passthrough() {
        let input = Value::Text("passthrough".to_owned());
        let ptr_before = match &input {
            Value::Text(s) => s.as_ptr(),
            _ => unreachable!(),
        };
        let result = CAST_TO_STRING.evaluate(vec![input], &ctx()).unwrap();
        let ptr_after = match &result {
            Value::Text(s) => s.as_ptr(),
            _ => unreachable!(),
        };
        assert_eq!(ptr_before, ptr_after);
    }
}
