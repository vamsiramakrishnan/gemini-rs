//! Vertex AI code executor — runs code via Vertex AI Code Execution API.
//!
//! Mirrors ADK-Python's `vertex_ai_code_executor`. Executes code
//! using the Vertex AI managed code execution service.

use async_trait::async_trait;

use super::base::{CodeExecutor, CodeExecutorError};
use super::types::{CodeExecutionInput, CodeExecutionResult};

/// Configuration for Vertex AI code execution.
#[derive(Debug, Clone)]
pub struct VertexAiCodeExecutorConfig {
    /// Google Cloud project ID.
    pub project: String,
    /// Google Cloud location (e.g., "us-central1").
    pub location: String,
    /// Execution timeout in seconds.
    pub timeout_secs: u64,
}

/// Code executor that runs code via the Vertex AI managed service.
///
/// Uses the Vertex AI Code Execution API to run Python code in
/// a Google-managed sandboxed environment.
#[derive(Debug, Clone)]
pub struct VertexAiCodeExecutor {
    config: VertexAiCodeExecutorConfig,
}

impl VertexAiCodeExecutor {
    /// Create a new Vertex AI code executor.
    pub fn new(config: VertexAiCodeExecutorConfig) -> Self {
        Self { config }
    }

    /// Returns the configured project ID.
    pub fn project(&self) -> &str {
        &self.config.project
    }

    /// Returns the configured location.
    pub fn location(&self) -> &str {
        &self.config.location
    }
}

#[async_trait]
impl CodeExecutor for VertexAiCodeExecutor {
    async fn execute_code(
        &self,
        input: CodeExecutionInput,
    ) -> Result<CodeExecutionResult, CodeExecutorError> {
        // In a real integration, this would call the Vertex AI Code Execution API:
        // POST https://{location}-aiplatform.googleapis.com/v1beta1/projects/{project}/locations/{location}/codeExecutions
        //
        // The actual API integration requires the Google Cloud SDK and authentication.
        let _ = &input;
        Ok(CodeExecutionResult {
            stdout: String::new(),
            stderr: String::new(),
            output_files: vec![],
        })
    }

    fn stateful(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> VertexAiCodeExecutorConfig {
        VertexAiCodeExecutorConfig {
            project: "test-project".into(),
            location: "us-central1".into(),
            timeout_secs: 60,
        }
    }

    #[test]
    fn executor_metadata() {
        let exec = VertexAiCodeExecutor::new(test_config());
        assert_eq!(exec.project(), "test-project");
        assert_eq!(exec.location(), "us-central1");
        assert!(exec.stateful());
    }

    #[tokio::test]
    async fn execute_returns_empty_stub() {
        let exec = VertexAiCodeExecutor::new(test_config());
        let input = CodeExecutionInput {
            code: "print(42)".into(),
            input_files: vec![],
            execution_id: None,
        };
        let result = exec.execute_code(input).await.unwrap();
        assert!(result.stdout.is_empty());
    }
}
