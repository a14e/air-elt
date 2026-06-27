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
use air_elt_expr_funcs::{ArgWindow, FuncError, FuncRef, FunctionRegistry};
use air_elt_expr_parse::FieldsSelector;
use air_elt_expr_types::limits::{MAX_EXPR_DEPTH, MAX_EXPR_STRING_BYTES};
use air_elt_types::value_to_string;
use air_elt_types::{Key, Value};
use thiserror::Error;

use crate::model::program::{
    ArgSlice, CompactProgram, CompactYield, ConstId, KeySlice, NodeRef, OptNode, RegisterId,
    SwitchTableId, TypeClass,
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

/// Supplies source-column values to the evaluator. The field-free correctness
/// oracle uses [`NoFields`] (every access is a [`EvalError::FieldNotBound`]); the
/// per-row runtime binds a real row. Treating an access as unbound is always a
/// well-defined error, so an evaluator over the field-free subset stays correct.
pub trait FieldSource {
    /// The value of source column `name`.
    fn field(&self, name: &str) -> Result<Value, EvalError>;

    /// The object produced by `fields("*")` / `fields("a,b")`.
    fn fields(&self, selector: &FieldsSelector) -> Result<Value, EvalError>;
}

/// The field-free source: every source-column access is unbound. Used by the
/// optimizer's correctness oracle, which only exercises the field-free subset.
pub struct NoFields;

impl FieldSource for NoFields {
    fn field(&self, _name: &str) -> Result<Value, EvalError> {
        Err(EvalError::FieldNotBound)
    }

    fn fields(&self, _selector: &FieldsSelector) -> Result<Value, EvalError> {
        Err(EvalError::FieldNotBound)
    }
}

static NO_FIELDS: NoFields = NoFields;

/// Evaluates a compacted program against a registry, context, and field source.
/// With [`NoFields`] it covers the field-free subset (constants, operators,
/// conditionals, variables); with a row-bound [`FieldSource`] it is the per-row
/// runtime evaluator.
pub struct ProgramEvaluator<'a> {
    program: &'a CompactProgram,
    registry: &'a FunctionRegistry,
    context: &'a EvalContext,
    fields: &'a dyn FieldSource,
    registers: Vec<Value>,
    /// Reusable argument stack. Each call pushes one [`ArgStackItem`] per
    /// argument above `base = arg_stack.len()` (constants and registers as a
    /// tag carrying only an index — no copy; sub-expressions as an owned
    /// `Value`) and truncates back to `base` once the function returns. One
    /// allocation is reused across every call within an evaluation, and argument
    /// `i` of a call is simply `arg_stack[base + i]` — no slot bookkeeping.
    arg_stack: Vec<ArgStackItem>,
}

impl<'a> ProgramEvaluator<'a> {
    /// Field-free evaluator (the correctness oracle): any source-field access is
    /// a [`EvalError::FieldNotBound`].
    pub fn create(
        program: &'a CompactProgram,
        registry: &'a FunctionRegistry,
        context: &'a EvalContext,
    ) -> Self {
        Self::create_with_fields(program, registry, context, &NO_FIELDS)
    }

    /// Evaluator bound to a row's [`FieldSource`] — the per-row runtime path.
    pub fn create_with_fields(
        program: &'a CompactProgram,
        registry: &'a FunctionRegistry,
        context: &'a EvalContext,
        fields: &'a dyn FieldSource,
    ) -> Self {
        let registers = vec![Value::Null; program.register_count() as usize];
        Self {
            program,
            registry,
            context,
            fields,
            registers,
            arg_stack: Vec::new(),
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

        // `program`/`fields` are copies of the `&'a` references, so the borrows
        // they hand out (matched node, arg runs, a source-field value) live for
        // `'a` and do not borrow `self` — leaving `&mut self` free for the
        // register take below.
        let program = self.program;
        let fields = self.fields;
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
            OptNode::SourceField(name) => fields.field(name),
            OptNode::Fields(selector) => fields.fields(selector),
            // `field(<dynamic>)` is rejected by the type-check pass; a survivor
            // has no row binding.
            OptNode::Field(_) => Err(EvalError::FieldNotBound),
            OptNode::Call { func, args } => self.eval_call(*func, *args, next),
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
            OptNode::Array(values) => self.eval_array(*values, next),
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
            // Evaluate the binding into its register, then the body. Reached only
            // when control descends here, so the binding is written lazily — a
            // block inside an untaken branch never runs.
            OptNode::Bind {
                register,
                value,
                body,
            } => {
                let bound = self.eval_node(*value, next)?;
                self.registers[*register as usize] = bound;
                self.eval_node(*body, next)
            }
        }
    }

    /// Evaluate a function call. Each argument becomes one [`ArgStackItem`] on
    /// the reusable argument stack: a constant or a register is a tag that reads
    /// its storage in place with no copy; every other argument is a
    /// sub-expression evaluated left-to-right onto the stack — preserving the
    /// eager failure/effect order, since a failing sub-expression aborts before
    /// later ones run. The function then reaches its arguments through an
    /// [`ArenaArgWindow`], borrowing the read-only ones ([`read`](ArgWindow::read))
    /// and moving the ones it consumes ([`take`](ArgWindow::take)). The stack is
    /// truncated back to its base whether the call succeeds or fails, so an error
    /// never leaves it dirty.
    fn eval_call(
        &mut self,
        func: FuncRef,
        args: ArgSlice,
        depth: usize,
    ) -> Result<Value, EvalError> {
        let program = self.program;
        let arg_refs = program.args(args);
        let base = self.arg_stack.len();
        // Push one item per argument, left-to-right. A constant or register is a
        // tag carrying only its index — read in place from the arena, never
        // copied. A sub-expression is evaluated now (so eager failure/effect
        // order is preserved — a failing sub-expression aborts before later ones
        // run) and stored as an owned `Value`.
        for arg_ref in arg_refs {
            let item = match program.node(*arg_ref) {
                OptNode::Const(id) => ArgStackItem::Const(*id),
                OptNode::Register(register) => ArgStackItem::Register(*register),
                OptNode::RegisterTake(register) => ArgStackItem::RegisterTake(*register),
                _ => ArgStackItem::Value(self.eval_node(*arg_ref, depth)?),
            };
            self.arg_stack.push(item);
        }

        let function = self.registry.get_by_ref(func);
        let outcome = {
            let mut window = ArenaArgWindow {
                items: &mut self.arg_stack[base..],
                program,
                registers: &mut self.registers,
            };
            function.evaluate(&mut window, self.context)
        };
        self.arg_stack.truncate(base);
        Ok(outcome?)
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
        // A single input (the common compiled shape) keys on the value directly
        // — no Vec; two inputs form a composite key. An unkeyable value (null /
        // non-keyable type) misses → default.
        let key = if let [input_ref] = input_refs {
            let value = self.eval_node(*input_ref, depth)?;
            Key::from_value(&value)
        } else {
            let mut values = Vec::with_capacity(input_refs.len());
            for input_ref in input_refs {
                values.push(self.eval_node(*input_ref, depth)?);
            }
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
            // Text (the dominant segment type) appends directly; the
            // `value_to_string` path would clone it into an intermediate String.
            match &value {
                Value::Text(text) => rendered.push_str(text),
                other => rendered.push_str(&value_to_string(other)),
            }
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
        let mut fields = Vec::with_capacity(key_ids.len());
        for (key_id, value_ref) in key_ids.iter().zip(value_refs) {
            let evaluated = self.eval_node(*value_ref, depth)?;
            fields.push((program.key_name(*key_id).to_string(), evaluated));
        }
        Ok(Value::Object(fields))
    }

    fn eval_array(&mut self, values: ArgSlice, depth: usize) -> Result<Value, EvalError> {
        let program = self.program;
        let value_refs = program.args(values);
        let mut elements = Vec::with_capacity(value_refs.len());
        for value_ref in value_refs {
            elements.push(self.eval_node(*value_ref, depth)?);
        }
        Ok(Value::Array(elements))
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

/// One argument on the evaluator's reusable argument stack. A constant or a
/// register carries only its index — read in place from the const pool or the
/// register file, never copied; a sub-expression carries its computed value
/// inline. So the descriptor and the value stack are one and the same, and
/// argument `i` of a call is the `i`-th item of the call's stack region.
enum ArgStackItem {
    /// A computed sub-expression result, owned inline (`take` moves it out).
    Value(Value),
    /// A constant in the program's interned pool (`take` clones — it is shared).
    Const(ConstId),
    /// A register read again later (`take` clones to preserve it).
    Register(RegisterId),
    /// A register at its proven last use (`take` moves it out of the register).
    RegisterTake(RegisterId),
}

/// The arena evaluator's [`ArgWindow`] over a call's region of the argument
/// stack. A [`read`](ArgWindow::read) borrows the value in place — from the
/// const pool, the register file, or the inline slot — copying nothing; a
/// [`take`](ArgWindow::take) moves the values the window owns (inline
/// sub-expressions and last-use registers) and clones the ones it only aliases
/// (constants and non-last-use registers).
struct ArenaArgWindow<'w> {
    items: &'w mut [ArgStackItem],
    program: &'w CompactProgram,
    registers: &'w mut Vec<Value>,
}

impl ArgWindow for ArenaArgWindow<'_> {
    fn len(&self) -> usize {
        self.items.len()
    }

    fn read(&self, index: usize) -> &Value {
        match &self.items[index] {
            ArgStackItem::Value(value) => value,
            ArgStackItem::Const(id) => self.program.constant(*id),
            ArgStackItem::Register(register) | ArgStackItem::RegisterTake(register) => {
                &self.registers[*register as usize]
            }
        }
    }

    fn take(&mut self, index: usize) -> Value {
        match &mut self.items[index] {
            ArgStackItem::Value(value) => std::mem::replace(value, Value::Null),
            ArgStackItem::Const(id) => self.program.constant(*id).clone(),
            ArgStackItem::Register(register) => self.registers[*register as usize].clone(),
            ArgStackItem::RegisterTake(register) => {
                std::mem::replace(&mut self.registers[*register as usize], Value::Null)
            }
        }
    }
}
