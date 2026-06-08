use core::convert::Infallible;

use rand::distr::{Alphanumeric, SampleString};
use rand::rngs::{StdRng, ThreadRng};
use rand::{Rng, RngExt, SeedableRng, TryRng};
use uuid::Builder;

use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{ArgWindow, EvalContext, ExprFunction};

/// Maximum length for random string generation (randomAlphanumeric, randomHex).
const MAX_STRING_LENGTH: i64 = 1024;

/// Maximum length for random bytes generation.
const MAX_BYTES_LENGTH: i64 = 1_048_576;

static RANDOM_UUID: RandomUuidFunc = RandomUuidFunc;
static RANDOM_INT: RandomIntFunc = RandomIntFunc;
static RANDOM_FLOAT: RandomFloatFunc = RandomFloatFunc;
static RANDOM_ALPHANUMERIC: RandomAlphanumericFunc = RandomAlphanumericFunc;
static RANDOM_HEX: RandomHexFunc = RandomHexFunc;
static RANDOM_BYTES: RandomBytesFunc = RandomBytesFunc;
static RANDOM_CHOICE: RandomChoiceFunc = RandomChoiceFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&RANDOM_UUID);
    registry.register(&RANDOM_INT);
    registry.register(&RANDOM_FLOAT);
    registry.register(&RANDOM_ALPHANUMERIC);
    registry.register(&RANDOM_HEX);
    registry.register(&RANDOM_BYTES);
    registry.register(&RANDOM_CHOICE);
}

/// Random source for the `random*` builtins. A constant `seed` argument
/// makes the source deterministic — which is what lets the optimizer
/// const-fold a seeded call (see each function's `purity`). Without a seed
/// the per-row thread RNG is used and the call stays impure.
enum Prng {
    Thread(ThreadRng),
    // Boxed because `StdRng` is ~320 bytes — keeping it inline makes every
    // `Prng` that large even for the common thread-RNG variant.
    Seeded(Box<StdRng>),
}

impl Prng {
    /// `Some(seed)` → deterministic `StdRng`; `None` → thread RNG.
    fn new(seed: Option<i64>) -> Self {
        match seed {
            Some(s) => Prng::Seeded(Box::new(StdRng::seed_from_u64(s as u64))),
            None => Prng::Thread(rand::rng()),
        }
    }
}

// Delegate the fallible core trait to the inner source. `Rng` (infallible)
// is then provided by the blanket `impl Rng for R: TryRng<Error=Infallible>`,
// and `RngExt` (random_range/fill/…) by `impl RngExt for R: Rng` — so the
// existing call sites work unchanged on both variants.
impl TryRng for Prng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        Ok(match self {
            Prng::Thread(r) => r.next_u32(),
            Prng::Seeded(r) => r.next_u32(),
        })
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        Ok(match self {
            Prng::Thread(r) => r.next_u64(),
            Prng::Seeded(r) => r.next_u64(),
        })
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        match self {
            Prng::Thread(r) => r.fill_bytes(dst),
            Prng::Seeded(r) => r.fill_bytes(dst),
        }
        Ok(())
    }
}

/// Outcome of inspecting the optional trailing `seed` argument.
enum SeedArg {
    /// No seed slot present — use the thread RNG (impure).
    None,
    /// Constant/explicit `Int64` seed — deterministic RNG.
    Seed(i64),
    /// Seed was `Null` — the whole call short-circuits to `Null`, so a
    /// const-folded null-seeded call is deterministic (always `Null`).
    Null,
}

/// Pop an optional trailing `seed` argument when `args.len()` exceeds the
/// function's base arity.
fn take_seed(
    function: &str,
    args: &mut dyn ArgWindow,
    base_arity: usize,
) -> Result<SeedArg, FuncError> {
    if args.len() <= base_arity {
        return Ok(SeedArg::None);
    }
    match args.take(base_arity) {
        Value::Int64(s) => Ok(SeedArg::Seed(s)),
        Value::Null => Ok(SeedArg::Null),
        other => Err(FuncError::TypeMismatch {
            function: function.to_owned(),
            expected: "Int64 seed".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

impl SeedArg {
    /// `Some(rng)` to proceed, or `None` when the seed was `Null` and the
    /// call must return `Null`.
    fn rng(self) -> Option<Prng> {
        match self {
            SeedArg::None => Some(Prng::new(None)),
            SeedArg::Seed(s) => Some(Prng::new(Some(s))),
            SeedArg::Null => None,
        }
    }
}

/// Purity for a `random*` function: pure iff a `seed` argument is present
/// (so the call is deterministic) and that seed is itself constant.
/// `base_arity` is the argument count without the seed.
fn seeded_purity(const_args: &[bool], base_arity: usize) -> bool {
    const_args.len() == base_arity + 1 && const_args[base_arity]
}

/// Validates the optional trailing `seed` slot of a constant argument list.
/// The slot is present only when `args.len()` exceeds `base_arity`; if that
/// argument is a constant that is neither `Int64` nor `Null`, it cannot be a
/// valid seed — surface the same `TypeMismatch` that [`take_seed`] raises at
/// runtime. Dynamic seeds (`None`) are skipped.
fn validate_seed_const(
    function: &str,
    args: &[Option<&Value>],
    base_arity: usize,
) -> Result<(), FuncError> {
    if args.len() <= base_arity {
        return Ok(());
    }
    if let Some(seed) = args[base_arity] {
        if !matches!(seed, Value::Int64(_) | Value::Null) {
            return Err(FuncError::TypeMismatch {
                function: function.to_owned(),
                expected: "Int64 seed".to_owned(),
                actual: format!("{:?}", seed.data_type()),
            });
        }
    }
    Ok(())
}

/// Rejects a `min`/`max` pair where `min > max`, mirroring the requirement
/// shared by `randomInt`/`randomFloat`. Comparison goes through the canonical
/// [`Value`] ordering so a mixed `Int64`/`Float64` pair compares correctly.
/// Both `evaluate` (on extracted typed bounds re-wrapped as [`Value`]) and the
/// const validator funnel through this single check.
fn reject_min_gt_max(function: &str, min: &Value, max: &Value) -> Result<(), FuncError> {
    if min > max {
        return Err(FuncError::EvalFailed {
            function: function.to_owned(),
            reason: format!("min ({min:?}) must not exceed max ({max:?})"),
        });
    }
    Ok(())
}

/// Validates a constant `min`/`max` pair for `randomInt`/`randomFloat`. Only
/// errors when both bounds are constant and `min > max`. Dynamic bounds are
/// skipped.
fn validate_min_max_const(function: &str, args: &[Option<&Value>]) -> Result<(), FuncError> {
    let (Some(Some(min)), Some(Some(max))) = (args.first(), args.get(1)) else {
        return Ok(());
    };
    reject_min_gt_max(function, min, max)
}

/// Rejects a length outside `0..=max`, mirroring the bound shared by every
/// length-prefixed random function. Both [`extract_length`] (runtime) and
/// [`validate_length_const`] (const validator) funnel through this single check.
fn reject_length_out_of_range(function: &str, n: i64, max: i64) -> Result<(), FuncError> {
    if n < 0 {
        return Err(FuncError::EvalFailed {
            function: function.to_owned(),
            reason: format!("length must be non-negative, got {n}"),
        });
    }
    if n > max {
        return Err(FuncError::EvalFailed {
            function: function.to_owned(),
            reason: format!("length {n} exceeds maximum {max}"),
        });
    }
    Ok(())
}

/// Validates a constant length argument (arg index 0) for the length-prefixed
/// random functions. Dynamic or non-`Int64` lengths are skipped; a constant
/// length below `0` or above `max` raises the same `EvalFailed` as
/// [`extract_length`].
fn validate_length_const(
    function: &str,
    args: &[Option<&Value>],
    max: i64,
) -> Result<(), FuncError> {
    if let Some(Some(Value::Int64(n))) = args.first() {
        reject_length_out_of_range(function, *n, max)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// randomUuid
// ---------------------------------------------------------------------------

struct RandomUuidFunc;

impl ExprFunction for RandomUuidFunc {
    fn name(&self) -> &str {
        "randomUuid"
    }

    fn min_args(&self) -> usize {
        0
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn purity(&self, const_args: &[bool]) -> bool {
        seeded_purity(const_args, 0)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Uuid))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let Some(mut rng) = take_seed("randomUuid", args, 0)?.rng() else {
            return Ok(Value::Null);
        };
        let mut bytes = [0u8; 16];
        rng.fill_bytes(&mut bytes);
        Ok(Value::Uuid(Builder::from_random_bytes(bytes).into_uuid()))
    }

    fn validate_const_args(
        &self,
        args: &[Option<&Value>],
        _context: &EvalContext,
    ) -> Result<(), FuncError> {
        validate_seed_const("randomUuid", args, 0)
    }
}

// ---------------------------------------------------------------------------
// randomInt
// ---------------------------------------------------------------------------

struct RandomIntFunc;

impl ExprFunction for RandomIntFunc {
    fn name(&self) -> &str {
        "randomInt"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(3)
    }

    fn purity(&self, const_args: &[bool]) -> bool {
        seeded_purity(const_args, 2)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Int64))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let seed = take_seed("randomInt", args, 2)?;
        let min_val = args.read(0);
        let max_val = args.read(1);

        let min = extract_i64("randomInt", "min", min_val)?;
        let max = extract_i64("randomInt", "max", max_val)?;

        reject_min_gt_max("randomInt", min_val, max_val)?;

        let Some(mut rng) = seed.rng() else {
            return Ok(Value::Null);
        };
        let val = rng.random_range(min..=max);
        Ok(Value::Int64(val))
    }

    fn validate_const_args(
        &self,
        args: &[Option<&Value>],
        _context: &EvalContext,
    ) -> Result<(), FuncError> {
        validate_min_max_const("randomInt", args)?;
        validate_seed_const("randomInt", args, 2)
    }
}

// ---------------------------------------------------------------------------
// randomFloat
// ---------------------------------------------------------------------------

struct RandomFloatFunc;

impl ExprFunction for RandomFloatFunc {
    fn name(&self) -> &str {
        "randomFloat"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(3)
    }

    fn purity(&self, const_args: &[bool]) -> bool {
        seeded_purity(const_args, 2)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Float64))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let seed = take_seed("randomFloat", args, 2)?;
        let min_val = args.read(0);
        let max_val = args.read(1);

        let min = extract_f64("randomFloat", "min", min_val)?;
        let max = extract_f64("randomFloat", "max", max_val)?;

        reject_min_gt_max("randomFloat", min_val, max_val)?;

        let Some(mut rng) = seed.rng() else {
            return Ok(Value::Null);
        };
        let val: f64 = rng.random_range(min..max);
        Ok(Value::Float64(val))
    }

    fn validate_const_args(
        &self,
        args: &[Option<&Value>],
        _context: &EvalContext,
    ) -> Result<(), FuncError> {
        validate_min_max_const("randomFloat", args)?;
        validate_seed_const("randomFloat", args, 2)
    }
}

// ---------------------------------------------------------------------------
// randomAlphanumeric
// ---------------------------------------------------------------------------

struct RandomAlphanumericFunc;

impl ExprFunction for RandomAlphanumericFunc {
    fn name(&self) -> &str {
        "randomAlphanumeric"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn purity(&self, const_args: &[bool]) -> bool {
        seeded_purity(const_args, 1)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Text { size: None }))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let seed = take_seed("randomAlphanumeric", args, 1)?;
        let length_val = args.read(0);
        let length = extract_length("randomAlphanumeric", length_val, MAX_STRING_LENGTH)?;

        let Some(mut rng) = seed.rng() else {
            return Ok(Value::Null);
        };
        let s = Alphanumeric.sample_string(&mut rng, length as usize);
        Ok(Value::Text(s))
    }

    fn validate_const_args(
        &self,
        args: &[Option<&Value>],
        _context: &EvalContext,
    ) -> Result<(), FuncError> {
        validate_length_const("randomAlphanumeric", args, MAX_STRING_LENGTH)?;
        validate_seed_const("randomAlphanumeric", args, 1)
    }
}

// ---------------------------------------------------------------------------
// randomHex
// ---------------------------------------------------------------------------

struct RandomHexFunc;

impl ExprFunction for RandomHexFunc {
    fn name(&self) -> &str {
        "randomHex"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn purity(&self, const_args: &[bool]) -> bool {
        seeded_purity(const_args, 1)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Text { size: None }))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let seed = take_seed("randomHex", args, 1)?;
        let length_val = args.read(0);
        let length = extract_length("randomHex", length_val, MAX_STRING_LENGTH)?;

        let Some(mut rng) = seed.rng() else {
            return Ok(Value::Null);
        };
        let s: String = (0..length)
            .map(|_| {
                let nibble: u8 = rng.random_range(0..16);
                char::from(if nibble < 10 {
                    b'0' + nibble
                } else {
                    b'a' + nibble - 10
                })
            })
            .collect();
        Ok(Value::Text(s))
    }

    fn validate_const_args(
        &self,
        args: &[Option<&Value>],
        _context: &EvalContext,
    ) -> Result<(), FuncError> {
        validate_length_const("randomHex", args, MAX_STRING_LENGTH)?;
        validate_seed_const("randomHex", args, 1)
    }
}

// ---------------------------------------------------------------------------
// randomBytes
// ---------------------------------------------------------------------------

struct RandomBytesFunc;

impl ExprFunction for RandomBytesFunc {
    fn name(&self) -> &str {
        "randomBytes"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn purity(&self, const_args: &[bool]) -> bool {
        seeded_purity(const_args, 1)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Bytes { size: None }))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let seed = take_seed("randomBytes", args, 1)?;
        let length_val = args.read(0);
        let length = extract_length("randomBytes", length_val, MAX_BYTES_LENGTH)?;

        let Some(mut rng) = seed.rng() else {
            return Ok(Value::Null);
        };
        let mut bytes = vec![0u8; length as usize];
        rng.fill(&mut bytes[..]);
        Ok(Value::Bytes(bytes))
    }

    fn validate_const_args(
        &self,
        args: &[Option<&Value>],
        _context: &EvalContext,
    ) -> Result<(), FuncError> {
        validate_length_const("randomBytes", args, MAX_BYTES_LENGTH)?;
        validate_seed_const("randomBytes", args, 1)
    }
}

// ---------------------------------------------------------------------------
// randomChoice
// ---------------------------------------------------------------------------

struct RandomChoiceFunc;

impl ExprFunction for RandomChoiceFunc {
    fn name(&self) -> &str {
        "randomChoice"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        None
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        if args.is_empty() {
            return Err(FuncError::ArityMismatch {
                function: "randomChoice".to_owned(),
                expected: "at least 1".to_owned(),
                actual: 0,
            });
        }
        // The return type is nullable if any argument is nullable; the data type
        // is taken from the first argument (all args should be the same type in
        // practice, but we don't enforce that at type-check time).
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(args[0].data_type.clone(), nullable))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        if args.is_empty() {
            return Err(FuncError::ArityMismatch {
                function: "randomChoice".to_owned(),
                expected: "at least 1".to_owned(),
                actual: 0,
            });
        }
        let mut rng = rand::rng();
        let idx = rng.random_range(0..args.len());
        Ok(args.take(idx))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_i64(function: &str, param_name: &str, value: &Value) -> Result<i64, FuncError> {
    match value {
        Value::Int64(n) => Ok(*n),
        other => Err(FuncError::TypeMismatch {
            function: function.to_owned(),
            expected: format!("Int64 for {param_name}"),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

fn extract_f64(function: &str, param_name: &str, value: &Value) -> Result<f64, FuncError> {
    match value {
        Value::Float64(n) => Ok(*n),
        Value::Int64(n) => Ok(*n as f64),
        other => Err(FuncError::TypeMismatch {
            function: function.to_owned(),
            expected: format!("Float64 or Int64 for {param_name}"),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

fn extract_length(function: &str, value: &Value, max: i64) -> Result<i64, FuncError> {
    let n = match value {
        Value::Int64(n) => *n,
        other => {
            return Err(FuncError::TypeMismatch {
                function: function.to_owned(),
                expected: "Int64 for length".to_owned(),
                actual: format!("{:?}", other.data_type()),
            });
        }
    };
    reject_length_out_of_range(function, n, max)?;
    Ok(n)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_support::{ctx, eval};

    #[test]
    fn random_uuid_produces_valid_v4() {
        let f = RandomUuidFunc;
        let result = eval(&f, smallvec::smallvec![], &ctx()).unwrap();
        match result {
            Value::Uuid(id) => {
                assert_eq!(id.get_version_num(), 4);
            }
            other => panic!("expected Uuid, got {other:?}"),
        }
    }

    #[test]
    fn random_uuid_produces_different_values() {
        let f = RandomUuidFunc;
        let r1 = eval(&f, smallvec::smallvec![], &ctx()).unwrap();
        let r2 = eval(&f, smallvec::smallvec![], &ctx()).unwrap();
        assert_ne!(r1, r2);
    }

    #[test]
    fn random_int_in_range() {
        let f = RandomIntFunc;
        for _ in 0..100 {
            let result = eval(
                &f,
                smallvec::smallvec![Value::Int64(10), Value::Int64(20)],
                &ctx(),
            )
            .unwrap();
            match result {
                Value::Int64(n) => {
                    assert!((10..=20).contains(&n), "got {n} outside [10, 20]");
                }
                other => panic!("expected Int64, got {other:?}"),
            }
        }
    }

    #[test]
    fn random_int_equal_bounds() {
        let f = RandomIntFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Int64(5), Value::Int64(5)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Int64(5));
    }

    #[test]
    fn random_int_min_exceeds_max() {
        let f = RandomIntFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Int64(20), Value::Int64(10)],
            &ctx(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn random_float_in_range() {
        let f = RandomFloatFunc;
        for _ in 0..100 {
            let result = eval(
                &f,
                smallvec::smallvec![Value::Float64(1.0), Value::Float64(2.0)],
                &ctx(),
            )
            .unwrap();
            match result {
                Value::Float64(n) => {
                    assert!((1.0..2.0).contains(&n), "got {n} outside [1.0, 2.0)");
                }
                other => panic!("expected Float64, got {other:?}"),
            }
        }
    }

    #[test]
    fn random_alphanumeric_correct_length() {
        let f = RandomAlphanumericFunc;
        let result = eval(&f, smallvec::smallvec![Value::Int64(32)], &ctx()).unwrap();
        match result {
            Value::Text(s) => {
                assert_eq!(s.len(), 32);
                assert!(s.chars().all(|c| c.is_ascii_alphanumeric()));
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn random_alphanumeric_zero_length() {
        let f = RandomAlphanumericFunc;
        let result = eval(&f, smallvec::smallvec![Value::Int64(0)], &ctx()).unwrap();
        assert_eq!(result, Value::Text(String::new()));
    }

    #[test]
    fn random_alphanumeric_exceeds_max() {
        let f = RandomAlphanumericFunc;
        let result = eval(&f, smallvec::smallvec![Value::Int64(2000)], &ctx());
        assert!(result.is_err());
    }

    #[test]
    fn random_hex_correct_length_and_chars() {
        let f = RandomHexFunc;
        let result = eval(&f, smallvec::smallvec![Value::Int64(16)], &ctx()).unwrap();
        match result {
            Value::Text(s) => {
                assert_eq!(s.len(), 16);
                assert!(
                    s.chars()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
                );
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn random_bytes_correct_length() {
        let f = RandomBytesFunc;
        let result = eval(&f, smallvec::smallvec![Value::Int64(64)], &ctx()).unwrap();
        match result {
            Value::Bytes(b) => assert_eq!(b.len(), 64),
            other => panic!("expected Bytes, got {other:?}"),
        }
    }

    #[test]
    fn random_bytes_exceeds_max() {
        let f = RandomBytesFunc;
        let result = eval(&f, smallvec::smallvec![Value::Int64(2_000_000)], &ctx());
        assert!(result.is_err());
    }

    #[test]
    fn random_choice_returns_one_of_args() {
        let f = RandomChoiceFunc;
        let options = vec![
            Value::Text("a".into()),
            Value::Text("b".into()),
            Value::Text("c".into()),
        ];
        for _ in 0..50 {
            let result = eval(&f, options.clone(), &ctx()).unwrap();
            match &result {
                Value::Text(s) => {
                    assert!(s == "a" || s == "b" || s == "c", "unexpected value: {s}");
                }
                other => panic!("expected Text, got {other:?}"),
            }
        }
    }

    #[test]
    fn random_choice_single_arg() {
        let f = RandomChoiceFunc;
        let result = eval(&f, smallvec::smallvec![Value::Int64(42)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(42));
    }

    #[test]
    fn random_int_seeded_is_deterministic() {
        let f = RandomIntFunc;
        let args = || vec![Value::Int64(0), Value::Int64(1_000_000), Value::Int64(42)];
        let a = eval(&f, args(), &ctx()).unwrap();
        let b = eval(&f, args(), &ctx()).unwrap();
        assert_eq!(a, b, "same seed must yield the same value");
        match a {
            Value::Int64(n) => assert!((0..=1_000_000).contains(&n)),
            other => panic!("expected Int64, got {other:?}"),
        }
    }

    #[test]
    fn random_int_different_seed_differs() {
        let f = RandomIntFunc;
        let a = eval(
            &f,
            smallvec::smallvec![Value::Int64(0), Value::Int64(i64::MAX), Value::Int64(1)],
            &ctx(),
        )
        .unwrap();
        let b = eval(
            &f,
            smallvec::smallvec![Value::Int64(0), Value::Int64(i64::MAX), Value::Int64(2)],
            &ctx(),
        )
        .unwrap();
        assert_ne!(
            a, b,
            "distinct seeds should produce distinct values over a wide range"
        );
    }

    #[test]
    fn random_uuid_seeded_is_deterministic() {
        let f = RandomUuidFunc;
        let a = eval(&f, smallvec::smallvec![Value::Int64(7)], &ctx()).unwrap();
        let b = eval(&f, smallvec::smallvec![Value::Int64(7)], &ctx()).unwrap();
        assert_eq!(a, b);
        match a {
            Value::Uuid(id) => assert_eq!(id.get_version_num(), 4),
            other => panic!("expected Uuid, got {other:?}"),
        }
    }

    #[test]
    fn random_null_seed_returns_null() {
        let int_f = RandomIntFunc;
        let r = eval(
            &int_f,
            smallvec::smallvec![Value::Int64(0), Value::Int64(10), Value::Null],
            &ctx(),
        )
        .unwrap();
        assert_eq!(r, Value::Null);

        let uuid_f = RandomUuidFunc;
        assert_eq!(
            eval(&uuid_f, smallvec::smallvec![Value::Null], &ctx()).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn random_non_int_seed_errors() {
        let f = RandomIntFunc;
        let r = eval(
            &f,
            smallvec::smallvec![Value::Int64(0), Value::Int64(10), Value::Text("x".into())],
            &ctx(),
        );
        assert!(matches!(r, Err(FuncError::TypeMismatch { .. })));
    }

    #[test]
    fn validate_const_seed_int_ok() {
        let seed = Value::Int64(42);
        let result = RandomUuidFunc.validate_const_args(&[Some(&seed)], &ctx());
        assert!(result.is_ok());
    }

    #[test]
    fn validate_const_seed_absent_ok() {
        // No seed slot present → skip.
        let result = RandomUuidFunc.validate_const_args(&[], &ctx());
        assert!(result.is_ok());
    }

    #[test]
    fn validate_const_seed_dynamic_ok() {
        // Seed slot present but dynamic (None) → skip.
        let result = RandomUuidFunc.validate_const_args(&[None], &ctx());
        assert!(result.is_ok());
    }

    #[test]
    fn validate_const_seed_wrong_type_errors() {
        let seed = Value::Text("x".into());
        let min = Value::Int64(0);
        let max = Value::Int64(10);
        let result =
            RandomIntFunc.validate_const_args(&[Some(&min), Some(&max), Some(&seed)], &ctx());
        assert!(matches!(result, Err(FuncError::TypeMismatch { .. })));
    }

    #[test]
    fn validate_const_min_max_ok() {
        let min = Value::Int64(0);
        let max = Value::Int64(10);
        let result = RandomIntFunc.validate_const_args(&[Some(&min), Some(&max)], &ctx());
        assert!(result.is_ok());
    }

    #[test]
    fn validate_const_min_equals_max_ok() {
        // min == max is the inclusive boundary — only min > max is an error.
        let bound = Value::Int64(5);
        let int_result = RandomIntFunc.validate_const_args(&[Some(&bound), Some(&bound)], &ctx());
        assert!(int_result.is_ok());

        let float_bound = Value::Float64(5.0);
        let float_result =
            RandomFloatFunc.validate_const_args(&[Some(&float_bound), Some(&float_bound)], &ctx());
        assert!(float_result.is_ok());
    }

    #[test]
    fn validate_const_min_exceeds_max_errors() {
        let min = Value::Int64(20);
        let max = Value::Int64(10);
        let result = RandomIntFunc.validate_const_args(&[Some(&min), Some(&max)], &ctx());
        assert!(matches!(result, Err(FuncError::EvalFailed { .. })));
    }

    #[test]
    fn validate_const_length_ok() {
        let length = Value::Int64(32);
        let result = RandomAlphanumericFunc.validate_const_args(&[Some(&length)], &ctx());
        assert!(result.is_ok());
    }

    #[test]
    fn validate_const_length_negative_errors() {
        let length = Value::Int64(-1);
        let result = RandomHexFunc.validate_const_args(&[Some(&length)], &ctx());
        assert!(matches!(result, Err(FuncError::EvalFailed { .. })));
    }

    #[test]
    fn validate_const_length_exceeds_max_errors() {
        let length = Value::Int64(2_000_000);
        let result = RandomBytesFunc.validate_const_args(&[Some(&length)], &ctx());
        assert!(matches!(result, Err(FuncError::EvalFailed { .. })));
    }

    #[test]
    fn random_purity_requires_constant_seed() {
        // No seed arg → impure (cannot be const-folded).
        assert!(!RandomIntFunc.purity(&[true, true]));
        // Seed present and constant → pure.
        assert!(RandomIntFunc.purity(&[true, true, true]));
        // Seed present but non-constant → impure.
        assert!(!RandomIntFunc.purity(&[true, true, false]));
        // randomUuid: base arity 0.
        assert!(!RandomUuidFunc.purity(&[]));
        assert!(RandomUuidFunc.purity(&[true]));
        // Bare is_pure (no arg info) stays false for random.
        assert!(!RandomIntFunc.is_pure());
    }
}
