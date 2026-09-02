//! Callback types for tool execution lifecycle.
//!
//! Callbacks provide a lightweight alternative to plugins for simple
//! before/after tool interception. They are closures registered on the
//! agent builder.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use gemini_genai_rs::prelude::FunctionCall;

use crate::error::ToolError;

/// The result of a before-tool callback.
#[derive(Debug, Clone)]
pub enum BeforeToolResult {
    /// Continue with the tool call.
    Continue,
    /// Skip the tool call and use this value as the result.
    Skip(serde_json::Value),
    /// Deny the tool call with a reason.
    Deny(String),
}

/// The result of a tool call, passed to after-tool callbacks.
#[derive(Debug, Clone)]
pub struct ToolCallResult {
    /// The function call that was executed.
    pub call: FunctionCall,
    /// The result (Ok = tool output, Err = tool error).
    pub result: Result<serde_json::Value, ToolError>,
    /// How long the tool call took.
    pub duration: std::time::Duration,
}

/// A before-tool callback function type.
///
/// Receives the function call about to be executed and returns a decision
/// about whether to proceed.
pub type BeforeToolCallback = Arc<
    dyn Fn(&FunctionCall) -> Pin<Box<dyn Future<Output = BeforeToolResult> + Send + '_>>
        + Send
        + Sync,
>;

/// An after-tool callback function type.
///
/// Receives the tool call result for observation/logging purposes.
/// Cannot modify the result.
pub type AfterToolCallback =
    Arc<dyn Fn(&ToolCallResult) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn before_tool_result_variants() {
        let cont = BeforeToolResult::Continue;
        assert!(matches!(cont, BeforeToolResult::Continue));

        let skip = BeforeToolResult::Skip(serde_json::json!({"cached": true}));
        assert!(matches!(skip, BeforeToolResult::Skip(_)));

        let deny = BeforeToolResult::Deny("not allowed".into());
        assert!(matches!(deny, BeforeToolResult::Deny(_)));
    }

    #[test]
    fn tool_call_result_ok() {
        let result = ToolCallResult {
            call: FunctionCall {
                name: "test".into(),
                args: serde_json::json!({}),
                id: None,
            },
            result: Ok(serde_json::json!({"success": true})),
            duration: std::time::Duration::from_millis(42),
        };
        assert!(result.result.is_ok());
    }

    #[test]
    fn tool_call_result_err() {
        let result = ToolCallResult {
            call: FunctionCall {
                name: "test".into(),
                args: serde_json::json!({}),
                id: None,
            },
            result: Err(ToolError::ExecutionFailed("boom".into())),
            duration: std::time::Duration::from_millis(1),
        };
        assert!(result.result.is_err());
    }
}
