//! Base optimizer trait and result types.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Errors from optimization operations.
#[derive(Debug, thiserror::Error)]
pub enum OptimizerError {
    /// Sampling failed.
    #[error("Sampling error: {0}")]
    Sampling(String),
    /// Evaluation failed.
    #[error("Evaluation error: {0}")]
    Evaluation(String),
    /// LLM generation failed.
    #[error("LLM error: {0}")]
    Llm(String),
    /// Optimization logic error.
    #[error("Optimization error: {0}")]
    Optimization(String),
}

/// Result of an optimization run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizerResult {
    /// The best instruction found during optimization.
    pub best_instruction: String,
    /// Score of the best instruction on the validation set.
    pub best_score: f64,
    /// Number of iterations performed.
    pub iterations: usize,
    /// Score history across iterations (iteration_number, score).
    pub score_history: Vec<(usize, f64)>,
}

/// Trait for agent optimizers that iteratively improve agent instructions.
///
/// Mirrors ADK-Python's `AgentOptimizer` abstract class.
#[async_trait]
pub trait AgentOptimizer: Send + Sync {
    /// Run the optimization process.
    ///
    /// # Arguments
    /// * `initial_instruction` — The starting agent instruction to optimize.
    /// * `model_id` — The model to use for the agent being optimized.
    ///
    /// # Returns
    /// An [`OptimizerResult`] with the best instruction and scores.
    async fn optimize(
        &self,
        initial_instruction: &str,
        model_id: &str,
    ) -> Result<OptimizerResult, OptimizerError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn _assert_object_safe(_: &dyn AgentOptimizer) {}

    #[test]
    fn optimizer_result_serde() {
        let result = OptimizerResult {
            best_instruction: "Be helpful".into(),
            best_score: 0.9,
            iterations: 5,
            score_history: vec![(0, 0.5), (1, 0.7), (2, 0.9)],
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: OptimizerResult = serde_json::from_str(&json).unwrap();
        assert!((deserialized.best_score - 0.9).abs() < f64::EPSILON);
    }
}
