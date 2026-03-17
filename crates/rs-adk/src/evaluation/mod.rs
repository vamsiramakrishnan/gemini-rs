//! Evaluation framework — evaluate agent quality using various metrics.
//!
//! Mirrors ADK-Python's `evaluation` module. Provides traits and types for
//! evaluating agent invocations against expected results, including
//! LLM-as-judge, trajectory, and response evaluators.

mod eval_case;
mod eval_result;
mod evaluator;
mod llm_as_judge;
mod response_evaluator;
mod trajectory_evaluator;

pub use eval_case::{EvalCase, EvalSet, Invocation, InvocationTurn};
pub use eval_result::{EvalMetric, EvalResult, PerInvocationResult};
pub use evaluator::Evaluator;
pub use llm_as_judge::LlmAsJudge;
pub use response_evaluator::ResponseEvaluator;
pub use trajectory_evaluator::TrajectoryEvaluator;
