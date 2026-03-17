//! Simple prompt optimizer — iteratively rewrites instructions using an LLM.
//!
//! Mirrors ADK-Python's `SimplePromptOptimizer`.

use std::sync::Arc;

use async_trait::async_trait;

use super::optimizer::{AgentOptimizer, OptimizerError, OptimizerResult};
use super::sampler::Sampler;
use crate::llm::BaseLlm;

/// Configuration for the simple prompt optimizer.
#[derive(Debug, Clone)]
pub struct SimplePromptOptimizerConfig {
    /// Number of optimization iterations.
    pub num_iterations: usize,
    /// Number of training examples per evaluation batch.
    pub batch_size: usize,
}

impl Default for SimplePromptOptimizerConfig {
    fn default() -> Self {
        Self {
            num_iterations: 10,
            batch_size: 5,
        }
    }
}

/// Simple prompt optimizer that uses an LLM to iteratively rewrite instructions.
///
/// Process:
/// 1. Score the initial instruction on a training batch
/// 2. For each iteration, generate a candidate instruction using the optimizer LLM
/// 3. Score the candidate — keep it if it improves on the best score
/// 4. Validate the best instruction on the validation set
pub struct SimplePromptOptimizer {
    /// The LLM used to generate candidate instructions.
    optimizer_llm: Arc<dyn BaseLlm>,
    /// Sampler for training/validation examples.
    sampler: Arc<dyn Sampler>,
    /// Optimizer configuration.
    config: SimplePromptOptimizerConfig,
}

impl SimplePromptOptimizer {
    /// Create a new simple prompt optimizer.
    pub fn new(
        optimizer_llm: Arc<dyn BaseLlm>,
        sampler: Arc<dyn Sampler>,
        config: SimplePromptOptimizerConfig,
    ) -> Self {
        Self {
            optimizer_llm,
            sampler,
            config,
        }
    }

    /// Generate a candidate instruction by asking the optimizer LLM to improve it.
    async fn generate_candidate(
        &self,
        current_instruction: &str,
        current_score: f64,
    ) -> Result<String, OptimizerError> {
        let prompt = format!(
            "You are an expert prompt engineer. Your task is to improve the following \
             agent instruction to achieve better performance.\n\n\
             Current instruction (score: {current_score:.2}):\n\
             ---\n{current_instruction}\n---\n\n\
             Generate an improved version of the instruction. Focus on:\n\
             - Clarity and specificity\n\
             - Better task decomposition guidance\n\
             - More effective tool use instructions\n\
             - Appropriate constraints and guardrails\n\n\
             Respond with ONLY the improved instruction text, nothing else."
        );

        let request = crate::llm::LlmRequest::from_text(&prompt);
        let response = self
            .optimizer_llm
            .generate(request)
            .await
            .map_err(|e| OptimizerError::Llm(e.to_string()))?;

        Ok(response.text())
    }
}

#[async_trait]
impl AgentOptimizer for SimplePromptOptimizer {
    async fn optimize(
        &self,
        initial_instruction: &str,
        model_id: &str,
    ) -> Result<OptimizerResult, OptimizerError> {
        let mut best_instruction = initial_instruction.to_string();
        let mut score_history = Vec::new();

        // Score baseline
        let training_batch = self.sampler.sample_training(self.config.batch_size).await?;
        let mut best_score = self
            .sampler
            .score(&best_instruction, model_id, &training_batch.cases)
            .await?;
        score_history.push((0, best_score));

        // Optimization loop
        for iteration in 1..=self.config.num_iterations {
            let candidate = self
                .generate_candidate(&best_instruction, best_score)
                .await?;

            let training_batch = self.sampler.sample_training(self.config.batch_size).await?;
            let candidate_score = self
                .sampler
                .score(&candidate, model_id, &training_batch.cases)
                .await?;

            score_history.push((iteration, candidate_score));

            if candidate_score > best_score {
                best_instruction = candidate;
                best_score = candidate_score;
            }
        }

        // Final validation
        let validation = self.sampler.validation_set().await?;
        let validation_score = self
            .sampler
            .score(&best_instruction, model_id, &validation.cases)
            .await?;

        Ok(OptimizerResult {
            best_instruction,
            best_score: validation_score,
            iterations: self.config.num_iterations,
            score_history,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = SimplePromptOptimizerConfig::default();
        assert_eq!(config.num_iterations, 10);
        assert_eq!(config.batch_size, 5);
    }
}
