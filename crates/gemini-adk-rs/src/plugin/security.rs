//! Security plugin — policy-based tool call authorization.

use async_trait::async_trait;

use gemini_genai_rs::prelude::FunctionCall;

use super::{Plugin, PluginResult};
use crate::context::InvocationContext;

/// The outcome of a policy evaluation.
#[derive(Debug, Clone)]
pub enum PolicyOutcome {
    /// Allow the tool call to proceed.
    Allow,
    /// Require user confirmation before proceeding.
    Confirm(String),
    /// Deny the tool call with a reason.
    Deny(String),
}

/// Trait for evaluating tool call policies.
///
/// Implementations can check tool names, arguments, user permissions,
/// rate limits, etc.
pub trait PolicyEngine: Send + Sync + 'static {
    /// Evaluate whether a tool call should be allowed.
    fn evaluate(&self, tool_name: &str, args: &serde_json::Value) -> PolicyOutcome;
}

/// Plugin that enforces tool call policies via a `PolicyEngine`.
///
/// Before every tool call, the security plugin consults the policy engine.
/// If the engine returns `Deny`, the tool call is blocked. If it returns
/// `Confirm`, the tool call is blocked with a confirmation message (in a
/// real system, this would prompt the user).
pub struct SecurityPlugin {
    engine: Box<dyn PolicyEngine>,
}

impl SecurityPlugin {
    /// Create a new security plugin with the given policy engine.
    pub fn new(engine: impl PolicyEngine + 'static) -> Self {
        Self {
            engine: Box::new(engine),
        }
    }
}

#[async_trait]
impl Plugin for SecurityPlugin {
    fn name(&self) -> &str {
        "security"
    }

    async fn before_tool(&self, call: &FunctionCall, _ctx: &InvocationContext) -> PluginResult {
        match self.engine.evaluate(&call.name, &call.args) {
            PolicyOutcome::Allow => {
                #[cfg(feature = "tracing-support")]
                tracing::debug!(tool = %call.name, "[plugin:security] Tool call allowed");
                PluginResult::Continue
            }
            PolicyOutcome::Confirm(msg) => {
                #[cfg(feature = "tracing-support")]
                tracing::warn!(tool = %call.name, reason = %msg, "[plugin:security] Tool call requires confirmation");
                PluginResult::Deny(format!("Confirmation required: {}", msg))
            }
            PolicyOutcome::Deny(reason) => {
                #[cfg(feature = "tracing-support")]
                tracing::warn!(tool = %call.name, reason = %reason, "[plugin:security] Tool call denied");
                PluginResult::Deny(reason)
            }
        }
    }
}

/// A simple policy engine that blocks specific tool names.
pub struct DenyListPolicy {
    blocked_tools: Vec<String>,
}

impl DenyListPolicy {
    /// Create a policy that denies specific tools by name.
    pub fn new(blocked_tools: Vec<String>) -> Self {
        Self { blocked_tools }
    }
}

impl PolicyEngine for DenyListPolicy {
    fn evaluate(&self, tool_name: &str, _args: &serde_json::Value) -> PolicyOutcome {
        if self.blocked_tools.iter().any(|t| t == tool_name) {
            PolicyOutcome::Deny(format!("Tool '{}' is blocked by policy", tool_name))
        } else {
            PolicyOutcome::Allow
        }
    }
}

/// A policy engine that allows all tool calls.
pub struct AllowAllPolicy;

impl PolicyEngine for AllowAllPolicy {
    fn evaluate(&self, _tool_name: &str, _args: &serde_json::Value) -> PolicyOutcome {
        PolicyOutcome::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_list_policy_blocks() {
        let policy = DenyListPolicy::new(vec!["dangerous_tool".into()]);
        let result = policy.evaluate("dangerous_tool", &serde_json::json!({}));
        assert!(matches!(result, PolicyOutcome::Deny(_)));
    }

    #[test]
    fn deny_list_policy_allows() {
        let policy = DenyListPolicy::new(vec!["dangerous_tool".into()]);
        let result = policy.evaluate("safe_tool", &serde_json::json!({}));
        assert!(matches!(result, PolicyOutcome::Allow));
    }

    #[test]
    fn allow_all_policy() {
        let policy = AllowAllPolicy;
        let result = policy.evaluate("anything", &serde_json::json!({}));
        assert!(matches!(result, PolicyOutcome::Allow));
    }

    #[tokio::test]
    async fn security_plugin_denies_blocked_tool() {
        use tokio::sync::broadcast;

        let policy = DenyListPolicy::new(vec!["rm_rf".into()]);
        let plugin = SecurityPlugin::new(policy);

        let (evt_tx, _) = broadcast::channel(16);
        let writer: std::sync::Arc<dyn gemini_genai_rs::session::SessionWriter> =
            std::sync::Arc::new(crate::test_helpers::MockWriter);
        let session = crate::agent_session::AgentSession::from_writer(writer, evt_tx);
        let ctx = InvocationContext::new(session);

        let call = FunctionCall {
            name: "rm_rf".into(),
            args: serde_json::json!({}),
            id: None,
        };

        let result = plugin.before_tool(&call, &ctx).await;
        assert!(result.is_deny());
    }

    #[tokio::test]
    async fn security_plugin_allows_safe_tool() {
        use tokio::sync::broadcast;

        let policy = DenyListPolicy::new(vec!["rm_rf".into()]);
        let plugin = SecurityPlugin::new(policy);

        let (evt_tx, _) = broadcast::channel(16);
        let writer: std::sync::Arc<dyn gemini_genai_rs::session::SessionWriter> =
            std::sync::Arc::new(crate::test_helpers::MockWriter);
        let session = crate::agent_session::AgentSession::from_writer(writer, evt_tx);
        let ctx = InvocationContext::new(session);

        let call = FunctionCall {
            name: "get_weather".into(),
            args: serde_json::json!({}),
            id: None,
        };

        let result = plugin.before_tool(&call, &ctx).await;
        assert!(result.is_continue());
    }
}
