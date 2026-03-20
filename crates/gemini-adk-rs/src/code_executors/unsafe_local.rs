//! Unsafe local code executor — runs code directly on the host.
//!
//! Mirrors ADK-Python's `unsafe_local_code_executor`. Runs Python code
//! directly on the host machine without any sandboxing.
//!
//! **WARNING**: This executor provides NO isolation. Only use in
//! trusted development environments.

use async_trait::async_trait;

use super::base::{CodeExecutor, CodeExecutorError};
use super::types::{CodeExecutionInput, CodeExecutionResult};

/// Code executor that runs Python code directly on the host.
///
/// # Safety
///
/// This executor provides **NO sandboxing**. Code runs with the same
/// permissions as the host process. Only use in trusted development
/// environments where code is known to be safe.
#[derive(Debug, Clone, Default)]
pub struct UnsafeLocalCodeExecutor {
    /// Execution timeout in seconds.
    timeout_secs: u64,
}

impl UnsafeLocalCodeExecutor {
    /// Create a new unsafe local executor with a 30-second timeout.
    pub fn new() -> Self {
        Self { timeout_secs: 30 }
    }

    /// Set the execution timeout.
    pub fn with_timeout(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }
}

#[async_trait]
impl CodeExecutor for UnsafeLocalCodeExecutor {
    async fn execute_code(
        &self,
        input: CodeExecutionInput,
    ) -> Result<CodeExecutionResult, CodeExecutorError> {
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(self.timeout_secs),
            tokio::process::Command::new("python3")
                .arg("-c")
                .arg(&input.code)
                .output(),
        )
        .await
        .map_err(|_| CodeExecutorError::Timeout(self.timeout_secs))?
        .map_err(|e| CodeExecutorError::ExecutionFailed(format!("Local execution failed: {e}")))?;

        Ok(CodeExecutionResult {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            output_files: vec![],
        })
    }

    fn stateful(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_timeout() {
        let exec = UnsafeLocalCodeExecutor::new();
        assert_eq!(exec.timeout_secs, 30);
    }

    #[test]
    fn custom_timeout() {
        let exec = UnsafeLocalCodeExecutor::new().with_timeout(60);
        assert_eq!(exec.timeout_secs, 60);
    }

    #[test]
    fn not_stateful() {
        let exec = UnsafeLocalCodeExecutor::new();
        assert!(!exec.stateful());
    }
}
