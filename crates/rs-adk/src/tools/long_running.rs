//! Long-running function tool wrapper.
//!
//! Wraps any [`ToolFunction`] and marks it as long-running by appending an
//! instruction to the tool description that tells the LLM not to re-invoke
//! the tool while it is still pending.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::ToolError;
use crate::tool::ToolFunction;

/// Instruction appended to the tool description for long-running tools.
const LONG_RUNNING_INSTRUCTION: &str = "NOTE: This is a long-running operation. \
    Do not call this tool again if it has already returned some intermediate or pending status.";

/// Wraps a [`ToolFunction`] and marks it as long-running.
///
/// The wrapper appends [`LONG_RUNNING_INSTRUCTION`] to the inner tool's
/// description so the LLM knows not to re-invoke it while a previous call
/// is still in progress. All other trait methods delegate directly to the
/// inner tool.
pub struct LongRunningFunctionTool {
    inner: Arc<dyn ToolFunction>,
    /// Cached description: inner description + "\n" + LONG_RUNNING_INSTRUCTION.
    augmented_description: String,
}

impl LongRunningFunctionTool {
    /// Create a new `LongRunningFunctionTool` wrapping the given inner tool.
    pub fn new(inner: Arc<dyn ToolFunction>) -> Self {
        let augmented_description =
            format!("{}\n{}", inner.description(), LONG_RUNNING_INSTRUCTION);
        Self {
            inner,
            augmented_description,
        }
    }

    /// Returns `true` — this tool is always considered long-running.
    pub fn is_long_running(&self) -> bool {
        true
    }
}

#[async_trait]
impl ToolFunction for LongRunningFunctionTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        &self.augmented_description
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        self.inner.parameters()
    }

    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        self.inner.call(args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A minimal mock tool for testing delegation.
    struct MockInnerTool;

    #[async_trait]
    impl ToolFunction for MockInnerTool {
        fn name(&self) -> &str {
            "slow_operation"
        }
        fn description(&self) -> &str {
            "Performs a slow operation"
        }
        fn parameters(&self) -> Option<serde_json::Value> {
            Some(json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" }
                }
            }))
        }
        async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
            let task_id = args
                .get("task_id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            Ok(json!({ "status": "completed", "task_id": task_id }))
        }
    }

    #[test]
    fn description_is_augmented() {
        let inner = Arc::new(MockInnerTool);
        let tool = LongRunningFunctionTool::new(inner);

        let desc = tool.description();
        assert!(
            desc.starts_with("Performs a slow operation"),
            "should start with the inner description, got: {desc}"
        );
        assert!(
            desc.contains(LONG_RUNNING_INSTRUCTION),
            "should contain the long-running instruction, got: {desc}"
        );
        assert!(
            desc.contains('\n'),
            "inner description and instruction should be separated by a newline"
        );
    }

    #[test]
    fn name_delegates_to_inner() {
        let inner = Arc::new(MockInnerTool);
        let tool = LongRunningFunctionTool::new(inner);
        assert_eq!(tool.name(), "slow_operation");
    }

    #[test]
    fn parameters_delegates_to_inner() {
        let inner = Arc::new(MockInnerTool);
        let tool = LongRunningFunctionTool::new(inner);

        let params = tool.parameters().expect("should have parameters");
        assert!(params["properties"]["task_id"].is_object());
    }

    #[tokio::test]
    async fn call_delegates_to_inner() {
        let inner = Arc::new(MockInnerTool);
        let tool = LongRunningFunctionTool::new(inner);

        let result = tool
            .call(json!({ "task_id": "abc-123" }))
            .await
            .expect("call should succeed");
        assert_eq!(result["status"], "completed");
        assert_eq!(result["task_id"], "abc-123");
    }

    #[test]
    fn is_long_running_returns_true() {
        let inner = Arc::new(MockInnerTool);
        let tool = LongRunningFunctionTool::new(inner);
        assert!(tool.is_long_running());
    }

    #[test]
    fn description_format_is_correct() {
        let inner = Arc::new(MockInnerTool);
        let tool = LongRunningFunctionTool::new(inner);

        let expected = format!(
            "Performs a slow operation\n{}",
            LONG_RUNNING_INSTRUCTION
        );
        assert_eq!(tool.description(), expected);
    }
}
