//! The typed pass: rewrites that only a static type map can justify.
//!
//! Phase 2 parks every type-dependent simplification behind a runtime
//! [`TypeAssert`](crate::model::opt_expr::OptExpr::TypeAssert) or a defensive cast,
//! because the untyped IR cannot prove an operand's type. Once the type-check pass
//! ([`engines::type_check`](crate::engines)) has derived the static type map
//! (`AHashMap<NodeId, Type>`), this pass discharges that parked work and applies
//! the algebraic simplifications a known type makes sound: stripping redundant
//! type asserts and casts, type-gated `min`/`max`/`concat` flatten, string `+` →
//! `concat`, identity/annihilation peepholes, and power reduction.
//!
//! It mirrors the untyped [`rules`](crate::rules) engine — a [`TypedRule`](engine::TypedRule)
//! trait, a [`TypedRuleSet`], and a bottom-up [`TypedRewriteDriver`] (all in
//! [`engine`]) — but every rule additionally consults the type map. It is isolated
//! here because it is the only pass that consumes the map, and runs as a single
//! bottom-up sweep after the type-check pass and before compaction.

pub(crate) mod algebraic_identities;
pub(crate) mod engine;
pub(crate) mod flatten;
pub(crate) mod power_reduce;
pub(crate) mod strip;

pub(crate) use engine::{TypedRewriteDriver, TypedRuleSet};
