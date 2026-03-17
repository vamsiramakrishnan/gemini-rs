//! Sampler trait — provides evaluation examples for optimization.

use async_trait::async_trait;

use super::optimizer::OptimizerError;
use crate::evaluation::EvalCase;

/// A sample drawn from the evaluation set for optimization.
#[derive(Debug, Clone)]
pub struct EvalSample {
    /// The evaluation cases in this sample.
    pub cases: Vec<EvalCase>,
    /// IDs of the sampled cases.
    pub case_ids: Vec<String>,
}

/// Trait for sampling evaluation examples during optimization.
///
/// Implementations provide training/validation splits and scoring.
#[async_trait]
pub trait Sampler: Send + Sync {
    /// Sample a batch of training examples.
    async fn sample_training(&self, batch_size: usize)
        -> Result<EvalSample, OptimizerError>;

    /// Get the full validation set.
    async fn validation_set(&self) -> Result<EvalSample, OptimizerError>;

    /// Score an agent instruction against a set of evaluation cases.
    ///
    /// Returns a score between 0.0 and 1.0.
    async fn score(
        &self,
        instruction: &str,
        model_id: &str,
        cases: &[EvalCase],
    ) -> Result<f64, OptimizerError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn _assert_object_safe(_: &dyn Sampler) {}
}
