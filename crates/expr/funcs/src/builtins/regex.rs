use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};
use regex::Regex;

use crate::builtins::arg_extract::extract_text;
use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{EvalContext, ExprFunction};

static REGEX_MATCH: RegexMatchFunc = RegexMatchFunc;
static REGEX_REPLACE: RegexReplaceFunc = RegexReplaceFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&REGEX_MATCH);
    registry.register(&REGEX_REPLACE);
}

/// Compiles a regex pattern, mapping compilation errors onto
/// [`FuncError::RegexCompileFailed`]. The pattern is compiled inline on every
/// call; a per-flow cache is wired in a later phase.
fn compile_pattern(pattern: &str) -> Result<Regex, FuncError> {
    Regex::new(pattern).map_err(|err| FuncError::RegexCompileFailed {
        reason: format!("{pattern:?}: {err}"),
    })
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let pattern = args.remove(1);
        let text = args.remove(0);
        if text.is_null() || pattern.is_null() {
            return Ok(Value::Null);
        }
        let text = extract_text(text, "regexMatch")?;
        let pattern = extract_text(pattern, "regexMatch")?;
        let re = compile_pattern(&pattern)?;
        Ok(Value::Bool(re.is_match(&text)))
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let replacement = args.remove(2);
        let pattern = args.remove(1);
        let text = args.remove(0);
        if text.is_null() || pattern.is_null() || replacement.is_null() {
            return Ok(Value::Null);
        }
        let text = extract_text(text, "regexReplace")?;
        let pattern = extract_text(pattern, "regexReplace")?;
        let replacement = extract_text(replacement, "regexReplace")?;
        let re = compile_pattern(&pattern)?;
        let replaced = re.replace_all(&text, replacement.as_str()).into_owned();
        Ok(Value::Text(replaced))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_support::ctx;

    #[test]
    fn regex_match_true() {
        let result = REGEX_MATCH
            .evaluate(
                vec![Value::Text("hello123".into()), Value::Text(r"\d+".into())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn regex_match_false() {
        let result = REGEX_MATCH
            .evaluate(
                vec![Value::Text("hello".into()), Value::Text(r"\d+".into())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn regex_replace_all() {
        let result = REGEX_REPLACE
            .evaluate(
                vec![
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
        let result = REGEX_MATCH.evaluate(
            vec![Value::Text("hello".into()), Value::Text("(".into())],
            &ctx(),
        );
        assert!(matches!(result, Err(FuncError::RegexCompileFailed { .. })));
    }

    #[test]
    fn regex_replace_invalid_pattern() {
        let result = REGEX_REPLACE.evaluate(
            vec![
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
        let from_text = REGEX_MATCH
            .evaluate(vec![Value::Null, Value::Text(r"\d".into())], &ctx())
            .unwrap();
        assert_eq!(from_text, Value::Null);

        let from_pattern = REGEX_MATCH
            .evaluate(vec![Value::Text("abc".into()), Value::Null], &ctx())
            .unwrap();
        assert_eq!(from_pattern, Value::Null);
    }

    #[test]
    fn regex_replace_null_propagation() {
        let result = REGEX_REPLACE
            .evaluate(
                vec![
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
        let result =
            REGEX_MATCH.evaluate(vec![Value::Int64(42), Value::Text(r"\d".into())], &ctx());
        assert!(matches!(result, Err(FuncError::TypeMismatch { .. })));
    }

    #[test]
    fn regex_replace_non_text_arg() {
        let result = REGEX_REPLACE.evaluate(
            vec![
                Value::Text("abc".into()),
                Value::Text(r"\d".into()),
                Value::Int64(1),
            ],
            &ctx(),
        );
        assert!(matches!(result, Err(FuncError::TypeMismatch { .. })));
    }
}
