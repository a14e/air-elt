use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{ArgWindow, EvalContext, ExprFunction};

static SIN: SinFunc = SinFunc;
static COS: CosFunc = CosFunc;
static TAN: TanFunc = TanFunc;
static ASIN: AsinFunc = AsinFunc;
static ACOS: AcosFunc = AcosFunc;
static ATAN: AtanFunc = AtanFunc;
static ATAN2: Atan2Func = Atan2Func;
static LOG: LogFunc = LogFunc;
static LOG2: Log2Func = Log2Func;
static LOG10: Log10Func = Log10Func;
static EXP: ExpFunc = ExpFunc;
static CBRT: CbrtFunc = CbrtFunc;
static PI: PiFunc = PiFunc;
static E: EFunc = EFunc;
static ERF: ErfFunc = ErfFunc;
static ERFC: ErfcFunc = ErfcFunc;
static GAMMA: GammaFunc = GammaFunc;
static LN_GAMMA: LnGammaFunc = LnGammaFunc;
static LAMBERT_W: LambertWFunc = LambertWFunc;
static SINH: SinhFunc = SinhFunc;
static COSH: CoshFunc = CoshFunc;
static TANH: TanhFunc = TanhFunc;
static ASINH: AsinhFunc = AsinhFunc;
static ACOSH: AcoshFunc = AcoshFunc;
static ATANH: AtanhFunc = AtanhFunc;
static PHI: PhiFunc = PhiFunc;
static TAU: TauFunc = TauFunc;
static IS_NAN: IsNanFunc = IsNanFunc;
static IS_INFINITE: IsInfiniteFunc = IsInfiniteFunc;
static CLAMP: ClampFunc = ClampFunc;
static BETA: BetaFunc = BetaFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&SIN);
    registry.register(&COS);
    registry.register(&TAN);
    registry.register(&ASIN);
    registry.register(&ACOS);
    registry.register(&ATAN);
    registry.register(&ATAN2);
    registry.register(&LOG);
    registry.register(&LOG2);
    registry.register(&LOG10);
    registry.register(&EXP);
    registry.register(&CBRT);
    registry.register(&PI);
    registry.register(&E);
    registry.register(&ERF);
    registry.register(&ERFC);
    registry.register(&GAMMA);
    registry.register(&LN_GAMMA);
    registry.register(&LAMBERT_W);
    registry.register(&SINH);
    registry.register(&COSH);
    registry.register(&TANH);
    registry.register(&ASINH);
    registry.register(&ACOSH);
    registry.register(&ATANH);
    registry.register(&PHI);
    registry.register(&TAU);
    registry.register(&IS_NAN);
    registry.register(&IS_INFINITE);
    registry.register(&CLAMP);
    registry.register(&BETA);
}

fn to_f64(val: &Value, func_name: &str) -> Result<f64, FuncError> {
    match val {
        Value::Int64(x) => Ok(*x as f64),
        Value::Float64(x) => Ok(*x),
        other => Err(FuncError::TypeMismatch {
            function: func_name.to_owned(),
            expected: "numeric".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

// ---------------------------------------------------------------------------
// Trigonometry
// ---------------------------------------------------------------------------

struct SinFunc;

impl ExprFunction for SinFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "sin"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "sin")?;
        Ok(Value::Float64(x.sin()))
    }
}

struct CosFunc;

impl ExprFunction for CosFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "cos"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "cos")?;
        Ok(Value::Float64(x.cos()))
    }
}

struct TanFunc;

impl ExprFunction for TanFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "tan"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "tan")?;
        Ok(Value::Float64(x.tan()))
    }
}

struct AsinFunc;

impl ExprFunction for AsinFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "asin"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "asin")?;
        Ok(Value::Float64(x.asin()))
    }
}

struct AcosFunc;

impl ExprFunction for AcosFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "acos"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "acos")?;
        Ok(Value::Float64(x.acos()))
    }
}

struct AtanFunc;

impl ExprFunction for AtanFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "atan"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "atan")?;
        Ok(Value::Float64(x.atan()))
    }
}

struct Atan2Func;

impl ExprFunction for Atan2Func {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "atan2"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Float64, nullable))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let y = args.read(0);
        let x = args.read(1);
        if y.is_null() || x.is_null() {
            return Ok(Value::Null);
        }
        let y_val = to_f64(y, "atan2")?;
        let x_val = to_f64(x, "atan2")?;
        Ok(Value::Float64(y_val.atan2(x_val)))
    }
}

// ---------------------------------------------------------------------------
// Logarithms / Exponential
// ---------------------------------------------------------------------------

struct LogFunc;

impl ExprFunction for LogFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "log"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "log")?;
        Ok(Value::Float64(x.ln()))
    }
}

struct Log2Func;

impl ExprFunction for Log2Func {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "log2"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "log2")?;
        Ok(Value::Float64(x.log2()))
    }
}

struct Log10Func;

impl ExprFunction for Log10Func {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "log10"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "log10")?;
        Ok(Value::Float64(x.log10()))
    }
}

struct ExpFunc;

impl ExprFunction for ExpFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "exp"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "exp")?;
        Ok(Value::Float64(x.exp()))
    }
}

// ---------------------------------------------------------------------------
// Roots
// ---------------------------------------------------------------------------

struct CbrtFunc;

impl ExprFunction for CbrtFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "cbrt"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "cbrt")?;
        Ok(Value::Float64(x.cbrt()))
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

struct PiFunc;

impl ExprFunction for PiFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "pi"
    }

    fn min_args(&self) -> usize {
        0
    }

    fn max_args(&self) -> Option<usize> {
        Some(0)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Float64, false))
    }

    fn evaluate(
        &self,
        _args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        Ok(Value::Float64(std::f64::consts::PI))
    }
}

struct EFunc;

impl ExprFunction for EFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "e"
    }

    fn min_args(&self) -> usize {
        0
    }

    fn max_args(&self) -> Option<usize> {
        Some(0)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Float64, false))
    }

    fn evaluate(
        &self,
        _args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        Ok(Value::Float64(std::f64::consts::E))
    }
}

struct PhiFunc;

impl ExprFunction for PhiFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "phi"
    }

    fn min_args(&self) -> usize {
        0
    }

    fn max_args(&self) -> Option<usize> {
        Some(0)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Float64, false))
    }

    fn evaluate(
        &self,
        _args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        // Golden ratio: (1 + sqrt(5)) / 2
        Ok(Value::Float64((1.0 + 5.0_f64.sqrt()) / 2.0))
    }
}

struct TauFunc;

impl ExprFunction for TauFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "tau"
    }

    fn min_args(&self) -> usize {
        0
    }

    fn max_args(&self) -> Option<usize> {
        Some(0)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Float64, false))
    }

    fn evaluate(
        &self,
        _args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        Ok(Value::Float64(std::f64::consts::TAU))
    }
}

// ---------------------------------------------------------------------------
// Error Function family (via statrs)
// ---------------------------------------------------------------------------

struct ErfFunc;

impl ExprFunction for ErfFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "erf"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "erf")?;
        Ok(Value::Float64(statrs::function::erf::erf(x)))
    }
}

struct ErfcFunc;

impl ExprFunction for ErfcFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "erfc"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "erfc")?;
        Ok(Value::Float64(statrs::function::erf::erfc(x)))
    }
}

// ---------------------------------------------------------------------------
// Gamma Function family (via statrs)
// ---------------------------------------------------------------------------

struct GammaFunc;

impl ExprFunction for GammaFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "gamma"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "gamma")?;
        Ok(Value::Float64(statrs::function::gamma::gamma(x)))
    }
}

struct LnGammaFunc;

impl ExprFunction for LnGammaFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "lnGamma"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "lnGamma")?;
        Ok(Value::Float64(statrs::function::gamma::ln_gamma(x)))
    }
}

// ---------------------------------------------------------------------------
// Beta Function (via statrs)
// ---------------------------------------------------------------------------

struct BetaFunc;

impl ExprFunction for BetaFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "beta"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Float64, nullable))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a_val = args.read(0);
        let b_val = args.read(1);
        if a_val.is_null() || b_val.is_null() {
            return Ok(Value::Null);
        }
        let a = to_f64(a_val, "beta")?;
        let b = to_f64(b_val, "beta")?;
        Ok(Value::Float64(statrs::function::beta::beta(a, b)))
    }
}

// ---------------------------------------------------------------------------
// Lambert W Function (via lambert_w crate)
// ---------------------------------------------------------------------------

struct LambertWFunc;

impl ExprFunction for LambertWFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "lambertW"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "lambertW")?;
        Ok(Value::Float64(lambert_w::lambert_w0(x)))
    }
}

// ---------------------------------------------------------------------------
// Hyperbolic Functions
// ---------------------------------------------------------------------------

struct SinhFunc;

impl ExprFunction for SinhFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "sinh"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "sinh")?;
        Ok(Value::Float64(x.sinh()))
    }
}

struct CoshFunc;

impl ExprFunction for CoshFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "cosh"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "cosh")?;
        Ok(Value::Float64(x.cosh()))
    }
}

struct TanhFunc;

impl ExprFunction for TanhFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "tanh"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "tanh")?;
        Ok(Value::Float64(x.tanh()))
    }
}

struct AsinhFunc;

impl ExprFunction for AsinhFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "asinh"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "asinh")?;
        Ok(Value::Float64(x.asinh()))
    }
}

struct AcoshFunc;

impl ExprFunction for AcoshFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "acosh"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "acosh")?;
        Ok(Value::Float64(x.acosh()))
    }
}

struct AtanhFunc;

impl ExprFunction for AtanhFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "atanh"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "atanh")?;
        Ok(Value::Float64(x.atanh()))
    }
}

// ---------------------------------------------------------------------------
// Numeric Utilities
// ---------------------------------------------------------------------------

struct IsNanFunc;

impl ExprFunction for IsNanFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "isNaN"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "isNaN")?;
        Ok(Value::Bool(x.is_nan()))
    }
}

struct IsInfiniteFunc;

impl ExprFunction for IsInfiniteFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "isInfinite"
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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "isInfinite")?;
        Ok(Value::Bool(x.is_infinite()))
    }
}

struct ClampFunc;

impl ExprFunction for ClampFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "clamp"
    }

    fn min_args(&self) -> usize {
        3
    }

    fn max_args(&self) -> Option<usize> {
        Some(3)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Float64, nullable))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let x_val = args.read(0);
        let min_val = args.read(1);
        let max_val = args.read(2);
        if x_val.is_null() || min_val.is_null() || max_val.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(x_val, "clamp")?;
        let min = to_f64(min_val, "clamp")?;
        let max = to_f64(max_val, "clamp")?;
        Ok(Value::Float64(x.clamp(min, max)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_support::{ctx, eval};

    #[test]
    fn sin_zero() {
        let result = eval(&SIN, smallvec::smallvec![Value::Float64(0.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(0.0));
    }

    #[test]
    fn cos_zero() {
        let result = eval(&COS, smallvec::smallvec![Value::Float64(0.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(1.0));
    }

    #[test]
    fn tan_zero() {
        let result = eval(&TAN, smallvec::smallvec![Value::Float64(0.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(0.0));
    }

    #[test]
    fn sin_null_propagation() {
        let result = eval(&SIN, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn cos_null_propagation() {
        let result = eval(&COS, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn atan2_basic() {
        let result = eval(
            &ATAN2,
            smallvec::smallvec![Value::Float64(1.0), Value::Float64(1.0)],
            &ctx(),
        )
        .unwrap();
        match result {
            Value::Float64(v) => {
                let expected = std::f64::consts::FRAC_PI_4;
                assert!(
                    (v - expected).abs() < 1e-10,
                    "atan2(1,1) = {v}, expected {expected}"
                );
            }
            other => panic!("expected Float64, got {other:?}"),
        }
    }

    #[test]
    fn atan2_null_propagation() {
        let result = eval(
            &ATAN2,
            smallvec::smallvec![Value::Null, Value::Float64(1.0)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn log_one() {
        let result = eval(&LOG, smallvec::smallvec![Value::Float64(1.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(0.0));
    }

    #[test]
    fn log_e() {
        let result = eval(
            &LOG,
            smallvec::smallvec![Value::Float64(std::f64::consts::E)],
            &ctx(),
        )
        .unwrap();
        match result {
            Value::Float64(v) => {
                assert!((v - 1.0).abs() < 1e-10, "log(e) = {v}, expected 1.0");
            }
            other => panic!("expected Float64, got {other:?}"),
        }
    }

    #[test]
    fn log2_basic() {
        let result = eval(&LOG2, smallvec::smallvec![Value::Float64(8.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(3.0));
    }

    #[test]
    fn log10_basic() {
        let result = eval(&LOG10, smallvec::smallvec![Value::Float64(1000.0)], &ctx()).unwrap();
        match result {
            Value::Float64(v) => {
                assert!((v - 3.0).abs() < 1e-10, "log10(1000) = {v}, expected 3.0");
            }
            other => panic!("expected Float64, got {other:?}"),
        }
    }

    #[test]
    fn exp_zero() {
        let result = eval(&EXP, smallvec::smallvec![Value::Float64(0.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(1.0));
    }

    #[test]
    fn exp_one() {
        let result = eval(&EXP, smallvec::smallvec![Value::Float64(1.0)], &ctx()).unwrap();
        match result {
            Value::Float64(v) => {
                let expected = std::f64::consts::E;
                assert!(
                    (v - expected).abs() < 1e-10,
                    "exp(1) = {v}, expected {expected}"
                );
            }
            other => panic!("expected Float64, got {other:?}"),
        }
    }

    #[test]
    fn cbrt_basic() {
        let result = eval(&CBRT, smallvec::smallvec![Value::Float64(27.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(3.0));
    }

    #[test]
    fn cbrt_int_input() {
        let result = eval(&CBRT, smallvec::smallvec![Value::Int64(8)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(2.0));
    }

    #[test]
    fn pi_constant() {
        let result = eval(&PI, smallvec::smallvec![], &ctx()).unwrap();
        match result {
            Value::Float64(v) => {
                assert!(
                    (v - std::f64::consts::PI).abs() < 1e-15,
                    "pi() = {v}, expected {}",
                    std::f64::consts::PI
                );
            }
            other => panic!("expected Float64, got {other:?}"),
        }
    }

    #[test]
    fn e_constant() {
        let result = eval(&E, smallvec::smallvec![], &ctx()).unwrap();
        match result {
            Value::Float64(v) => {
                assert!(
                    (v - std::f64::consts::E).abs() < 1e-15,
                    "e() = {v}, expected {}",
                    std::f64::consts::E
                );
            }
            other => panic!("expected Float64, got {other:?}"),
        }
    }

    #[test]
    fn asin_zero() {
        let result = eval(&ASIN, smallvec::smallvec![Value::Float64(0.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(0.0));
    }

    #[test]
    fn acos_one() {
        let result = eval(&ACOS, smallvec::smallvec![Value::Float64(1.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(0.0));
    }

    #[test]
    fn atan_zero() {
        let result = eval(&ATAN, smallvec::smallvec![Value::Float64(0.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(0.0));
    }

    #[test]
    fn log_null_propagation() {
        let result = eval(&LOG, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn exp_null_propagation() {
        let result = eval(&EXP, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn cbrt_null_propagation() {
        let result = eval(&CBRT, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    // --- Error function tests ---

    #[test]
    fn erf_zero() {
        let result = eval(&ERF, smallvec::smallvec![Value::Float64(0.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(0.0));
    }

    #[test]
    fn erf_one() {
        let result = eval(&ERF, smallvec::smallvec![Value::Float64(1.0)], &ctx()).unwrap();
        match result {
            Value::Float64(v) => {
                assert!((v - 0.8427).abs() < 1e-4, "erf(1) = {v}, expected ~0.8427");
            }
            other => panic!("expected Float64, got {other:?}"),
        }
    }

    #[test]
    fn erf_null_propagation() {
        let result = eval(&ERF, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn erfc_zero() {
        let result = eval(&ERFC, smallvec::smallvec![Value::Float64(0.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(1.0));
    }

    #[test]
    fn erfc_null_propagation() {
        let result = eval(&ERFC, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    // --- Gamma function tests ---

    #[test]
    fn gamma_five() {
        // Gamma(5) = 4! = 24
        let result = eval(&GAMMA, smallvec::smallvec![Value::Float64(5.0)], &ctx()).unwrap();
        match result {
            Value::Float64(v) => {
                assert!((v - 24.0).abs() < 1e-10, "gamma(5) = {v}, expected 24.0");
            }
            other => panic!("expected Float64, got {other:?}"),
        }
    }

    #[test]
    fn gamma_half() {
        // Gamma(0.5) = sqrt(pi)
        let result = eval(&GAMMA, smallvec::smallvec![Value::Float64(0.5)], &ctx()).unwrap();
        let expected = std::f64::consts::PI.sqrt();
        match result {
            Value::Float64(v) => {
                assert!(
                    (v - expected).abs() < 1e-10,
                    "gamma(0.5) = {v}, expected {expected}"
                );
            }
            other => panic!("expected Float64, got {other:?}"),
        }
    }

    #[test]
    fn gamma_null_propagation() {
        let result = eval(&GAMMA, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn ln_gamma_one() {
        // lnGamma(1) = ln(Gamma(1)) = ln(1) = 0
        let result = eval(&LN_GAMMA, smallvec::smallvec![Value::Float64(1.0)], &ctx()).unwrap();
        match result {
            Value::Float64(v) => {
                assert!(v.abs() < 1e-10, "lnGamma(1) = {v}, expected 0.0");
            }
            other => panic!("expected Float64, got {other:?}"),
        }
    }

    #[test]
    fn ln_gamma_null_propagation() {
        let result = eval(&LN_GAMMA, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    // --- Beta function tests ---

    #[test]
    fn beta_one_one() {
        // B(1, 1) = 1
        let result = eval(
            &BETA,
            smallvec::smallvec![Value::Float64(1.0), Value::Float64(1.0)],
            &ctx(),
        )
        .unwrap();
        match result {
            Value::Float64(v) => {
                assert!((v - 1.0).abs() < 1e-10, "beta(1,1) = {v}, expected 1.0");
            }
            other => panic!("expected Float64, got {other:?}"),
        }
    }

    #[test]
    fn beta_two_three() {
        // B(2, 3) = Gamma(2)*Gamma(3)/Gamma(5) = 1*2/24 = 1/12 ~ 0.0833
        let result = eval(
            &BETA,
            smallvec::smallvec![Value::Float64(2.0), Value::Float64(3.0)],
            &ctx(),
        )
        .unwrap();
        match result {
            Value::Float64(v) => {
                assert!(
                    (v - 1.0 / 12.0).abs() < 1e-10,
                    "beta(2,3) = {v}, expected ~0.0833"
                );
            }
            other => panic!("expected Float64, got {other:?}"),
        }
    }

    #[test]
    fn beta_null_propagation() {
        let result = eval(
            &BETA,
            smallvec::smallvec![Value::Null, Value::Float64(1.0)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Null);
    }

    // --- Lambert W tests ---

    #[test]
    fn lambert_w_zero() {
        let result = eval(&LAMBERT_W, smallvec::smallvec![Value::Float64(0.0)], &ctx()).unwrap();
        match result {
            Value::Float64(v) => {
                assert!(v.abs() < 1e-10, "lambertW(0) = {v}, expected 0.0");
            }
            other => panic!("expected Float64, got {other:?}"),
        }
    }

    #[test]
    fn lambert_w_one() {
        // W(1) ~ 0.5671
        let result = eval(&LAMBERT_W, smallvec::smallvec![Value::Float64(1.0)], &ctx()).unwrap();
        match result {
            Value::Float64(v) => {
                assert!(
                    (v - 0.5671).abs() < 1e-4,
                    "lambertW(1) = {v}, expected ~0.5671"
                );
            }
            other => panic!("expected Float64, got {other:?}"),
        }
    }

    #[test]
    fn lambert_w_e() {
        // W(e) = 1
        let result = eval(
            &LAMBERT_W,
            smallvec::smallvec![Value::Float64(std::f64::consts::E)],
            &ctx(),
        )
        .unwrap();
        match result {
            Value::Float64(v) => {
                assert!((v - 1.0).abs() < 1e-10, "lambertW(e) = {v}, expected 1.0");
            }
            other => panic!("expected Float64, got {other:?}"),
        }
    }

    #[test]
    fn lambert_w_null_propagation() {
        let result = eval(&LAMBERT_W, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    // --- Hyperbolic function tests ---

    #[test]
    fn sinh_zero() {
        let result = eval(&SINH, smallvec::smallvec![Value::Float64(0.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(0.0));
    }

    #[test]
    fn cosh_zero() {
        let result = eval(&COSH, smallvec::smallvec![Value::Float64(0.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(1.0));
    }

    #[test]
    fn tanh_zero() {
        let result = eval(&TANH, smallvec::smallvec![Value::Float64(0.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(0.0));
    }

    #[test]
    fn sinh_null_propagation() {
        let result = eval(&SINH, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn cosh_null_propagation() {
        let result = eval(&COSH, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn tanh_null_propagation() {
        let result = eval(&TANH, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn asinh_zero() {
        let result = eval(&ASINH, smallvec::smallvec![Value::Float64(0.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(0.0));
    }

    #[test]
    fn acosh_one() {
        let result = eval(&ACOSH, smallvec::smallvec![Value::Float64(1.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(0.0));
    }

    #[test]
    fn atanh_zero() {
        let result = eval(&ATANH, smallvec::smallvec![Value::Float64(0.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(0.0));
    }

    // --- Special constants tests ---

    #[test]
    fn phi_constant() {
        let result = eval(&PHI, smallvec::smallvec![], &ctx()).unwrap();
        match result {
            Value::Float64(v) => {
                assert!(
                    (v - 1.618_033_988_749_895).abs() < 1e-12,
                    "phi() = {v}, expected ~1.618"
                );
            }
            other => panic!("expected Float64, got {other:?}"),
        }
    }

    #[test]
    fn tau_constant() {
        let result = eval(&TAU, smallvec::smallvec![], &ctx()).unwrap();
        match result {
            Value::Float64(v) => {
                assert!(
                    (v - std::f64::consts::TAU).abs() < 1e-15,
                    "tau() = {v}, expected {}",
                    std::f64::consts::TAU
                );
            }
            other => panic!("expected Float64, got {other:?}"),
        }
    }

    // --- Numeric utilities tests ---

    #[test]
    fn is_nan_with_nan() {
        let result = eval(
            &IS_NAN,
            smallvec::smallvec![Value::Float64(f64::NAN)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn is_nan_with_normal() {
        let result = eval(&IS_NAN, smallvec::smallvec![Value::Float64(1.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn is_nan_null_propagation() {
        let result = eval(&IS_NAN, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn is_infinite_with_inf() {
        let result = eval(
            &IS_INFINITE,
            smallvec::smallvec![Value::Float64(f64::INFINITY)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn is_infinite_with_neg_inf() {
        let result = eval(
            &IS_INFINITE,
            smallvec::smallvec![Value::Float64(f64::NEG_INFINITY)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn is_infinite_with_normal() {
        let result = eval(
            &IS_INFINITE,
            smallvec::smallvec![Value::Float64(1.0)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn is_infinite_null_propagation() {
        let result = eval(&IS_INFINITE, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn clamp_above_max() {
        let result = eval(
            &CLAMP,
            smallvec::smallvec![
                Value::Float64(5.0),
                Value::Float64(0.0),
                Value::Float64(3.0),
            ],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Float64(3.0));
    }

    #[test]
    fn clamp_below_min() {
        let result = eval(
            &CLAMP,
            smallvec::smallvec![
                Value::Float64(-1.0),
                Value::Float64(0.0),
                Value::Float64(10.0),
            ],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Float64(0.0));
    }

    #[test]
    fn clamp_within_range() {
        let result = eval(
            &CLAMP,
            smallvec::smallvec![
                Value::Float64(5.0),
                Value::Float64(0.0),
                Value::Float64(10.0),
            ],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Float64(5.0));
    }

    #[test]
    fn clamp_null_propagation() {
        let result = eval(
            &CLAMP,
            smallvec::smallvec![Value::Null, Value::Float64(0.0), Value::Float64(10.0)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn clamp_int_inputs() {
        let result = eval(
            &CLAMP,
            smallvec::smallvec![Value::Int64(5), Value::Int64(0), Value::Int64(3)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Float64(3.0));
    }
}
