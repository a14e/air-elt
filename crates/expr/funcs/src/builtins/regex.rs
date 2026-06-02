use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::builtins::arg_extract::extract_text_ref;
use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{ArgWindow, EvalContext, ExprFunction};

static REGEX_MATCH: RegexMatchFunc = RegexMatchFunc;
static REGEX_REPLACE: RegexReplaceFunc = RegexReplaceFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&REGEX_MATCH);
    registry.register(&REGEX_REPLACE);
}

/// Warms / validates a constant pattern argument through the cache. Surfaces a
/// [`FuncError::RegexCompileFailed`] for an inlined invalid pattern and leaves
/// the compiled regex resident for evaluation. Dynamic or non-`Text` patterns
/// are skipped.
fn validate_pattern_const(args: &[Option<&Value>], context: &EvalContext) -> Result<(), FuncError> {
    if let Some(Some(Value::Text(pattern))) = args.get(1) {
        context.caches.with_regex_cached(pattern, |_| ())?;
    }
    Ok(())
}

struct RegexMatchFunc;

impl ExprFunction for RegexMatchFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "regexMatch"
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
        context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let pattern = args.read(1);
        let text = args.read(0);
        if text.is_null() || pattern.is_null() {
            return Ok(Value::Null);
        }
        let text = extract_text_ref(text, "regexMatch")?;
        let pattern = extract_text_ref(pattern, "regexMatch")?;
        let matched = context
            .caches
            .with_regex_cached(pattern, |re| re.is_match(text))?;
        Ok(Value::Bool(matched))
    }

    fn validate_const_args(
        &self,
        args: &[Option<&Value>],
        context: &EvalContext,
    ) -> Result<(), FuncError> {
        validate_pattern_const(args, context)
    }
}

struct RegexReplaceFunc;

impl ExprFunction for RegexReplaceFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "regexReplace"
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
        context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let replacement = args.read(2);
        let pattern = args.read(1);
        let text = args.read(0);
        if text.is_null() || pattern.is_null() || replacement.is_null() {
            return Ok(Value::Null);
        }
        let text = extract_text_ref(text, "regexReplace")?;
        let pattern = extract_text_ref(pattern, "regexReplace")?;
        let replacement = extract_text_ref(replacement, "regexReplace")?;
        let replaced = context
            .caches
            .with_regex_cached(pattern, |re| re.replace_all(text, replacement).into_owned())?;
        Ok(Value::Text(replaced))
    }

    fn validate_const_args(
        &self,
        args: &[Option<&Value>],
        context: &EvalContext,
    ) -> Result<(), FuncError> {
        validate_pattern_const(args, context)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_support::{ctx, eval};

    #[test]
    fn regex_match_true() {
        let result = eval(
            &REGEX_MATCH,
            smallvec::smallvec![Value::Text("hello123".into()), Value::Text(r"\d+".into())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn regex_match_false() {
        let result = eval(
            &REGEX_MATCH,
            smallvec::smallvec![Value::Text("hello".into()), Value::Text(r"\d+".into())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn regex_replace_all() {
        let result = eval(
            &REGEX_REPLACE,
            smallvec::smallvec![
                Value::Text("a1b2c3".into()),
                Value::Text(r"\d".into()),
                Value::Text("X".into()),
            ],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Text("aXbXcX".into()));
    }

    #[test]
    fn regex_match_invalid_pattern() {
        let result = eval(
            &REGEX_MATCH,
            smallvec::smallvec![Value::Text("hello".into()), Value::Text("(".into())],
            &ctx(),
        );
        assert!(matches!(result, Err(FuncError::RegexCompileFailed { .. })));
    }

    #[test]
    fn regex_replace_invalid_pattern() {
        let result = eval(
            &REGEX_REPLACE,
            smallvec::smallvec![
                Value::Text("hello".into()),
                Value::Text("(".into()),
                Value::Text("x".into()),
            ],
            &ctx(),
        );
        assert!(matches!(result, Err(FuncError::RegexCompileFailed { .. })));
    }

    #[test]
    fn regex_match_null_propagation() {
        let from_text = eval(
            &REGEX_MATCH,
            smallvec::smallvec![Value::Null, Value::Text(r"\d".into())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(from_text, Value::Null);

        let from_pattern = eval(
            &REGEX_MATCH,
            smallvec::smallvec![Value::Text("abc".into()), Value::Null],
            &ctx(),
        )
        .unwrap();
        assert_eq!(from_pattern, Value::Null);
    }

    #[test]
    fn regex_replace_null_propagation() {
        let result = eval(
            &REGEX_REPLACE,
            smallvec::smallvec![
                Value::Text("abc".into()),
                Value::Text(r"\d".into()),
                Value::Null,
            ],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn regex_match_non_text_arg() {
        let result = eval(
            &REGEX_MATCH,
            smallvec::smallvec![Value::Int64(42), Value::Text(r"\d".into())],
            &ctx(),
        );
        assert!(matches!(result, Err(FuncError::TypeMismatch { .. })));
    }

    #[test]
    fn regex_replace_non_text_arg() {
        let result = eval(
            &REGEX_REPLACE,
            smallvec::smallvec![
                Value::Text("abc".into()),
                Value::Text(r"\d".into()),
                Value::Int64(1),
            ],
            &ctx(),
        );
        assert!(matches!(result, Err(FuncError::TypeMismatch { .. })));
    }

    #[test]
    fn validate_const_valid_pattern_ok() {
        let pattern = Value::Text(r"\d+".into());
        let result = REGEX_MATCH.validate_const_args(&[None, Some(&pattern)], &ctx());
        assert!(result.is_ok());
    }

    #[test]
    fn validate_const_dynamic_pattern_ok() {
        // Dynamic pattern (None) must skip the check.
        let result = REGEX_MATCH.validate_const_args(&[None, None], &ctx());
        assert!(result.is_ok());
    }

    #[test]
    fn validate_const_invalid_pattern_errors() {
        let pattern = Value::Text("(".into());
        let result = REGEX_REPLACE.validate_const_args(&[None, Some(&pattern), None], &ctx());
        assert!(matches!(result, Err(FuncError::RegexCompileFailed { .. })));
    }
}
