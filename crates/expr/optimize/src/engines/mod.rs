//! The standalone (non-rule) engines that [`Optimizer`](crate::optimizer)
//! composes: IR conversion, downward guard propagation, arena compaction, and
//! register move annotation. These are not rule plugins — each is a whole-program
//! pass — so they live here rather than with the rule-based engines in
//! [`rules`](crate::rules), [`second_pass_rules`](crate::second_pass_rules), and
//! [`check`](crate::check). [`ProgramEvaluator`] also lives here but is not part
//! of the compile pipeline — it is the field-free correctness oracle the tests
//! run over a `CompactProgram`.

pub(crate) mod compact;
pub(crate) mod evaluator;
pub(crate) mod guard_propagation;
pub(crate) mod move_annotator;
pub(crate) mod opt_program_converter;

pub(crate) use compact::Compactor;
pub use evaluator::{EvalError, ProgramEvaluator};
pub(crate) use guard_propagation::GuardPropagation;
pub(crate) use move_annotator::MoveAnnotator;
pub(crate) use opt_program_converter::OptProgramConverter;
