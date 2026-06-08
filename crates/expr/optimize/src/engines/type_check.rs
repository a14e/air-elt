//! Static type-check pass — a bottom-up type synthesis over the optimized heap
//! [`OptProgram`] that also **derives the static type map**.
//!
//! Unlike the node-local [`StaticCheckEngine`](crate::check::StaticCheckEngine), this is
//! a **synthesized** walk: a parent's type is derived from its children's, so the
//! traversal is bottom-up and each node yields a [`NullableExprType`]. The
//! per-function type algebra is **not** duplicated — [`OptExpr::Call`] reuses
//! [`ExprFunction::resolve_type`](air_elt_expr_funcs::ExprFunction::resolve_type),
//! exactly as the runtime `TypeResolver` does over the parse AST.
//!
//! **The type map.** As the walk visits each node it records the node's output
//! type under its [`NodeId`] in a [`TypeMap`] (`HashMap<NodeId, Type>`). This pass
//! is the **sole author** of the map — no other pass writes it — so it is a pure
//! derivation that cannot drift. The typed-discharge pass (Phase 3c) reads it back
//! by id to decide which `TypeAssert`/`toString` it can strip. The map keys are
//! the per-compile ids minted at node construction; it is read-only here (this
//! pass mints nothing), and lives on the heap IR only — it never reaches runtime
//! (a surviving `TypeAssert` checks the *value's* type per row).
//!
//! **Unknown propagation.** A node's type is [`None`] (unknown) when it cannot be
//! determined statically — a source field absent from a schemaless schema, or
//! anything derived from one. Unknown propagates up; errors fire only on **known**
//! sub-results, and an unknown node simply gets no map entry. With a
//! [`Fixed`](air_elt_types::SchemaKind::Fixed) schema every `SourceField` is known,
//! so the whole tree types, every check fires, and every node lands in the map.

use ahash::AHashMap;
use air_elt_expr_funcs::FunctionRegistry;
use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::matrix::{is_compatible, is_compatible_with_truncate};
use air_elt_types::{DataType, Schema, Value};

use crate::error::OptimizeError;
use crate::model::node_id::NodeId;
use crate::model::opt_expr::{AssertYield, OptExpr};
use crate::model::opt_program::OptProgram;
use crate::model::program::TypeClass;

/// The static type map: each node's [`NodeId`] to its synthesized output type.
/// Only nodes whose type is statically known appear; an unknown (schemaless)
/// node has no entry. Consumed by the typed-discharge pass (Phase 3c).
pub(crate) type TypeMap = AHashMap<NodeId, NullableExprType>;

/// The expected output a compiled program must produce: the sink column's
/// [`DataType`] plus the mapping's `truncate` flag (which permits the narrowing
/// matrix arms). Passed into [`Optimizer::compile`](crate::Optimizer::compile)
/// so a compute column that cannot produce its sink column's type fails at
/// compile time, before any data moves.
#[derive(Debug, Clone)]
pub struct ExpectedOutput {
    pub data_type: DataType,
    pub truncate: bool,
}

/// A node's synthesized type, or `None` when it cannot be determined statically.
type MaybeType = Option<NullableExprType>;

/// Bottom-up type synthesis over an [`OptProgram`]. Holds the per-register types
/// (an SSA binding is typed once, before any read) accumulated as the statements
/// are walked in order, and the [`TypeMap`] it fills as it goes.
pub(crate) struct TypeChecker<'a> {
    registry: &'a FunctionRegistry,
    schema: Option<&'a Schema>,
    registers: Vec<MaybeType>,
    map: TypeMap,
    /// When `true`, a would-be type error (absent field, non-const field arg,
    /// function type mismatch) yields an *unknown* type instead of raising. The
    /// interleaved fixpoint derives the map this way each round: an intermediate
    /// tree can hold a subtree (e.g. a not-yet-pruned dead branch) that the
    /// converged tree drops, so the per-round derivation must not reject it — only
    /// the final strict pass, on the converged tree, raises real errors.
    tolerant: bool,
}

impl<'a> TypeChecker<'a> {
    pub(crate) fn create(
        registry: &'a FunctionRegistry,
        schema: Option<&'a Schema>,
        register_count: u16,
        tolerant: bool,
    ) -> Self {
        Self {
            registry,
            schema,
            registers: vec![None; register_count as usize],
            map: TypeMap::new(),
            tolerant,
        }
    }

    /// Type-check the program and derive its type map. Types each statement
    /// binding in order (recording its register type for later reads), then the
    /// result. When `expected` is set and the result type is known, validates
    /// output compatibility. Consumes the checker, returning the [`TypeMap`]; the
    /// program's result type is the map entry for `program.result`'s id.
    pub(crate) fn check(
        mut self,
        program: &OptProgram,
        expected: Option<&ExpectedOutput>,
    ) -> Result<TypeMap, OptimizeError> {
        for statement in &program.statements {
            let value_type = self.type_of(&statement.value)?;
            self.registers[statement.register as usize] = value_type;
        }
        let result = self.type_of(&program.result)?;
        if let Some(expected) = expected {
            check_output(result.as_ref(), expected)?;
        }
        Ok(self.map)
    }

    /// Synthesize a node's type and record it in the map under the node's id (only
    /// when known — an unknown node gets no entry). The single recursion point, so
    /// every visited node is mapped.
    fn type_of(&mut self, node: &OptExpr) -> Result<MaybeType, OptimizeError> {
        let result = self.synthesize(node)?;
        if let Some(node_type) = &result {
            self.map.insert(node.id(), node_type.clone());
        }
        Ok(result)
    }

    fn synthesize(&mut self, node: &OptExpr) -> Result<MaybeType, OptimizeError> {
        match node {
            OptExpr::Const(_, value) => Ok(Some(type_of_const(value))),
            OptExpr::Register(_, register) => Ok(self.registers[*register as usize].clone()),
            OptExpr::SourceField(_, name) => self.type_of_source_field(name),
            // A `field(<expr>)` that survived optimization has a non-constant
            // column name — the same condition `FieldArgCheck` rejects. Tolerated
            // as unknown during the per-round derivation (it may sit in a
            // not-yet-pruned dead branch); raised by the final strict pass.
            OptExpr::Field(..) if self.tolerant => Ok(None),
            OptExpr::Field(..) => Err(OptimizeError::NonConstFieldArg),
            OptExpr::Fields(..) => Ok(Some(NullableExprType::non_null(DataType::Object))),
            OptExpr::Call { func, args, .. } => self.type_of_call(*func, args),
            OptExpr::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                self.type_of(condition)?;
                let then_type = self.type_of(then_branch)?;
                let else_type = self.type_of(else_branch)?;
                Ok(merge_branches(then_type, else_type))
            }
            OptExpr::MultiIf {
                branches, default, ..
            } => self.type_of_multi_if(branches, default),
            // The conditional/`&&`/`||` typing rules below mirror the runtime
            // `TypeResolver::check_conditional` (the source of truth); the only
            // intentional divergence is that an unknown operand here yields a
            // conservatively-nullable / unknown result (the runtime never sees
            // unknowns, as it types a fully-resolvable parse AST).
            OptExpr::IfNull {
                value, alternative, ..
            } => {
                let value_type = self.type_of(value)?;
                let alt_type = self.type_of(alternative)?;
                // The value is non-null after the check; nullability comes from
                // the alternative.
                Ok(value_type.map(|value_type| {
                    let alt_nullable = alt_type.is_none_or(|alt| alt.nullable);
                    NullableExprType::new(value_type.data_type, alt_nullable)
                }))
            }
            OptExpr::NullIf {
                value, sentinel, ..
            } => {
                let value_type = self.type_of(value)?;
                self.type_of(sentinel)?;
                // `nullIf` can always produce null.
                Ok(value_type.map(|value_type| NullableExprType::nullable(value_type.data_type)))
            }
            OptExpr::And { left, right, .. } | OptExpr::Or { left, right, .. } => {
                let left_type = self.type_of(left)?;
                let right_type = self.type_of(right)?;
                Ok(Some(bool_combination(left_type, right_type)))
            }
            OptExpr::Interpolation(_, segments) => {
                for segment in segments {
                    self.type_of(segment)?;
                }
                Ok(Some(NullableExprType::non_null(DataType::Text {
                    size: None,
                })))
            }
            OptExpr::Object(_, entries) => {
                for (_key, value) in entries {
                    self.type_of(value)?;
                }
                Ok(Some(NullableExprType::non_null(DataType::Object)))
            }
            OptExpr::Switch {
                inputs,
                table,
                default,
                ..
            } => {
                for input in inputs {
                    self.type_of(input)?;
                }
                let arms = table.iter().map(|(_, value)| value);
                self.type_of_branches(arms, default)
            }
            OptExpr::TypeAssert {
                inner,
                expect,
                on_present,
                ..
            } => {
                self.type_of(inner)?;
                match on_present {
                    AssertYield::Const(value) => Ok(Some(type_of_const(value))),
                    // The assert proves the operand is of `expect`'s class (or
                    // null) — a coarse but real type even when `inner` is unknown.
                    AssertYield::Identity => Ok(Some(NullableExprType::nullable(
                        type_class_data_type(expect),
                    ))),
                }
            }
            // Mirror the program-level statement handling: type each binding in
            // order, record its register type for later reads, then the result.
            // The block's type is its result's type.
            OptExpr::Block {
                statements, result, ..
            } => {
                for statement in statements {
                    let value_type = self.type_of(&statement.value)?;
                    self.registers[statement.register as usize] = value_type;
                }
                self.type_of(result)
            }
        }
    }

    fn type_of_source_field(&self, name: &str) -> Result<MaybeType, OptimizeError> {
        let Some(schema) = self.schema else {
            return Ok(None);
        };
        match schema.find(name) {
            Some(field) => Ok(Some(NullableExprType::new(
                field.data_type.clone(),
                field.nullable,
            ))),
            // A fixed schema is authoritative — an absent field is an error. A
            // schemaless one is not: the field may exist on a given row. During the
            // tolerant per-round derivation an absent field is left unknown (it may
            // be in a dead branch); the final strict pass raises it.
            None if schema.is_schemaless() || self.tolerant => Ok(None),
            None => Err(OptimizeError::FieldNotInSchema {
                name: name.to_owned(),
            }),
        }
    }

    fn type_of_call(
        &mut self,
        func: air_elt_expr_funcs::FuncRef,
        args: &[OptExpr],
    ) -> Result<MaybeType, OptimizeError> {
        let mut arg_types = Vec::with_capacity(args.len());
        let mut all_known = true;
        // Recurse into every argument regardless, so an error in an argument
        // surfaces even when an earlier argument was unknown.
        for arg in args {
            match self.type_of(arg)? {
                Some(arg_type) => arg_types.push(arg_type),
                None => all_known = false,
            }
        }
        if !all_known {
            return Ok(None);
        }
        let function = self.registry.get_by_ref(func);
        // Reuse the per-function type algebra; a `TypeMismatch` becomes a
        // compile-time type error via `OptimizeError: From<FuncError>`. During the
        // tolerant per-round derivation a mismatch yields unknown instead (the call
        // may be in a dead branch); the final strict pass raises it.
        match function.resolve_type(&arg_types) {
            Ok(result_type) => Ok(Some(result_type)),
            Err(_) if self.tolerant => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn type_of_multi_if(
        &mut self,
        branches: &[(OptExpr, OptExpr)],
        default: &OptExpr,
    ) -> Result<MaybeType, OptimizeError> {
        for (condition, _value) in branches {
            self.type_of(condition)?;
        }
        let values = branches.iter().map(|(_, value)| value);
        self.type_of_branches(values, default)
    }

    /// Type a set of branch values plus a default: the result data type is the
    /// first branch's (the default's if there are none), nullable if any branch
    /// or the default is. Unknown if any participating value is unknown.
    fn type_of_branches<'b>(
        &mut self,
        values: impl Iterator<Item = &'b OptExpr>,
        default: &OptExpr,
    ) -> Result<MaybeType, OptimizeError> {
        let mut data_type = None;
        let mut nullable = false;
        let mut all_known = true;
        for value in values {
            match self.type_of(value)? {
                Some(value_type) => {
                    if data_type.is_none() {
                        data_type = Some(value_type.data_type);
                    }
                    nullable |= value_type.nullable;
                }
                None => all_known = false,
            }
        }
        let default_type = self.type_of(default)?;
        match default_type {
            Some(default_type) => {
                if data_type.is_none() {
                    data_type = Some(default_type.data_type);
                }
                nullable |= default_type.nullable;
            }
            None => all_known = false,
        }
        if !all_known {
            return Ok(None);
        }
        Ok(data_type.map(|data_type| NullableExprType::new(data_type, nullable)))
    }
}

/// Validate the resolved result type against the expected sink type. The
/// **materialized** data type is used so a bounded integer (e.g. `1 + 1`, bound
/// 2) matches a narrow sink column. `None` (unknown result) is permitted — the
/// untyped/schemaless path defers the type guarantee to the runtime asserts.
fn check_output(
    result: Option<&NullableExprType>,
    expected: &ExpectedOutput,
) -> Result<(), OptimizeError> {
    let Some(result) = result else {
        return Ok(());
    };
    let source = result.materialized_data_type();
    let compatible = if expected.truncate {
        is_compatible_with_truncate(source.clone(), expected.data_type.clone())
    } else {
        is_compatible(source.clone(), expected.data_type.clone())
    };
    if compatible {
        return Ok(());
    }
    Err(OptimizeError::OutputTypeMismatch {
        resolved: format!("{source}"),
        expected: format!("{}", expected.data_type),
    })
}

/// The synthesized type of a constant. Mirrors the runtime `TypeResolver`'s
/// literal typing: a null types as nullable `Bool` (the language's current null
/// literal type), an integer carries its significant-bit bound so a small
/// constant materializes to a narrow type.
fn type_of_const(value: &Value) -> NullableExprType {
    match value {
        Value::Null => NullableExprType::nullable(DataType::Bool),
        _ => {
            let data_type = value
                .data_type()
                .expect("a non-null value always has a data type");
            match int_bound(value) {
                Some(bound) => NullableExprType::int_with_bound(data_type, bound),
                None => NullableExprType::non_null(data_type),
            }
        }
    }
}

/// Significant-bit bound for a signed integer constant (so it can materialize to
/// the smallest fitting type), or `None` for non-(signed-integer) values whose
/// `data_type()` is already their final type.
fn int_bound(value: &Value) -> Option<u8> {
    let magnitude: u64 = match value {
        Value::Int8(int) => int.unsigned_abs() as u64,
        Value::Int16(int) => int.unsigned_abs() as u64,
        Value::Int32(int) => int.unsigned_abs() as u64,
        Value::Int64(int) => int.unsigned_abs(),
        _ => return None,
    };
    let bits = if magnitude == 0 {
        1
    } else {
        (64 - magnitude.leading_zeros()) as u8
    };
    Some(bits)
}

/// The representative data type a [`TypeClass`] guarantees (the coarse type a
/// surviving `TypeAssert` proves about its operand).
fn type_class_data_type(class: &TypeClass) -> DataType {
    match class {
        TypeClass::String => DataType::Text { size: None },
        TypeClass::Bool => DataType::Bool,
        TypeClass::Bytes => DataType::Bytes { size: None },
    }
}

/// `if`-style branch merge: data type from the then-branch, nullable if either
/// branch is. Unknown if either branch is unknown. The integer bound is dropped
/// (a conditional's value is not a single literal).
fn merge_branches(then_type: MaybeType, else_type: MaybeType) -> MaybeType {
    match (then_type, else_type) {
        (Some(then_type), Some(else_type)) => Some(NullableExprType::new(
            then_type.data_type,
            then_type.nullable || else_type.nullable,
        )),
        _ => None,
    }
}

/// `&&` / `||` always yield `Bool`; nullability follows the operands (Kleene
/// three-valued logic can produce null), and is taken conservatively nullable
/// when an operand type is unknown.
fn bool_combination(left: MaybeType, right: MaybeType) -> NullableExprType {
    let nullable = match (left, right) {
        (Some(left), Some(right)) => left.nullable || right.nullable,
        _ => true,
    };
    NullableExprType::new(DataType::Bool, nullable)
}
