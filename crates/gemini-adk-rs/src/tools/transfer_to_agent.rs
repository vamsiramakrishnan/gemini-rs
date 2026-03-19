//! Transfer-to-agent tool — enables agent handoff in multi-agent systems.
//!
//! Mirrors ADK-Python's `TransferToAgentTool`. Provides enum-constrained
//! agent names to prevent LLM hallucination of invalid agent names.

use async_trait::async_trait;

use crate::error::ToolError;
use crate::tool::ToolFunction;

/// Tool that transfers control to another agent.
///
/// The `agent_name` parameter is constrained to a set of valid agent names
/// via JSON Schema enum, preventing the model from hallucinating invalid names.
#[derive(Debug, Clone)]
pub struct TransferToAgentTool {
    /// Valid agent names that can be transferred to.
    agent_names: Vec<String>,
}

impl TransferToAgentTool {
    /// Create a new transfer tool with the given valid agent names.
    pub fn new(agent_names: Vec<String>) -> Self {
        Self { agent_names }
    }

    /// Returns the list of valid agent names.
    pub fn agent_names(&self) -> &[String] {
        &self.agent_names
    }
}

#[async_trait]
impl ToolFunction for TransferToAgentTool {
    fn name(&self) -> &str {
        "transfer_to_agent"
    }

    fn description(&self) -> &str {
        "Transfer the question to another agent. Use this tool to hand off control \
         to a more suitable agent based on their description."
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "agent_name": {
                    "type": "string",
                    "description": "The name of the agent to transfer to.",
                    "enum": self.agent_names
                }
            },
            "required": ["agent_name"]
        }))
    }

    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let agent_name = args
            .get("agent_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("Missing agent_name".into()))?;

        if !self.agent_names.iter().any(|n| n == agent_name) {
            return Err(ToolError::InvalidArgs(format!(
                "Invalid agent name '{}'. Valid agents: {:?}",
                agent_name, self.agent_names
            )));
        }

        // The actual transfer is handled by the runtime checking
        // the tool response for the transfer signal.
        Ok(serde_json::json!({
            "status": "transferred",
            "agent_name": agent_name
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parameters_include_enum() {
        let tool = TransferToAgentTool::new(vec!["agent_a".into(), "agent_b".into()]);
        let params = tool.parameters().unwrap();
        let enums = params["properties"]["agent_name"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(enums.len(), 2);
        assert_eq!(enums[0], "agent_a");
        assert_eq!(enums[1], "agent_b");
    }

    #[tokio::test]
    async fn valid_transfer() {
        let tool = TransferToAgentTool::new(vec!["support".into(), "billing".into()]);
        let result = tool.call(json!({"agent_name": "support"})).await.unwrap();
        assert_eq!(result["status"], "transferred");
        assert_eq!(result["agent_name"], "support");
    }

    #[tokio::test]
    async fn invalid_agent_name() {
        let tool = TransferToAgentTool::new(vec!["support".into()]);
        let result = tool.call(json!({"agent_name": "hacker"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn missing_agent_name() {
        let tool = TransferToAgentTool::new(vec!["support".into()]);
        let result = tool.call(json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn agent_names_accessor() {
        let tool = TransferToAgentTool::new(vec!["a".into(), "b".into()]);
        assert_eq!(tool.agent_names(), &["a", "b"]);
    }
}
