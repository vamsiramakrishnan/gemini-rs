//! Core evaluator trait — base interface for all evaluators.

use async_trait::async_trait;

use super::eval_case::Invocation;
use super::eval_result::EvalResult;

/// Errors from evaluation operations.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// The evaluator encountered an error during evaluation.
    #[error("Evaluation error: {0}")]
    Evaluation(String),
    /// The LLM call failed.
    #[error("LLM error: {0}")]
    Llm(String),
    /// Invalid input.
    #[error("Invalid input: {0}")]
    InvalidInput(String),
    /// I/O error (file read/write).
    #[error("IO error: {0}")]
    Io(String),
    /// Parse error (JSON, config, evalset).
    #[error("Parse error: {0}")]
    Parse(String),
}

/// Trait for evaluating agent invocations against expected results.
///
/// Mirrors ADK-Python's `Evaluator` abstract class.
#[async_trait]
pub trait Evaluator: Send + Sync {
    /// Evaluate agent invocations.
    ///
    /// # Arguments
    /// * `actual` — The agent's actual invocations.
    /// * `expected` — Optional expected (golden) invocations for comparison.
    ///
    /// # Returns
    /// An [`EvalResult`] containing per-metric and overall scores.
    async fn evaluate(
        &self,
        actual: &[Invocation],
        expected: Option<&[Invocation]>,
    ) -> Result<EvalResult, EvalError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn _assert_object_safe(_: &dyn Evaluator) {}

    #[test]
    fn evaluator_is_object_safe() {
        // Compile-time check only
    }

    #[test]
    fn eval_error_display() {
        let err = EvalError::Evaluation("test".into());
        assert!(err.to_string().contains("test"));
    }
}
