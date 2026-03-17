use super::types::{CodeExecutionInput, CodeExecutionResult};
use async_trait::async_trait;

/// Errors from code executor operations.
#[derive(Debug, thiserror::Error)]
pub enum CodeExecutorError {
    /// The code execution failed at runtime.
    #[error("Code execution failed: {0}")]
    ExecutionFailed(String),
    /// The requested model does not support code execution.
    #[error("Unsupported model: {0}")]
    UnsupportedModel(String),
    /// The execution timed out.
    #[error("Execution timed out after {0}s")]
    Timeout(u64),
    /// A catch-all for other code executor errors.
    #[error("{0}")]
    Other(String),
}

/// Trait for sandboxed code execution in agent pipelines.
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
