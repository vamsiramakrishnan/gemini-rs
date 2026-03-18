//! Get user choice tool — presents options to the user for selection.
//!
//! Mirrors ADK-Python's `get_user_choice_tool`. Wraps around the
//! long-running tool mechanism to pause execution until the user responds.

use async_trait::async_trait;

use crate::error::ToolError;
use crate::tool::ToolFunction;

/// Tool that presents a list of options to the user and waits for selection.
///
/// This is a long-running tool — the model calls it to present choices,
/// and execution pauses until the user makes a selection.
#[derive(Debug, Clone, Default)]
pub struct GetUserChoiceTool;

impl GetUserChoiceTool {
    /// Create a new get user choice tool.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolFunction for GetUserChoiceTool {
    fn name(&self) -> &str {
        "get_user_choice"
    }

    fn description(&self) -> &str {
        "Provides a list of options to the user and asks them to choose one. \
         Use this when you need user input to decide between multiple options."
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "The list of options to present to the user."
                }
            },
            "required": ["options"]
        }))
    }

    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let options = args
            .get("options")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ToolError::InvalidArgs("Missing options array".into()))?;

        // In a real integration, the runtime would intercept this and
        // present the options to the user via the UI.
        Ok(serde_json::json!({
            "status": "awaiting_user_choice",
            "options": options,
            "message": "Waiting for user to select an option."
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_metadata() {
        let tool = GetUserChoiceTool::new();
        assert_eq!(tool.name(), "get_user_choice");
        assert!(tool.parameters().is_some());
    }

    #[tokio::test]
    async fn call_with_options() {
        let tool = GetUserChoiceTool::new();
        let result = tool
            .call(json!({"options": ["Option A", "Option B", "Option C"]}))
            .await
            .unwrap();
        assert_eq!(result["status"], "awaiting_user_choice");
        assert_eq!(result["options"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn missing_options_error() {
        let tool = GetUserChoiceTool::new();
        let result = tool.call(json!({})).await;
        assert!(result.is_err());
    }
}
