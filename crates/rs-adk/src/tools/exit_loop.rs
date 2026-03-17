//! Exit loop tool — allows agents to break out of loop execution.
//!
//! Mirrors ADK-Python's `exit_loop` tool. When called by the model,
//! signals the loop agent to terminate by setting the escalate action.

use async_trait::async_trait;

use crate::error::ToolError;
use crate::tool::ToolFunction;

/// Tool that signals a loop agent to exit its loop.
///
/// When the model calls this tool, the loop agent's escalate flag is set,
/// causing it to break out of the current iteration.
#[derive(Debug, Clone, Default)]
pub struct ExitLoopTool;

impl ExitLoopTool {
    /// Create a new exit loop tool.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolFunction for ExitLoopTool {
    fn name(&self) -> &str {
        "exit_loop"
    }

    fn description(&self) -> &str {
        "Exits the current loop. Call this function only when you are instructed to do so."
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        None
    }

    async fn call(&self, _args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        // The actual loop exit is handled by the runtime checking
        // the tool name. This response confirms the exit.
        Ok(serde_json::json!({
            "status": "loop_exited",
            "message": "Loop has been exited successfully."
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_and_description() {
        let tool = ExitLoopTool::new();
        assert_eq!(tool.name(), "exit_loop");
        assert!(tool.description().contains("Exit"));
    }

    #[test]
    fn no_parameters() {
        let tool = ExitLoopTool::new();
        assert!(tool.parameters().is_none());
    }

    #[tokio::test]
    async fn call_returns_success() {
        let tool = ExitLoopTool::new();
        let result = tool.call(serde_json::json!({})).await.unwrap();
        assert_eq!(result["status"], "loop_exited");
    }
}
