//! Code execution infrastructure — sandboxed code execution for agents.

/// Base trait and error types for code execution.
pub mod base;
/// Built-in code executor using Gemini's native code execution.
pub mod built_in;
/// Container-based code executor using Docker.
pub mod container;
/// Types used by code executors (input, output, files).
pub mod types;
/// Unsafe local code executor (no sandboxing).
pub mod unsafe_local;
/// Utility functions for extracting code blocks and building parts.
pub mod utils;
/// Vertex AI managed code executor.
pub mod vertex_ai;

pub use base::{CodeExecutor, CodeExecutorError};
pub use built_in::BuiltInCodeExecutor;
pub use container::{ContainerCodeExecutor, ContainerCodeExecutorConfig};
pub use types::{CodeExecutionInput, CodeExecutionResult, CodeFile};
pub use unsafe_local::UnsafeLocalCodeExecutor;
pub use vertex_ai::{VertexAiCodeExecutor, VertexAiCodeExecutorConfig};

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
        assert_eq!(
            delims[0],
            ("```tool_code\n".to_string(), "\n```".to_string())
        );
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

    // --- BuiltInCodeExecutor tests ---

    #[test]
    fn built_in_process_llm_request_adds_code_execution_for_gemini2() {
        let executor = built_in::BuiltInCodeExecutor;
        let mut request = crate::llm::LlmRequest::from_text("hello");
        executor
            .process_llm_request(&mut request, "gemini-2.5-flash")
            .unwrap();
        assert_eq!(request.tools.len(), 1);
        assert!(request.tools[0].code_execution.is_some());
    }

    #[test]
    fn built_in_process_llm_request_rejects_non_gemini2() {
        let executor = built_in::BuiltInCodeExecutor;
        let mut request = crate::llm::LlmRequest::from_text("hello");
        let err = executor
            .process_llm_request(&mut request, "gemini-1.5-pro")
            .unwrap_err();
        assert!(
            err.to_string().contains("Gemini 2.0+"),
            "expected UnsupportedModel error, got: {}",
            err
        );
        assert!(request.tools.is_empty());
    }

    #[tokio::test]
    async fn built_in_execute_code_returns_empty() {
        let executor = built_in::BuiltInCodeExecutor;
        let input = CodeExecutionInput {
            code: "print('hi')".into(),
            input_files: Vec::new(),
            execution_id: None,
        };
        let result = executor.execute_code(input).await.unwrap();
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
        assert!(result.output_files.is_empty());
    }

    // --- utils tests ---

    #[test]
    fn extract_code_from_tool_code_block() {
        let text = "Some text\n```tool_code\nprint('hello')\n```\nMore text";
        let delimiters = vec![
            ("```tool_code\n".to_string(), "\n```".to_string()),
            ("```python\n".to_string(), "\n```".to_string()),
        ];
        let (code, remaining) = utils::extract_code_from_text(text, &delimiters).unwrap();
        assert_eq!(code, "print('hello')");
        assert_eq!(remaining, "Some text\n\nMore text");
    }

    #[test]
    fn extract_code_from_python_block() {
        let text = "Intro\n```python\nx = 42\n```\nDone";
        let delimiters = vec![
            ("```tool_code\n".to_string(), "\n```".to_string()),
            ("```python\n".to_string(), "\n```".to_string()),
        ];
        let (code, remaining) = utils::extract_code_from_text(text, &delimiters).unwrap();
        assert_eq!(code, "x = 42");
        assert_eq!(remaining, "Intro\n\nDone");
    }

    #[test]
    fn extract_code_returns_none_when_no_block() {
        let text = "Just plain text, no code blocks here.";
        let delimiters = vec![
            ("```tool_code\n".to_string(), "\n```".to_string()),
            ("```python\n".to_string(), "\n```".to_string()),
        ];
        assert!(utils::extract_code_from_text(text, &delimiters).is_none());
    }

    #[test]
    fn build_executable_code_part_correct() {
        let part = utils::build_executable_code_part("x = 1");
        match part {
            gemini_live::prelude::Part::ExecutableCode { executable_code } => {
                assert_eq!(executable_code.language, "PYTHON");
                assert_eq!(executable_code.code, "x = 1");
            }
            other => panic!("expected ExecutableCode, got: {:?}", other),
        }
    }

    #[test]
    fn build_code_execution_result_part_ok_outcome() {
        let part = utils::build_code_execution_result_part("42\n", "");
        match part {
            gemini_live::prelude::Part::CodeExecutionResult {
                code_execution_result,
            } => {
                assert_eq!(code_execution_result.outcome, "OK");
                assert_eq!(code_execution_result.output.as_deref(), Some("42\n"));
            }
            other => panic!("expected CodeExecutionResult, got: {:?}", other),
        }
    }

    #[test]
    fn build_code_execution_result_part_failed_outcome() {
        let part =
            utils::build_code_execution_result_part("partial output", "NameError: x not defined");
        match part {
            gemini_live::prelude::Part::CodeExecutionResult {
                code_execution_result,
            } => {
                assert_eq!(code_execution_result.outcome, "FAILED");
                let output = code_execution_result.output.unwrap();
                assert!(output.contains("partial output"));
                assert!(output.contains("NameError: x not defined"));
            }
            other => panic!("expected CodeExecutionResult, got: {:?}", other),
        }
    }
}
