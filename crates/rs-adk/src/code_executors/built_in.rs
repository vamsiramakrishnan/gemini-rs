use async_trait::async_trait;

use crate::llm::LlmRequest;
use crate::utils::model_name::is_gemini2_or_above;

use super::base::{CodeExecutor, CodeExecutorError};
use super::types::{CodeExecutionInput, CodeExecutionResult};

/// Server-side code executor (Gemini 2.0+). Does not run code locally.
/// Adds `{codeExecution: {}}` to the LLM request tools.
pub struct BuiltInCodeExecutor;

impl BuiltInCodeExecutor {
    /// Add code execution capability to the LLM request.
    /// Returns error if model is not Gemini 2.0+.
    pub fn process_llm_request(
        &self,
        request: &mut LlmRequest,
        model: &str,
    ) -> Result<(), CodeExecutorError> {
        if !is_gemini2_or_above(model) {
            return Err(CodeExecutorError::UnsupportedModel(format!(
                "Built-in code execution requires Gemini 2.0+, got: {}",
                model
            )));
        }
        request.tools.push(rs_genai::prelude::Tool::code_execution());
        Ok(())
    }
}

#[async_trait]
impl CodeExecutor for BuiltInCodeExecutor {
    async fn execute_code(
        &self,
        _input: CodeExecutionInput,
    ) -> Result<CodeExecutionResult, CodeExecutorError> {
        // Server-side execution — no local result needed
        Ok(CodeExecutionResult::empty())
    }
}
