pub mod types;
pub mod base;

pub use types::{CodeExecutionInput, CodeExecutionResult, CodeFile};
pub use base::{CodeExecutor, CodeExecutorError};

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    // --- CodeFile tests ---

    #[test]
    fn code_file_construction() {
        let file = CodeFile {
            name: "main.py".into(),
            content: "print('hello')".into(),
            mime_type: "text/x-python".into(),
        };
        assert_eq!(file.name, "main.py");
        assert_eq!(file.content, "print('hello')");
        assert_eq!(file.mime_type, "text/x-python");
    }

    #[test]
    fn code_file_serde_roundtrip() {
        let file = CodeFile {
            name: "data.csv".into(),
            content: "a,b\n1,2".into(),
            mime_type: "text/csv".into(),
        };
        let json = serde_json::to_string(&file).unwrap();
        let deserialized: CodeFile = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, file.name);
        assert_eq!(deserialized.content, file.content);
        assert_eq!(deserialized.mime_type, file.mime_type);
    }

    // --- CodeExecutionResult tests ---

    #[test]
    fn code_execution_result_empty() {
        let result = CodeExecutionResult::empty();
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
        assert!(result.output_files.is_empty());
    }

    // --- CodeExecutionInput tests ---

    #[test]
    fn code_execution_input_construction() {
        let input = CodeExecutionInput {
            code: "x = 1 + 2".into(),
            input_files: vec![CodeFile {
                name: "input.txt".into(),
                content: "hello".into(),
                mime_type: "text/plain".into(),
            }],
            execution_id: Some("exec-123".into()),
        };
        assert_eq!(input.code, "x = 1 + 2");
        assert_eq!(input.input_files.len(), 1);
        assert_eq!(input.execution_id.as_deref(), Some("exec-123"));
    }

    #[test]
    fn code_execution_input_no_execution_id() {
        let input = CodeExecutionInput {
            code: "pass".into(),
            input_files: Vec::new(),
            execution_id: None,
        };
        assert!(input.execution_id.is_none());
        assert!(input.input_files.is_empty());
    }

    // --- CodeExecutor trait default methods ---

    /// A minimal executor to test default trait method implementations.
    struct StubExecutor;

    #[async_trait]
    impl CodeExecutor for StubExecutor {
        async fn execute_code(
            &self,
            _input: CodeExecutionInput,
        ) -> Result<CodeExecutionResult, CodeExecutorError> {
            Ok(CodeExecutionResult::empty())
        }
    }

    #[test]
    fn default_code_block_delimiters() {
        let exec = StubExecutor;
        let delims = exec.code_block_delimiters();
        assert_eq!(delims.len(), 2);
        assert_eq!(delims[0], ("```tool_code\n".to_string(), "\n```".to_string()));
        assert_eq!(delims[1], ("```python\n".to_string(), "\n```".to_string()));
    }

    #[test]
    fn default_execution_result_delimiters() {
        let exec = StubExecutor;
        let (open, close) = exec.execution_result_delimiters();
        assert_eq!(open, "```tool_output\n");
        assert_eq!(close, "\n```");
    }

    #[test]
    fn default_error_retry_attempts() {
        let exec = StubExecutor;
        assert_eq!(exec.error_retry_attempts(), 2);
    }

    #[test]
    fn default_stateful() {
        let exec = StubExecutor;
        assert!(!exec.stateful());
    }

    #[tokio::test]
    async fn stub_executor_execute_code() {
        let exec = StubExecutor;
        let input = CodeExecutionInput {
            code: "1 + 1".into(),
            input_files: Vec::new(),
            execution_id: None,
        };
        let result = exec.execute_code(input).await.unwrap();
        assert!(result.stdout.is_empty());
    }

    // --- Object safety ---

    fn _assert_object_safe(_: &dyn CodeExecutor) {}

    #[test]
    fn code_executor_is_object_safe() {
        let exec = StubExecutor;
        _assert_object_safe(&exec);
    }
}
