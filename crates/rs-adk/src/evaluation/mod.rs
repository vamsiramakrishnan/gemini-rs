//! Evaluation framework — evaluate agent quality using various metrics.
//!
//! Mirrors ADK-Python's `evaluation` module. Provides traits and types for
//! evaluating agent invocations against expected results, including
//! LLM-as-judge, trajectory, response, rubric, hallucination, safety,
//! and user simulator evaluators.

mod eval_case;
mod eval_result;
mod evalset_parser;
mod evaluator;
mod hallucination_evaluator;
mod llm_as_judge;
mod match_type;
mod response_evaluator;
mod rubric_evaluator;
mod safety_evaluator;
mod test_config;
mod trajectory_evaluator;
mod user_simulator_evaluator;

pub use eval_case::{EvalCase, EvalSet, Invocation, InvocationTurn};
pub use eval_result::{EvalMetric, EvalResult, PerInvocationResult};
pub use evalset_parser::{
    parse_evalset, parse_evalset_str, EvalCaseFile, EvalSetFile, ExpectedToolUse, IntermediateData,
    InvocationFile, ToolUseRecord,
};
pub use evaluator::{EvalError, Evaluator};
pub use hallucination_evaluator::HallucinationEvaluator;
pub use llm_as_judge::{LlmAsJudge, LlmAsJudgeConfig};
pub use match_type::TrajectoryMatchType;
pub use response_evaluator::{MatchStrategy, ResponseEvaluator};
pub use rubric_evaluator::{RubricEvaluator, RubricMode};
pub use safety_evaluator::{SafetyCategory, SafetyEvaluator, SafetySignal};
pub use test_config::{parse_test_config, parse_test_config_str, CriterionConfig, TestConfig};
pub use trajectory_evaluator::TrajectoryEvaluator;
pub use user_simulator_evaluator::UserSimulatorEvaluator;
