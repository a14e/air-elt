//! [`RuntimeProgram`] — the per-row executable handle over an optimizer
//! [`CompactProgram`], plus [`RowFields`], the fixed-schema field binding.
//!
//! The optimizer produces a field-free [`CompactProgram`]; the per-row hot path
//! lives here. [`RuntimeProgram`] owns that program and runs it against a row
//! bound through a [`RowFields`] source, reusing the optimizer's arena evaluator
//! ([`ProgramEvaluator`]) so one evaluator serves both the field-free correctness
//! oracle and the per-row runtime.

use air_elt_expr_funcs::FunctionRegistry;
use air_elt_expr_funcs::signature::EvalContext;
use air_elt_expr_optimize::{CompactProgram, EvalError, FieldSource, OptNode, ProgramEvaluator};
use air_elt_expr_parse::FieldsSelector;
use air_elt_types::{Schema, Value};

/// A compiled expression ready for per-row execution. Owns the optimizer's
/// [`CompactProgram`] (the field-free arena IR) and binds source columns per row
/// at evaluation time.
pub struct RuntimeProgram {
    program: CompactProgram,
}

impl RuntimeProgram {
    /// Wrap an optimized [`CompactProgram`] for per-row execution.
    pub fn create(program: CompactProgram) -> Self {
        Self { program }
    }

    /// Evaluate a field-free program (no `field(...)` access) through the arena
    /// evaluator. The single production path for const / default / switch
    /// literals and config patching — a comptime program with no source binding.
    /// A surviving source-field access is a [`EvalError::FieldNotBound`].
    pub fn evaluate(
        &self,
        registry: &FunctionRegistry,
        context: &EvalContext,
    ) -> Result<Value, EvalError> {
        let mut evaluator = ProgramEvaluator::create(&self.program, registry, context);
        evaluator.evaluate()
    }

    /// Evaluate the program against `row`, resolving `field(...)` / `fields(...)`
    /// nodes positionally through `schema`. `context` carries the per-batch `now`
    /// and the shared compiled-artifact caches; `registry` resolves calls.
    pub fn evaluate_row(
        &self,
        registry: &FunctionRegistry,
        context: &EvalContext,
        schema: &Schema,
        row: &[Value],
    ) -> Result<Value, EvalError> {
        let fields = RowFields { schema, row };
        let mut evaluator =
            ProgramEvaluator::create_with_fields(&self.program, registry, context, &fields);
        evaluator.evaluate()
    }

    /// The result node, once the program has no statements (no variable
    /// bindings). After optimization a const / identity column collapses to a
    /// single result node with an empty statement list, which is what the
    /// Transform compiler inspects to inline it.
    fn bare_result(&self) -> Option<&OptNode> {
        if self.program.statements().is_empty() {
            Some(self.program.node(self.program.result()))
        } else {
            None
        }
    }

    /// `true` when the program folded to a single constant — its value is
    /// row-independent and can be inlined as a literal column.
    pub fn is_value(&self) -> bool {
        matches!(self.bare_result(), Some(OptNode::Const(_)))
    }

    /// The folded constant value, if the program [`is_value`](Self::is_value).
    /// The Transform compiler lowers such a compute column to the literal.
    pub fn as_value(&self) -> Option<&Value> {
        match self.bare_result() {
            Some(OptNode::Const(id)) => Some(self.program.constant(*id)),
            _ => None,
        }
    }

    /// `true` when the program is a bare source-column passthrough
    /// (`field("c")`) — the Transform compiler lowers it to a `Take`.
    pub fn is_identity(&self) -> bool {
        matches!(self.bare_result(), Some(OptNode::SourceField(_)))
    }

    /// The passed-through column name, if the program
    /// [`is_identity`](Self::is_identity).
    pub fn identity_column(&self) -> Option<&str> {
        match self.bare_result() {
            Some(OptNode::SourceField(name)) => Some(name),
            _ => None,
        }
    }
}

/// Binds source-column access to a positional row under a fixed [`Schema`]:
/// `field("c")` resolves the column name to its schema index, then reads
/// `row[index]`. A name absent from the schema (or an index past the row) yields
/// [`Value::Null`] — "no data" for that column, the ELT-natural reading of a
/// missing field — rather than an error; a fixed schema rejects unknown column
/// names at compile time, so this only guards a schema/row length mismatch.
pub struct RowFields<'a> {
    pub schema: &'a Schema,
    pub row: &'a [Value],
}

impl RowFields<'_> {
    /// The value of column `name`, or [`Value::Null`] when the column is absent.
    fn column(&self, name: &str) -> Value {
        match self.schema.index_of(name) {
            Some(index) => self.row.get(index).cloned().unwrap_or(Value::Null),
            None => Value::Null,
        }
    }
}

impl FieldSource for RowFields<'_> {
    fn field(&self, name: &str) -> Result<Value, EvalError> {
        Ok(self.column(name))
    }

    fn fields(&self, selector: &FieldsSelector) -> Result<Value, EvalError> {
        let fields = match selector {
            FieldsSelector::All => self
                .schema
                .fields()
                .iter()
                .enumerate()
                .map(|(index, field)| {
                    let value = self.row.get(index).cloned().unwrap_or(Value::Null);
                    (field.name.clone(), value)
                })
                .collect(),
            FieldsSelector::Named(names) => names
                .iter()
                .map(|name| (name.clone(), self.column(name)))
                .collect(),
        };
        Ok(Value::Object(fields))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use air_elt_expr_funcs::signature::{EnvResolver, EvalContext, FileResolver};
    use air_elt_expr_funcs::{FuncError, FunctionRegistry};
    use air_elt_expr_optimize::Optimizer;
    use air_elt_expr_parse::Parser;
    use air_elt_types::{DataType, Field, Schema, Value};

    use super::*;

    struct NoEnv;

    impl EnvResolver for NoEnv {
        fn get(&self, _key: &str) -> Option<String> {
            None
        }
    }

    struct NoFiles;

    impl FileResolver for NoFiles {
        fn read(&self, path: &str, _base_dir: &std::path::Path) -> Result<String, FuncError> {
            Err(FuncError::FileReadFailed {
                path: path.to_owned(),
                reason: "not implemented".to_owned(),
            })
        }
    }

    fn test_context() -> EvalContext {
        EvalContext {
            env_resolver: Arc::new(NoEnv),
            file_resolver: Arc::new(NoFiles),
            now: chrono::Utc::now(),
            base_dir: PathBuf::from("/tmp"),
            is_compile_time: false,
            caches: air_elt_expr_funcs::ExprCaches::default(),
        }
    }

    fn schema() -> Schema {
        Schema::new(vec![
            Field {
                name: "age".to_owned(),
                data_type: DataType::Int64,
                nullable: false,
            },
            Field {
                name: "name".to_owned(),
                data_type: DataType::Text { size: None },
                nullable: false,
            },
        ])
    }

    /// Compile a runtime expression (field grammar allowed) into a
    /// [`RuntimeProgram`].
    fn compile(source: &str, registry: &FunctionRegistry, context: &EvalContext) -> RuntimeProgram {
        let program = Parser::create().parse_expression(source).unwrap();
        let compact = Optimizer::create(registry, context)
            .compile(&program, None, None)
            .unwrap();
        RuntimeProgram::create(compact)
    }

    fn eval_row(source: &str, row: &[Value]) -> Value {
        let registry = FunctionRegistry::with_builtins();
        let context = test_context();
        let program = compile(source, &registry, &context);
        program
            .evaluate_row(&registry, &context, &schema(), row)
            .unwrap()
    }

    #[test]
    fn field_reads_row_column() {
        let row = vec![Value::Int64(42), Value::Text("ann".to_owned())];
        assert_eq!(eval_row("`age`", &row), Value::Int64(42));
        assert_eq!(eval_row("`name`", &row), Value::Text("ann".to_owned()));
    }

    #[test]
    fn field_arithmetic() {
        let row = vec![Value::Int64(42), Value::Text("ann".to_owned())];
        assert_eq!(eval_row("`age` + 1", &row), Value::Int64(43));
    }

    // --- hoisted-register aliasing across a call's args (regression) ----------
    // `field_hoist` collapses a field read used >=2 times into one register.
    // Under the lazy `ArgWindow`, a register arg is read/moved when the function
    // takes it, so a `RegisterTake` (move) must never strand another read of the
    // same register in the same call.

    #[test]
    fn hoisted_field_aliased_in_nested_arg() {
        // add(Register r, negate(Register r)): the nested negate moves the
        // register during the push loop, so the direct `Register` arg must still
        // see the value. `age + (-age)` is 0 for any age, never Null.
        let row = vec![Value::Int64(7), Value::Text("ann".to_owned())];
        assert_eq!(eval_row("`age` + (-`age`)", &row), Value::Int64(0));
    }

    #[test]
    fn hoisted_field_aliased_in_descending_take_call() {
        // replace() takes its args in descending index order (take(2),take(1),
        // take(0)); with all three the same hoisted field, the move slot is taken
        // before the others are read. replace(name, name, name) == name.
        let row = vec![Value::Int64(7), Value::Text("ann".to_owned())];
        assert_eq!(
            eval_row("replace(`name`, `name`, `name`)", &row),
            Value::Text("ann".to_owned())
        );
    }

    #[test]
    fn missing_column_is_null() {
        // `absent` is not in the schema → "no data" → Null, so `ifNull` falls
        // through to the alternative.
        let row = vec![Value::Int64(42), Value::Text("ann".to_owned())];
        assert_eq!(
            eval_row("ifNull(field('absent'), -1)", &row),
            Value::Int64(-1)
        );
    }

    #[test]
    fn short_row_is_null() {
        // Schema declares `name` at index 1, but the row has only one cell.
        let row = vec![Value::Int64(42)];
        assert_eq!(
            eval_row("ifNull(`name`, 'none')", &row),
            Value::Text("none".to_owned())
        );
    }

    #[test]
    fn fields_all_builds_object() {
        // `fields("*")` builds a `Value::Object`, preserving each column's
        // canonical value type (Int64 stays Int64, not a JSON number).
        let row = vec![Value::Int64(42), Value::Text("ann".to_owned())];
        let result = eval_row("fields(\"*\")", &row);
        assert_eq!(
            result,
            Value::Object(vec![
                ("age".to_owned(), Value::Int64(42)),
                ("name".to_owned(), Value::Text("ann".to_owned())),
            ])
        );
    }

    #[test]
    fn fields_named_subset() {
        let row = vec![Value::Int64(42), Value::Text("ann".to_owned())];
        let result = eval_row("fields(\"name\")", &row);
        assert_eq!(
            result,
            Value::Object(vec![("name".to_owned(), Value::Text("ann".to_owned()))])
        );
    }

    #[test]
    fn object_literal_with_field_evaluates_per_row() {
        // A non-constant object literal stays a live `Object` node (it does not
        // const-fold), so the arena evaluator builds it per row against the
        // binding — preserving the column's canonical type — and the result is a
        // `Value::Object` consumable by the object builtins.
        let row = vec![Value::Int64(42), Value::Text("ann".to_owned())];
        assert_eq!(
            eval_row("{\"v\" = `age`}", &row),
            Value::Object(vec![("v".to_owned(), Value::Int64(42))])
        );
        assert_eq!(
            eval_row("objectGet({\"v\" = `age`}, \"v\")", &row),
            Value::Int64(42)
        );
    }

    #[test]
    fn field_free_program_ignores_binding() {
        // A program with no field access evaluates the same with any row.
        assert_eq!(eval_row("1 + 2", &[]), Value::Int64(3));
    }

    // --- Transform extraction API: is_value / as_value / is_identity / ... -----

    #[test]
    fn const_program_is_value() {
        let registry = FunctionRegistry::with_builtins();
        let context = test_context();
        // A pure expression folds to a single `Const`.
        let program = compile("1 + 2", &registry, &context);
        assert!(program.is_value());
        assert_eq!(program.as_value(), Some(&Value::Int64(3)));
        // ... and is not an identity passthrough.
        assert!(!program.is_identity());
        assert_eq!(program.identity_column(), None);
    }

    #[test]
    fn bare_field_program_is_identity() {
        let registry = FunctionRegistry::with_builtins();
        let context = test_context();
        // A bare column reference is an identity passthrough.
        let program = compile("`age`", &registry, &context);
        assert!(program.is_identity());
        assert_eq!(program.identity_column(), Some("age"));
        // ... and is not a constant value.
        assert!(!program.is_value());
        assert_eq!(program.as_value(), None);
    }

    #[test]
    fn computed_field_program_is_neither() {
        let registry = FunctionRegistry::with_builtins();
        let context = test_context();
        // `field + 1` stays a call over a source field — neither value nor identity.
        let program = compile("`age` + 1", &registry, &context);
        assert!(!program.is_value());
        assert!(!program.is_identity());
        assert_eq!(program.as_value(), None);
        assert_eq!(program.identity_column(), None);
    }
}
