//! [`ProgramEvaluator`] — an evaluator over the compacted program.
//!
//! It executes statements into a register file, then evaluates the result,
//! recursing through the node arena. Its semantics mirror the canonical heap
//! evaluator in `air-elt-expr-runtime` exactly — this is what proves compaction
//! (and the optimizations feeding it) meaning-preserving.
//!
//! Source-field access is **not** bound here: `field`/`fields` need a per-row
//! binding that the transform layer supplies (Phase 3/4). Reaching one is a
//! [`EvalError::FieldNotBound`]. This evaluator therefore covers exactly the
//! field-free subset — constants, operators, conditionals, variables — which is
//! what the optimizer's correctness tests exercise.

use air_elt_expr_funcs::signature::EvalContext;
use air_elt_expr_funcs::{FuncError, FunctionRegistry};
use air_elt_expr_types::limits::{MAX_EXPR_DEPTH, MAX_EXPR_STRING_BYTES};
use air_elt_types::value_to_string;
use air_elt_types::{Key, Value};
use thiserror::Error;

use crate::model::program::{
    ArgSlice, CompactProgram, CompactYield, KeySlice, NodeRef, OptNode, SwitchTableId, TypeClass,
};

/// Errors raised while evaluating a [`CompactProgram`].
#[derive(Debug, Error)]
pub enum EvalError {
    /// A `field`/`fields`/source-column node was reached without a row binding.
    #[error("field()/fields() reached without a row binding")]
    FieldNotBound,

    /// Evaluation recursed past the maximum expression depth.
    #[error("expression nesting too deep (max {max})")]
    NestingTooDeep { max: usize },

    /// A boolean position (condition / `&&` / `||`) saw a non-boolean value.
    #[error("type error in {context}: expected Bool, got {actual}")]
    ExpectedBool {
        context: &'static str,
        actual: String,
    },

    /// An interpolation produced a string past the size cap.
    #[error("string too large: {len} bytes (max {max})")]
    StringTooLarge { len: usize, max: usize },

    /// A `TypeAssert` operand was present but not of the expected class — the
    /// preserved `TypeMismatch` of the eliminated operation.
    #[error("type assertion failed: expected {expected}, got {actual}")]
    TypeAssert {
        expected: &'static str,
        actual: String,
    },

    /// A called function failed.
    #[error(transparent)]
    Function(#[from] FuncError),
}

/// Evaluates a field-free compacted program against a registry and context.
pub struct ProgramEvaluator<'a> {
    program: &'a CompactProgram,
    registry: &'a FunctionRegistry,
    context: &'a EvalContext,
    registers: Vec<Value>,
}

impl<'a> ProgramEvaluator<'a> {
    pub fn create(
        program: &'a CompactProgram,
        registry: &'a FunctionRegistry,
        context: &'a EvalContext,
    ) -> Self {
        let registers = vec![Value::Null; program.register_count() as usize];
        Self {
            program,
            registry,
            context,
            registers,
        }
    }

    /// Run the statements into the register file, then evaluate the result.
    pub fn evaluate(&mut self) -> Result<Value, EvalError> {
        let count = self.program.statements().len();
        for index in 0..count {
            let (register, value_ref) = {
                let statement = &self.program.statements()[index];
                (statement.register, statement.value)
            };
            let value = self.eval_node(value_ref, 0)?;
            self.registers[register as usize] = value;
        }
        self.eval_node(self.program.result(), 0)
    }

    fn eval_node(&mut self, node_ref: NodeRef, depth: usize) -> Result<Value, EvalError> {
        if depth > MAX_EXPR_DEPTH {
            return Err(EvalError::NestingTooDeep {
                max: MAX_EXPR_DEPTH,
            });
        }
        let next = depth + 1;

        // `program` is a copy of the `&'a CompactProgram` reference, so the
        // borrows it hands out (matched node, arg runs) live for `'a` and do not
        // borrow `self` — leaving `&mut self` free for the register take below.
        let program = self.program;
        match program.node(node_ref) {
            OptNode::Const(id) => Ok(program.constant(*id).clone()),
            OptNode::Register(register) => Ok(self.registers[*register as usize].clone()),
            // The annotator proved this is the register's last read on every
            // path, so move the value out instead of cloning; the slot is left
            // `Null` (never read again).
            OptNode::RegisterTake(register) => {
                let register = *register as usize;
                Ok(std::mem::replace(
                    &mut self.registers[register],
                    Value::Null,
                ))
            }
            OptNode::SourceField(_) | OptNode::Field(_) | OptNode::Fields(_) => {
                Err(EvalError::FieldNotBound)
            }
            OptNode::Call { func, args } => {
                let arg_refs = program.args(*args);
                let mut values = Vec::with_capacity(arg_refs.len());
                for arg_ref in arg_refs {
                    values.push(self.eval_node(*arg_ref, next)?);
                }
                let function = self.registry.get_by_ref(*func);
                Ok(function.evaluate(values, self.context)?)
            }
            OptNode::If {
                condition,
                then_branch,
                else_branch,
            } => self.eval_if(*condition, *then_branch, *else_branch, next),
            OptNode::MultiIf { branches, default } => self.eval_multi_if(*branches, *default, next),
            OptNode::IfNull { value, alternative } => {
                let value = self.eval_node(*value, next)?;
                if value.is_null() {
                    self.eval_node(*alternative, next)
                } else {
                    Ok(value)
                }
            }
            OptNode::NullIf { value, sentinel } => {
                let value = self.eval_node(*value, next)?;
                let sentinel = self.eval_node(*sentinel, next)?;
                Ok(if value == sentinel {
                    Value::Null
                } else {
                    value
                })
            }
            OptNode::And { left, right } => self.eval_and(*left, *right, next),
            OptNode::Or { left, right } => self.eval_or(*left, *right, next),
            OptNode::Interpolation(segments) => self.eval_interpolation(*segments, next),
            OptNode::Object { keys, values } => self.eval_object(*keys, *values, next),
            OptNode::Switch {
                inputs,
                table,
                default,
            } => self.eval_switch(*inputs, *table, *default, next),
            OptNode::TypeAssert {
                inner,
                expect,
                on_present,
            } => self.eval_type_assert(*inner, *expect, *on_present, next),
        }
    }

    /// Evaluate `inner`, then reproduce the eliminated operation's null/type
    /// contract: null → `Null`; wrong class → `TypeMismatch`; otherwise yield.
    fn eval_type_assert(
        &mut self,
        inner: NodeRef,
        expect: TypeClass,
        on_present: CompactYield,
        depth: usize,
    ) -> Result<Value, EvalError> {
        let value = self.eval_node(inner, depth)?;
        // `Null` has no data type → propagate null (the eliminated op did too).
        let Some(data_type) = value.data_type() else {
            return Ok(Value::Null);
        };
        if !expect.accepts(&data_type) {
            return Err(EvalError::TypeAssert {
                expected: expect.describe(),
                actual: format!("{data_type:?}"),
            });
        }
        match on_present {
            CompactYield::Identity => Ok(value),
            CompactYield::Const(id) => Ok(self.program.constant(id).clone()),
        }
    }

    fn eval_switch(
        &mut self,
        inputs: ArgSlice,
        table: SwitchTableId,
        default: NodeRef,
        depth: usize,
    ) -> Result<Value, EvalError> {
        let program = self.program;
        let input_refs = program.args(inputs);
        let mut values = Vec::with_capacity(input_refs.len());
        for input_ref in input_refs {
            values.push(self.eval_node(*input_ref, depth)?);
        }

        // A single input keys on the value directly; two inputs form a composite
        // key. An unkeyable value (null / non-keyable type) misses → default.
        let key = if values.len() == 1 {
            Key::from_value(&values[0])
        } else {
            Key::composite(values).ok()
        };

        let switch_table = program.switch_table(table);
        match key.and_then(|key| switch_table.lookup(&key)) {
            Some(branch) => self.eval_node(branch, depth),
            None => self.eval_node(default, depth),
        }
    }

    fn eval_if(
        &mut self,
        condition: NodeRef,
        then_branch: NodeRef,
        else_branch: NodeRef,
        depth: usize,
    ) -> Result<Value, EvalError> {
        let condition = self.eval_node(condition, depth)?;
        match condition {
            Value::Bool(true) => self.eval_node(then_branch, depth),
            Value::Bool(false) | Value::Null => self.eval_node(else_branch, depth),
            other => Err(EvalError::ExpectedBool {
                context: "if",
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }

    fn eval_multi_if(
        &mut self,
        branches: crate::model::ArgSlice,
        default: NodeRef,
        depth: usize,
    ) -> Result<Value, EvalError> {
        let program = self.program;
        let branch_refs = program.args(branches);
        let mut index = 0;
        while index + 1 < branch_refs.len() {
            let condition = self.eval_node(branch_refs[index], depth)?;
            match condition {
                Value::Bool(true) => return self.eval_node(branch_refs[index + 1], depth),
                Value::Bool(false) | Value::Null => {}
                other => {
                    return Err(EvalError::ExpectedBool {
                        context: "multiIf",
                        actual: format!("{:?}", other.data_type()),
                    });
                }
            }
            index += 2;
        }
        self.eval_node(default, depth)
    }

    fn eval_and(
        &mut self,
        left: NodeRef,
        right: NodeRef,
        depth: usize,
    ) -> Result<Value, EvalError> {
        let left = self.eval_node(left, depth)?;
        match left {
            Value::Bool(false) => Ok(Value::Bool(false)),
            Value::Bool(true) => combine_bool(self.eval_node(right, depth)?, "and"),
            // SQL three-valued: NULL AND FALSE = FALSE, NULL AND TRUE/NULL = NULL.
            Value::Null => match self.eval_node(right, depth)? {
                Value::Bool(false) => Ok(Value::Bool(false)),
                Value::Bool(true) | Value::Null => Ok(Value::Null),
                other => Err(expected_bool("and", &other)),
            },
            other => Err(expected_bool("and", &other)),
        }
    }

    fn eval_or(&mut self, left: NodeRef, right: NodeRef, depth: usize) -> Result<Value, EvalError> {
        let left = self.eval_node(left, depth)?;
        match left {
            Value::Bool(true) => Ok(Value::Bool(true)),
            Value::Bool(false) => combine_bool(self.eval_node(right, depth)?, "or"),
            // SQL three-valued: NULL OR TRUE = TRUE, NULL OR FALSE/NULL = NULL.
            Value::Null => match self.eval_node(right, depth)? {
                Value::Bool(true) => Ok(Value::Bool(true)),
                Value::Bool(false) | Value::Null => Ok(Value::Null),
                other => Err(expected_bool("or", &other)),
            },
            other => Err(expected_bool("or", &other)),
        }
    }

    fn eval_interpolation(&mut self, segments: ArgSlice, depth: usize) -> Result<Value, EvalError> {
        let program = self.program;
        let mut rendered = String::new();
        for segment in program.args(segments) {
            let value = self.eval_node(*segment, depth)?;
            rendered.push_str(&value_to_string(&value));
            if rendered.len() > MAX_EXPR_STRING_BYTES {
                return Err(EvalError::StringTooLarge {
                    len: rendered.len(),
                    max: MAX_EXPR_STRING_BYTES,
                });
            }
        }
        Ok(Value::Text(rendered))
    }

    fn eval_object(
        &mut self,
        keys: KeySlice,
        values: ArgSlice,
        depth: usize,
    ) -> Result<Value, EvalError> {
        let program = self.program;
        let key_ids = program.keys(keys);
        let value_refs = program.args(values);
        let mut map = serde_json::Map::with_capacity(key_ids.len());
        for (key_id, value_ref) in key_ids.iter().zip(value_refs) {
            let evaluated = self.eval_node(*value_ref, depth)?;
            let json = air_elt_types::value_to_json(&evaluated).unwrap_or(serde_json::Value::Null);
            map.insert(program.key_name(*key_id).to_string(), json);
        }
        Ok(Value::Json(serde_json::Value::Object(map)))
    }
}

/// Coerce the resolved right operand of `&&` / `||` to its boolean/null result.
fn combine_bool(value: Value, context: &'static str) -> Result<Value, EvalError> {
    match value {
        Value::Null => Ok(Value::Null),
        Value::Bool(boolean) => Ok(Value::Bool(boolean)),
        other => Err(expected_bool(context, &other)),
    }
}

fn expected_bool(context: &'static str, value: &Value) -> EvalError {
    EvalError::ExpectedBool {
        context,
        actual: format!("{:?}", value.data_type()),
    }
}
