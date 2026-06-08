//! Static-analysis pass: a read-only walk over the optimized program that
//! surfaces definite compile-time errors. It runs ONCE after the rewrite
//! fixpoint converges — every prunable dead branch is already gone, and every
//! foldable literal already folded, so what survives is what really runs.
//!
//! [`StaticCheckEngine`](engine::StaticCheckEngine) drives a single
//! position-aware traversal: it knows which child positions are **eager**
//! (always evaluated) versus **lazy** (reached only on a branch/short-circuit).
//! At every node it runs the registered [`Check`](engine::Check)s, handing each
//! the node and its `eager` flag. The checks split into two philosophies:
//!
//! * **Value-failure** ([`EagerConstEval`](eager_const::EagerConstEval)) — fully
//!   evaluate an all-constant pure call. Fires in EAGER positions only: whether
//!   `1 / 0` errors depends on the path being taken, so a lazy occurrence defers
//!   to runtime.
//! * **Structural / categorical** ([`ConstArgsValidation`](const_args::ConstArgsValidation),
//!   [`FieldArgCheck`](field_arg::FieldArgCheck), and
//!   [`ConjunctionInfeasibility`](conjunction::ConjunctionInfeasibility)) — a
//!   malformed inlined literal, a `field` with a non-constant name, or a `&&`
//!   chain that asserts an operand is two incompatible types can never be valid
//!   regardless of the path, so these fire EVERYWHERE.

mod conjunction;
mod const_args;
mod eager_const;
mod engine;
mod field_arg;

pub(crate) use engine::{Check, CheckCx, StaticCheckEngine};
