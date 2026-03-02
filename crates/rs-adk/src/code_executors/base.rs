use async_trait::async_trait;
use super::types::{CodeExecutionInput, CodeExecutionResult};

/// Errors from code executor operations.
#[derive(Debug, thiserror::Error)]
pub enum CodeExecutorError {
    #[error("Code execution failed: {0}")]
    ExecutionFailed(String),
    #[error("Unsupported model: {0}")]
    UnsupportedModel(String),
    #[error("{0}")]
    Other(String),
}

#[async_trait]
pub trait CodeExecutor: Send + Sync {
    /// Delimiters for identifying code blocks in model output.
    fn code_block_delimiters(&self) -> Vec<(String, String)> {
        vec![
            ("```tool_code\n".into(), "\n```".into()),
            ("```python\n".into(), "\n```".into()),
        ]
    }

    /// Delimiters for wrapping execution results.
    fn execution_result_delimiters(&self) -> (String, String) {
        ("```tool_output\n".into(), "\n```".into())
    }

    /// Number of retry attempts on error.
    fn error_retry_attempts(&self) -> u32 {
        2
    }

    /// Whether this executor maintains state across executions.
    fn stateful(&self) -> bool {
        false
    }

    /// Execute code and return the result.
    async fn execute_code(
        &self,
        input: CodeExecutionInput,
    ) -> Result<CodeExecutionResult, CodeExecutorError>;
}
