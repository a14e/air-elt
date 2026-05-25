use chrono::{DateTime, Datelike, Duration, Months, Timelike, Utc};

use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{EvalContext, ExprFunction};

// ---------------------------------------------------------------------------
// Static instances
// ---------------------------------------------------------------------------

static NOW: NowFunc = NowFunc;
static TODAY: TodayFunc = TodayFunc;

static SECOND: SecondFunc = SecondFunc;
static MINUTE: MinuteFunc = MinuteFunc;
static HOUR: HourFunc = HourFunc;
static DAY: DayFunc = DayFunc;
static MONTH: MonthFunc = MonthFunc;
static YEAR: YearFunc = YearFunc;
static MILLISECOND: MillisecondFunc = MillisecondFunc;
static DAY_OF_WEEK: DayOfWeekFunc = DayOfWeekFunc;
static DAY_OF_YEAR: DayOfYearFunc = DayOfYearFunc;

static TO_SECONDS: ToSecondsFunc = ToSecondsFunc;
static TO_MILLIS: ToMillisFunc = ToMillisFunc;
static FROM_SECONDS: FromSecondsFunc = FromSecondsFunc;
static FROM_MILLIS: FromMillisFunc = FromMillisFunc;

static ADD_DAYS: AddDaysFunc = AddDaysFunc;
static ADD_HOURS: AddHoursFunc = AddHoursFunc;
static ADD_MINUTES: AddMinutesFunc = AddMinutesFunc;
static ADD_SECONDS: AddSecondsFunc = AddSecondsFunc;
static ADD_MILLISECONDS: AddMillisecondsFunc = AddMillisecondsFunc;
static ADD_MONTHS: AddMonthsFunc = AddMonthsFunc;
static ADD_YEARS: AddYearsFunc = AddYearsFunc;

static SUBTRACT_DAYS: SubtractDaysFunc = SubtractDaysFunc;
static SUBTRACT_HOURS: SubtractHoursFunc = SubtractHoursFunc;
static SUBTRACT_MINUTES: SubtractMinutesFunc = SubtractMinutesFunc;
static SUBTRACT_SECONDS: SubtractSecondsFunc = SubtractSecondsFunc;
static SUBTRACT_MILLISECONDS: SubtractMillisecondsFunc = SubtractMillisecondsFunc;
static SUBTRACT_MONTHS: SubtractMonthsFunc = SubtractMonthsFunc;
static SUBTRACT_YEARS: SubtractYearsFunc = SubtractYearsFunc;

static DATE_DIFF: DateDiffFunc = DateDiffFunc;
static FORMAT_DATE_TIME: FormatDateTimeFunc = FormatDateTimeFunc;

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&NOW);
    registry.register(&TODAY);

    registry.register(&SECOND);
    registry.register(&MINUTE);
    registry.register(&HOUR);
    registry.register(&DAY);
    registry.register(&MONTH);
    registry.register(&YEAR);
    registry.register(&MILLISECOND);
    registry.register(&DAY_OF_WEEK);
    registry.register(&DAY_OF_YEAR);

    registry.register(&TO_SECONDS);
    registry.register(&TO_MILLIS);
    registry.register(&FROM_SECONDS);
    registry.register(&FROM_MILLIS);

    registry.register(&ADD_DAYS);
    registry.register(&ADD_HOURS);
    registry.register(&ADD_MINUTES);
    registry.register(&ADD_SECONDS);
    registry.register(&ADD_MILLISECONDS);
    registry.register(&ADD_MONTHS);
    registry.register(&ADD_YEARS);

    registry.register(&SUBTRACT_DAYS);
    registry.register(&SUBTRACT_HOURS);
    registry.register(&SUBTRACT_MINUTES);
    registry.register(&SUBTRACT_SECONDS);
    registry.register(&SUBTRACT_MILLISECONDS);
    registry.register(&SUBTRACT_MONTHS);
    registry.register(&SUBTRACT_YEARS);

    registry.register(&DATE_DIFF);
    registry.register(&FORMAT_DATE_TIME);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_timestamp(val: Value, func: &str) -> Result<DateTime<Utc>, FuncError> {
    match val {
        Value::Timestamp(t) => Ok(t),
        Value::Date(d) => d
            .and_hms_opt(0, 0, 0)
            .map(|ndt| ndt.and_utc())
            .ok_or_else(|| FuncError::EvalFailed {
                function: func.to_owned(),
                reason: "invalid date for timestamp conversion".to_owned(),
            }),
        _ => Err(FuncError::TypeMismatch {
            function: func.to_owned(),
            expected: "Timestamp or Date".to_owned(),
            actual: format!("{:?}", val.data_type()),
        }),
    }
}

fn extract_int(val: Value, func: &str) -> Result<i64, FuncError> {
    match val {
        Value::Int64(n) => Ok(n),
        _ => Err(FuncError::TypeMismatch {
            function: func.to_owned(),
            expected: "Int64".to_owned(),
            actual: format!("{:?}", val.data_type()),
        }),
    }
}

fn extract_string(val: Value, func: &str) -> Result<String, FuncError> {
    match val {
        Value::Text(s) => Ok(s),
        _ => Err(FuncError::TypeMismatch {
            function: func.to_owned(),
            expected: "Text".to_owned(),
            actual: format!("{:?}", val.data_type()),
        }),
    }
}

/// Resolve type for functions accepting a timestamp/date argument.
fn timestamp_input_type(args: &[NullableExprType], func: &str) -> Result<(), FuncError> {
    match &args[0].data_type {
        DataType::Timestamp | DataType::Date => Ok(()),
        other => Err(FuncError::TypeMismatch {
            function: func.to_owned(),
            expected: "Timestamp or Date".to_owned(),
            actual: format!("{other}"),
        }),
    }
}

// ---------------------------------------------------------------------------
// now() / today()
// ---------------------------------------------------------------------------

struct NowFunc;

impl ExprFunction for NowFunc {
    fn name(&self) -> &str {
        "now"
    }

    fn min_args(&self) -> usize {
        0
    }

    fn max_args(&self) -> Option<usize> {
        Some(0)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Timestamp))
    }

    fn evaluate(&self, _args: Vec<Value>, context: &EvalContext) -> Result<Value, FuncError> {
        Ok(Value::Timestamp(context.now))
    }
}

struct TodayFunc;

impl ExprFunction for TodayFunc {
    fn name(&self) -> &str {
        "today"
    }

    fn min_args(&self) -> usize {
        0
    }

    fn max_args(&self) -> Option<usize> {
        Some(0)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Date))
    }

    fn evaluate(&self, _args: Vec<Value>, context: &EvalContext) -> Result<Value, FuncError> {
        Ok(Value::Date(context.now.date_naive()))
    }
}

// ---------------------------------------------------------------------------
// Component extraction (bare nouns) — all return Int64
// ---------------------------------------------------------------------------

macro_rules! extract_component_func {
    ($struct_name:ident, $func_name:expr, $extract_fn:expr) => {
        struct $struct_name;

        impl ExprFunction for $struct_name {
            fn name(&self) -> &str {
                $func_name
            }

            fn min_args(&self) -> usize {
                1
            }

            fn max_args(&self) -> Option<usize> {
                Some(1)
            }

            fn resolve_type(
                &self,
                args: &[NullableExprType],
            ) -> Result<NullableExprType, FuncError> {
                timestamp_input_type(args, $func_name)?;
                Ok(NullableExprType::new(DataType::Int64, args[0].nullable))
            }

            fn evaluate(
                &self,
                mut args: Vec<Value>,
                _context: &EvalContext,
            ) -> Result<Value, FuncError> {
                let a = args.remove(0);
                if a.is_null() {
                    return Ok(Value::Null);
                }
                let dt = extract_timestamp(a, $func_name)?;
                let extractor: fn(DateTime<Utc>) -> i64 = $extract_fn;
                Ok(Value::Int64(extractor(dt)))
            }
        }
    };
}

extract_component_func!(SecondFunc, "second", |dt: DateTime<Utc>| dt.second() as i64);
extract_component_func!(MinuteFunc, "minute", |dt: DateTime<Utc>| dt.minute() as i64);
extract_component_func!(HourFunc, "hour", |dt: DateTime<Utc>| dt.hour() as i64);
extract_component_func!(DayFunc, "day", |dt: DateTime<Utc>| dt.day() as i64);
extract_component_func!(MonthFunc, "month", |dt: DateTime<Utc>| dt.month() as i64);
extract_component_func!(YearFunc, "year", |dt: DateTime<Utc>| dt.year() as i64);
extract_component_func!(MillisecondFunc, "millisecond", |dt: DateTime<Utc>| {
    (dt.nanosecond() / 1_000_000) as i64
});
extract_component_func!(DayOfWeekFunc, "dayOfWeek", |dt: DateTime<Utc>| {
    dt.weekday().number_from_monday() as i64
});
extract_component_func!(DayOfYearFunc, "dayOfYear", |dt: DateTime<Utc>| {
    dt.ordinal() as i64
});

// ---------------------------------------------------------------------------
// toSeconds / toMillis
// ---------------------------------------------------------------------------

struct ToSecondsFunc;

impl ExprFunction for ToSecondsFunc {
    fn name(&self) -> &str {
        "toSeconds"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        timestamp_input_type(args, "toSeconds")?;
        Ok(NullableExprType::new(DataType::Int64, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let dt = extract_timestamp(a, "toSeconds")?;
        Ok(Value::Int64(dt.timestamp()))
    }
}

struct ToMillisFunc;

impl ExprFunction for ToMillisFunc {
    fn name(&self) -> &str {
        "toMillis"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        timestamp_input_type(args, "toMillis")?;
        Ok(NullableExprType::new(DataType::Int64, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let dt = extract_timestamp(a, "toMillis")?;
        Ok(Value::Int64(dt.timestamp_millis()))
    }
}

// ---------------------------------------------------------------------------
// fromSeconds / fromMillis
// ---------------------------------------------------------------------------

struct FromSecondsFunc;

impl ExprFunction for FromSecondsFunc {
    fn name(&self) -> &str {
        "fromSeconds"
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
        let secs = extract_int(a, "fromSeconds")?;
        let dt = DateTime::from_timestamp(secs, 0).ok_or_else(|| FuncError::EvalFailed {
            function: "fromSeconds".to_owned(),
            reason: format!("timestamp {secs}s is out of range"),
        })?;
        Ok(Value::Timestamp(dt))
    }
}

struct FromMillisFunc;

impl ExprFunction for FromMillisFunc {
    fn name(&self) -> &str {
        "fromMillis"
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
        let millis = extract_int(a, "fromMillis")?;
        let dt = DateTime::from_timestamp_millis(millis).ok_or_else(|| FuncError::EvalFailed {
            function: "fromMillis".to_owned(),
            reason: format!("timestamp {millis}ms is out of range"),
        })?;
        Ok(Value::Timestamp(dt))
    }
}

// ---------------------------------------------------------------------------
// Date arithmetic: addX / subtractX
// ---------------------------------------------------------------------------

macro_rules! duration_arithmetic_func {
    ($struct_name:ident, $func_name:expr, $duration_fn:expr) => {
        struct $struct_name;

        impl ExprFunction for $struct_name {
            fn name(&self) -> &str {
                $func_name
            }

            fn min_args(&self) -> usize {
                2
            }

            fn max_args(&self) -> Option<usize> {
                Some(2)
            }

            fn resolve_type(
                &self,
                args: &[NullableExprType],
            ) -> Result<NullableExprType, FuncError> {
                timestamp_input_type(args, $func_name)?;
                let nullable = args.iter().any(|a| a.nullable);
                Ok(NullableExprType::new(DataType::Timestamp, nullable))
            }

            fn evaluate(
                &self,
                mut args: Vec<Value>,
                _context: &EvalContext,
            ) -> Result<Value, FuncError> {
                let b = args.remove(1);
                let a = args.remove(0);
                if a.is_null() || b.is_null() {
                    return Ok(Value::Null);
                }
                let dt = extract_timestamp(a, $func_name)?;
                let n = extract_int(b, $func_name)?;
                let make_duration: fn(i64) -> Option<Duration> = $duration_fn;
                let dur = make_duration(n).ok_or_else(|| FuncError::EvalFailed {
                    function: $func_name.to_owned(),
                    reason: format!("duration value {n} is out of range"),
                })?;
                let result = dt
                    .checked_add_signed(dur)
                    .ok_or_else(|| FuncError::EvalFailed {
                        function: $func_name.to_owned(),
                        reason: format!("result timestamp is out of range"),
                    })?;
                Ok(Value::Timestamp(result))
            }
        }
    };
}

macro_rules! duration_subtract_func {
    ($struct_name:ident, $func_name:expr, $duration_fn:expr) => {
        struct $struct_name;

        impl ExprFunction for $struct_name {
            fn name(&self) -> &str {
                $func_name
            }

            fn min_args(&self) -> usize {
                2
            }

            fn max_args(&self) -> Option<usize> {
                Some(2)
            }

            fn resolve_type(
                &self,
                args: &[NullableExprType],
            ) -> Result<NullableExprType, FuncError> {
                timestamp_input_type(args, $func_name)?;
                let nullable = args.iter().any(|a| a.nullable);
                Ok(NullableExprType::new(DataType::Timestamp, nullable))
            }

            fn evaluate(
                &self,
                mut args: Vec<Value>,
                _context: &EvalContext,
            ) -> Result<Value, FuncError> {
                let b = args.remove(1);
                let a = args.remove(0);
                if a.is_null() || b.is_null() {
                    return Ok(Value::Null);
                }
                let dt = extract_timestamp(a, $func_name)?;
                let n = extract_int(b, $func_name)?;
                let make_duration: fn(i64) -> Option<Duration> = $duration_fn;
                let dur = make_duration(n).ok_or_else(|| FuncError::EvalFailed {
                    function: $func_name.to_owned(),
                    reason: format!("duration value {n} is out of range"),
                })?;
                let result = dt
                    .checked_sub_signed(dur)
                    .ok_or_else(|| FuncError::EvalFailed {
                        function: $func_name.to_owned(),
                        reason: format!("result timestamp is out of range"),
                    })?;
                Ok(Value::Timestamp(result))
            }
        }
    };
}

duration_arithmetic_func!(AddDaysFunc, "addDays", |n| Duration::try_days(n));
duration_arithmetic_func!(AddHoursFunc, "addHours", |n| Duration::try_hours(n));
duration_arithmetic_func!(AddMinutesFunc, "addMinutes", |n| Duration::try_minutes(n));
duration_arithmetic_func!(AddSecondsFunc, "addSeconds", |n| Duration::try_seconds(n));
duration_arithmetic_func!(AddMillisecondsFunc, "addMilliseconds", |n| {
    Duration::try_milliseconds(n)
});

duration_subtract_func!(SubtractDaysFunc, "subtractDays", |n| Duration::try_days(n));
duration_subtract_func!(SubtractHoursFunc, "subtractHours", |n| {
    Duration::try_hours(n)
});
duration_subtract_func!(SubtractMinutesFunc, "subtractMinutes", |n| {
    Duration::try_minutes(n)
});
duration_subtract_func!(SubtractSecondsFunc, "subtractSeconds", |n| {
    Duration::try_seconds(n)
});
duration_subtract_func!(SubtractMillisecondsFunc, "subtractMilliseconds", |n| {
    Duration::try_milliseconds(n)
});

// ---------------------------------------------------------------------------
// addMonths / subtractMonths / addYears / subtractYears
// (chrono::Months-based, not Duration-based)
// ---------------------------------------------------------------------------

struct AddMonthsFunc;

impl ExprFunction for AddMonthsFunc {
    fn name(&self) -> &str {
        "addMonths"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        timestamp_input_type(args, "addMonths")?;
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Timestamp, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        let dt = extract_timestamp(a, "addMonths")?;
        let n = extract_int(b, "addMonths")?;
        add_months_to_dt(dt, n, "addMonths")
    }
}

struct SubtractMonthsFunc;

impl ExprFunction for SubtractMonthsFunc {
    fn name(&self) -> &str {
        "subtractMonths"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        timestamp_input_type(args, "subtractMonths")?;
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Timestamp, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        let dt = extract_timestamp(a, "subtractMonths")?;
        let n = extract_int(b, "subtractMonths")?;
        subtract_months_from_dt(dt, n, "subtractMonths")
    }
}

struct AddYearsFunc;

impl ExprFunction for AddYearsFunc {
    fn name(&self) -> &str {
        "addYears"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        timestamp_input_type(args, "addYears")?;
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Timestamp, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        let dt = extract_timestamp(a, "addYears")?;
        let n = extract_int(b, "addYears")?;
        let months = n.checked_mul(12).ok_or_else(|| FuncError::EvalFailed {
            function: "addYears".to_owned(),
            reason: format!("year value {n} overflows when converted to months"),
        })?;
        add_months_to_dt(dt, months, "addYears")
    }
}

struct SubtractYearsFunc;

impl ExprFunction for SubtractYearsFunc {
    fn name(&self) -> &str {
        "subtractYears"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        timestamp_input_type(args, "subtractYears")?;
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Timestamp, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        let dt = extract_timestamp(a, "subtractYears")?;
        let n = extract_int(b, "subtractYears")?;
        let months = n.checked_mul(12).ok_or_else(|| FuncError::EvalFailed {
            function: "subtractYears".to_owned(),
            reason: format!("year value {n} overflows when converted to months"),
        })?;
        subtract_months_from_dt(dt, months, "subtractYears")
    }
}

fn add_months_to_dt(dt: DateTime<Utc>, n: i64, func: &str) -> Result<Value, FuncError> {
    if n >= 0 {
        let months = u32::try_from(n).map_err(|_| FuncError::EvalFailed {
            function: func.to_owned(),
            reason: format!("month count {n} is out of range"),
        })?;
        let result =
            dt.checked_add_months(Months::new(months))
                .ok_or_else(|| FuncError::EvalFailed {
                    function: func.to_owned(),
                    reason: "result timestamp is out of range".to_owned(),
                })?;
        Ok(Value::Timestamp(result))
    } else {
        let abs_months = u32::try_from(n.unsigned_abs()).map_err(|_| FuncError::EvalFailed {
            function: func.to_owned(),
            reason: format!("month count {n} is out of range"),
        })?;
        let result = dt
            .checked_sub_months(Months::new(abs_months))
            .ok_or_else(|| FuncError::EvalFailed {
                function: func.to_owned(),
                reason: "result timestamp is out of range".to_owned(),
            })?;
        Ok(Value::Timestamp(result))
    }
}

fn subtract_months_from_dt(dt: DateTime<Utc>, n: i64, func: &str) -> Result<Value, FuncError> {
    if n >= 0 {
        let months = u32::try_from(n).map_err(|_| FuncError::EvalFailed {
            function: func.to_owned(),
            reason: format!("month count {n} is out of range"),
        })?;
        let result =
            dt.checked_sub_months(Months::new(months))
                .ok_or_else(|| FuncError::EvalFailed {
                    function: func.to_owned(),
                    reason: "result timestamp is out of range".to_owned(),
                })?;
        Ok(Value::Timestamp(result))
    } else {
        let abs_months = u32::try_from(n.unsigned_abs()).map_err(|_| FuncError::EvalFailed {
            function: func.to_owned(),
            reason: format!("month count {n} is out of range"),
        })?;
        let result = dt
            .checked_add_months(Months::new(abs_months))
            .ok_or_else(|| FuncError::EvalFailed {
                function: func.to_owned(),
                reason: "result timestamp is out of range".to_owned(),
            })?;
        Ok(Value::Timestamp(result))
    }
}

// ---------------------------------------------------------------------------
// dateDiff(unit, dt1, dt2) -> Int64
// ---------------------------------------------------------------------------

struct DateDiffFunc;

impl ExprFunction for DateDiffFunc {
    fn name(&self) -> &str {
        "dateDiff"
    }

    fn min_args(&self) -> usize {
        3
    }

    fn max_args(&self) -> Option<usize> {
        Some(3)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Int64, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let dt2_val = args.remove(2);
        let dt1_val = args.remove(1);
        let unit_val = args.remove(0);
        if unit_val.is_null() || dt1_val.is_null() || dt2_val.is_null() {
            return Ok(Value::Null);
        }
        let unit = extract_string(unit_val, "dateDiff")?;
        let dt1 = extract_timestamp(dt1_val, "dateDiff")?;
        let dt2 = extract_timestamp(dt2_val, "dateDiff")?;
        let diff = dt2 - dt1;
        let result = match unit.as_str() {
            "second" => diff.num_seconds(),
            "minute" => diff.num_minutes(),
            "hour" => diff.num_hours(),
            "day" => diff.num_days(),
            other => {
                return Err(FuncError::EvalFailed {
                    function: "dateDiff".to_owned(),
                    reason: format!(
                        "unsupported unit '{other}', expected one of: second, minute, hour, day"
                    ),
                });
            }
        };
        Ok(Value::Int64(result))
    }
}

// ---------------------------------------------------------------------------
// formatDateTime(dt, mask) -> Text
// ---------------------------------------------------------------------------

struct FormatDateTimeFunc;

impl ExprFunction for FormatDateTimeFunc {
    fn name(&self) -> &str {
        "formatDateTime"
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
        let mask_val = args.remove(1);
        let dt_val = args.remove(0);
        if dt_val.is_null() || mask_val.is_null() {
            return Ok(Value::Null);
        }
        let dt = extract_timestamp(dt_val, "formatDateTime")?;
        let mask = extract_string(mask_val, "formatDateTime")?;
        Ok(Value::Text(dt.format(&mask).to_string()))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use chrono::{NaiveDate, TimeZone, Utc};

    use air_elt_types::Value;

    use crate::ExprFunction;
    use crate::signature::EvalContext;

    use super::*;

    fn ctx() -> EvalContext {
        EvalContext {
            env_resolver: Arc::new(crate::test_support::EmptyEnv),
            file_resolver: Arc::new(crate::test_support::NoopFiles),
            now: Utc.with_ymd_and_hms(2024, 6, 15, 10, 30, 45).unwrap(),
            base_dir: PathBuf::new(),
        }
    }

    fn ts(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> Value {
        Value::Timestamp(Utc.with_ymd_and_hms(y, m, d, h, min, s).unwrap())
    }

    fn date(y: i32, m: u32, d: u32) -> Value {
        Value::Date(NaiveDate::from_ymd_opt(y, m, d).unwrap())
    }

    // -----------------------------------------------------------------------
    // now / today
    // -----------------------------------------------------------------------

    #[test]
    fn now_returns_context_now() {
        let c = ctx();
        let result = NowFunc.evaluate(vec![], &c).unwrap();
        assert_eq!(result, Value::Timestamp(c.now));
    }

    #[test]
    fn today_returns_date_part() {
        let c = ctx();
        let result = TodayFunc.evaluate(vec![], &c).unwrap();
        assert_eq!(
            result,
            Value::Date(NaiveDate::from_ymd_opt(2024, 6, 15).unwrap())
        );
    }

    // -----------------------------------------------------------------------
    // Component extraction
    // -----------------------------------------------------------------------

    #[test]
    fn second_extraction() {
        let result = SecondFunc
            .evaluate(vec![ts(2024, 6, 15, 10, 30, 45)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(45));
    }

    #[test]
    fn minute_extraction() {
        let result = MinuteFunc
            .evaluate(vec![ts(2024, 6, 15, 10, 30, 45)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(30));
    }

    #[test]
    fn hour_extraction() {
        let result = HourFunc
            .evaluate(vec![ts(2024, 6, 15, 10, 30, 45)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(10));
    }

    #[test]
    fn day_extraction() {
        let result = DayFunc
            .evaluate(vec![ts(2024, 6, 15, 10, 30, 45)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(15));
    }

    #[test]
    fn month_extraction() {
        let result = MonthFunc
            .evaluate(vec![ts(2024, 6, 15, 10, 30, 45)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(6));
    }

    #[test]
    fn year_extraction() {
        let result = YearFunc
            .evaluate(vec![ts(2024, 6, 15, 10, 30, 45)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(2024));
    }

    #[test]
    fn millisecond_extraction() {
        let dt = Utc
            .with_ymd_and_hms(2024, 6, 15, 10, 30, 45)
            .unwrap()
            .with_nanosecond(123_000_000)
            .unwrap();
        let result = MillisecondFunc
            .evaluate(vec![Value::Timestamp(dt)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(123));
    }

    #[test]
    fn day_of_week_monday_is_1() {
        // 2024-06-17 is a Monday
        let result = DayOfWeekFunc
            .evaluate(vec![ts(2024, 6, 17, 0, 0, 0)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(1));
    }

    #[test]
    fn day_of_week_sunday_is_7() {
        // 2024-06-16 is a Sunday
        let result = DayOfWeekFunc
            .evaluate(vec![ts(2024, 6, 16, 0, 0, 0)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(7));
    }

    #[test]
    fn day_of_year_extraction() {
        // 2024-06-15 is day 167 (leap year)
        let result = DayOfYearFunc
            .evaluate(vec![ts(2024, 6, 15, 0, 0, 0)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(167));
    }

    // -----------------------------------------------------------------------
    // toSeconds / toMillis
    // -----------------------------------------------------------------------

    #[test]
    fn to_seconds_conversion() {
        let dt = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let result = ToSecondsFunc
            .evaluate(vec![Value::Timestamp(dt)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(dt.timestamp()));
    }

    #[test]
    fn to_millis_conversion() {
        let dt = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let result = ToMillisFunc
            .evaluate(vec![Value::Timestamp(dt)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(dt.timestamp_millis()));
    }

    // -----------------------------------------------------------------------
    // fromSeconds / fromMillis roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn from_seconds_roundtrip() {
        let original = Utc.with_ymd_and_hms(2024, 6, 15, 10, 30, 45).unwrap();
        let secs = original.timestamp();
        let result = FromSecondsFunc
            .evaluate(vec![Value::Int64(secs)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Timestamp(original));
    }

    #[test]
    fn from_millis_roundtrip() {
        let original = Utc.with_ymd_and_hms(2024, 6, 15, 10, 30, 45).unwrap();
        let millis = original.timestamp_millis();
        let result = FromMillisFunc
            .evaluate(vec![Value::Int64(millis)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Timestamp(original));
    }

    // -----------------------------------------------------------------------
    // addDays / subtractDays
    // -----------------------------------------------------------------------

    #[test]
    fn add_days_basic() {
        let result = AddDaysFunc
            .evaluate(vec![ts(2024, 6, 15, 10, 0, 0), Value::Int64(5)], &ctx())
            .unwrap();
        assert_eq!(result, ts(2024, 6, 20, 10, 0, 0));
    }

    #[test]
    fn subtract_days_basic() {
        let result = SubtractDaysFunc
            .evaluate(vec![ts(2024, 6, 15, 10, 0, 0), Value::Int64(5)], &ctx())
            .unwrap();
        assert_eq!(result, ts(2024, 6, 10, 10, 0, 0));
    }

    #[test]
    fn add_hours_basic() {
        let result = AddHoursFunc
            .evaluate(vec![ts(2024, 6, 15, 10, 0, 0), Value::Int64(3)], &ctx())
            .unwrap();
        assert_eq!(result, ts(2024, 6, 15, 13, 0, 0));
    }

    #[test]
    fn add_months_basic() {
        let result = AddMonthsFunc
            .evaluate(vec![ts(2024, 6, 15, 10, 0, 0), Value::Int64(2)], &ctx())
            .unwrap();
        assert_eq!(result, ts(2024, 8, 15, 10, 0, 0));
    }

    #[test]
    fn subtract_months_basic() {
        let result = SubtractMonthsFunc
            .evaluate(vec![ts(2024, 6, 15, 10, 0, 0), Value::Int64(3)], &ctx())
            .unwrap();
        assert_eq!(result, ts(2024, 3, 15, 10, 0, 0));
    }

    #[test]
    fn add_years_basic() {
        let result = AddYearsFunc
            .evaluate(vec![ts(2024, 6, 15, 10, 0, 0), Value::Int64(2)], &ctx())
            .unwrap();
        assert_eq!(result, ts(2026, 6, 15, 10, 0, 0));
    }

    #[test]
    fn subtract_years_basic() {
        let result = SubtractYearsFunc
            .evaluate(vec![ts(2024, 6, 15, 10, 0, 0), Value::Int64(1)], &ctx())
            .unwrap();
        assert_eq!(result, ts(2023, 6, 15, 10, 0, 0));
    }

    // -----------------------------------------------------------------------
    // dateDiff
    // -----------------------------------------------------------------------

    #[test]
    fn date_diff_days() {
        let result = DateDiffFunc
            .evaluate(
                vec![
                    Value::Text("day".into()),
                    ts(2024, 6, 10, 0, 0, 0),
                    ts(2024, 6, 15, 0, 0, 0),
                ],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Int64(5));
    }

    #[test]
    fn date_diff_hours() {
        let result = DateDiffFunc
            .evaluate(
                vec![
                    Value::Text("hour".into()),
                    ts(2024, 6, 15, 10, 0, 0),
                    ts(2024, 6, 15, 13, 0, 0),
                ],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Int64(3));
    }

    #[test]
    fn date_diff_invalid_unit() {
        let result = DateDiffFunc.evaluate(
            vec![
                Value::Text("week".into()),
                ts(2024, 6, 10, 0, 0, 0),
                ts(2024, 6, 15, 0, 0, 0),
            ],
            &ctx(),
        );
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // formatDateTime
    // -----------------------------------------------------------------------

    #[test]
    fn format_date_time_basic() {
        let result = FormatDateTimeFunc
            .evaluate(
                vec![
                    ts(2024, 6, 15, 10, 30, 45),
                    Value::Text("%Y-%m-%d %H:%M:%S".into()),
                ],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Text("2024-06-15 10:30:45".into()));
    }

    #[test]
    fn format_date_time_date_only() {
        let result = FormatDateTimeFunc
            .evaluate(
                vec![ts(2024, 6, 15, 10, 30, 45), Value::Text("%Y-%m-%d".into())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Text("2024-06-15".into()));
    }

    // -----------------------------------------------------------------------
    // Null propagation
    // -----------------------------------------------------------------------

    #[test]
    fn null_propagation_extraction() {
        let result = SecondFunc.evaluate(vec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn null_propagation_add_days() {
        let result = AddDaysFunc
            .evaluate(vec![Value::Null, Value::Int64(5)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn null_propagation_add_days_second_arg() {
        let result = AddDaysFunc
            .evaluate(vec![ts(2024, 6, 15, 10, 0, 0), Value::Null], &ctx())
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn null_propagation_date_diff() {
        let result = DateDiffFunc
            .evaluate(
                vec![
                    Value::Text("day".into()),
                    Value::Null,
                    ts(2024, 6, 15, 0, 0, 0),
                ],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn null_propagation_format() {
        let result = FormatDateTimeFunc
            .evaluate(vec![Value::Null, Value::Text("%Y".into())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    // -----------------------------------------------------------------------
    // Date input (not just Timestamp)
    // -----------------------------------------------------------------------

    #[test]
    fn extract_from_date_value() {
        let result = YearFunc.evaluate(vec![date(2024, 6, 15)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(2024));
    }

    #[test]
    fn add_days_from_date_value() {
        let result = AddDaysFunc
            .evaluate(vec![date(2024, 6, 15), Value::Int64(1)], &ctx())
            .unwrap();
        assert_eq!(result, ts(2024, 6, 16, 0, 0, 0));
    }

    #[test]
    fn to_seconds_from_date() {
        let result = ToSecondsFunc
            .evaluate(vec![date(2024, 1, 1)], &ctx())
            .unwrap();
        let expected = Utc
            .with_ymd_and_hms(2024, 1, 1, 0, 0, 0)
            .unwrap()
            .timestamp();
        assert_eq!(result, Value::Int64(expected));
    }
}
