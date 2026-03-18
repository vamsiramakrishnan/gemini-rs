//! TextAgentTool — wraps a TextAgent as a ToolFunction for voice orchestration.
//!
//! When the live model calls this tool, the wrapped TextAgent runs via
//! `BaseLlm::generate()` (request/response), not over a WebSocket. The agent's
//! text output is returned as the tool result. State is shared with the parent
//! session, so mutations are visible to watchers and phase transitions.
//!
//! This bridges live↔text: the voice model dispatches complex multi-step
//! reasoning to specialist text agent pipelines.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::error::ToolError;
use crate::state::State;
use crate::text::TextAgent;
use crate::tool::ToolFunction;

/// Wraps a [`TextAgent`] as a [`ToolFunction`] for live session tool dispatch.
///
/// Unlike [`AgentTool`](crate::AgentTool) (which wraps a live `Agent`),
/// `TextAgentTool` wraps a text-mode agent that uses `BaseLlm::generate()`.
/// This enables multi-step LLM reasoning pipelines to be invoked as tools
/// from a voice session.
///
/// # State Sharing
///
/// The text agent operates on the **same shared `State`** as the voice session.
/// This means:
/// - The agent can read live-extracted values (emotional_state, risk_level)
/// - Agent state mutations are visible to watchers and phase transitions
/// - No explicit "promote state" step is needed
///
/// # Example
///
/// ```ignore
/// let verifier = LlmTextAgent::new("verifier", flash)
///     .instruction("Cross-reference identity against account record")
///     .tools(Arc::new(db_dispatcher));
///
/// let tool = TextAgentTool::new("verify_identity", "Verify caller identity", verifier, state);
/// dispatcher.register(tool);
/// ```
pub struct TextAgentTool {
    name: String,
    description: String,
    agent: Arc<dyn TextAgent>,
    parameters: serde_json::Value,
    state: State,
}

impl TextAgentTool {
    /// Create a new TextAgentTool wrapping the given text agent.
    ///
    /// `state` should be the session's shared State so mutations flow
    /// bidirectionally between the voice session and the text agent.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        agent: impl TextAgent + 'static,
        state: State,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            agent: Arc::new(agent),
            parameters: json!({
                "type": "object",
                "properties": {
                    "request": {
                        "type": "string",
                        "description": "The request to process"
                    }
                },
                "required": ["request"]
            }),
            state,
        }
    }

    /// Create from an already-Arc'd text agent.
    pub fn from_arc(
        name: impl Into<String>,
        description: impl Into<String>,
        agent: Arc<dyn TextAgent>,
        state: State,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            agent,
            parameters: json!({
                "type": "object",
                "properties": {
                    "request": {
                        "type": "string",
                        "description": "The request to process"
                    }
                },
                "required": ["request"]
            }),
            state,
        }
    }

    /// Override the tool parameters schema.
    pub fn with_parameters(mut self, params: serde_json::Value) -> Self {
        self.parameters = params;
        self
    }
}

#[async_trait]
impl ToolFunction for TextAgentTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        Some(self.parameters.clone())
    }

    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        // 1. Inject tool call args into state
        if let Some(request) = args.get("request").and_then(|r| r.as_str()) {
            self.state.set("input", request);
        }
        self.state.set("agent_tool_args", &args);

        // 2. Run the text agent pipeline
        let result = self
            .agent
            .run(&self.state)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("{e}")))?;

        // 3. Return result as tool response
        Ok(json!({"result": result}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AgentError;

    /// Echo agent: reads "input" from state, returns it prefixed.
    struct EchoTextAgent;

    #[async_trait]
    impl TextAgent for EchoTextAgent {
        fn name(&self) -> &str {
            "echo"
        }
        async fn run(&self, state: &State) -> Result<String, AgentError> {
            let input = state
                .get::<String>("input")
                .unwrap_or_else(|| "no input".into());
            Ok(format!("Echo: {input}"))
        }
    }

    /// Agent that reads and writes state.
    struct StatefulAgent;

    #[async_trait]
    impl TextAgent for StatefulAgent {
        fn name(&self) -> &str {
            "stateful"
        }
        async fn run(&self, state: &State) -> Result<String, AgentError> {
            // Read a value set by the parent
            let parent_val = state
                .get::<String>("parent_key")
                .unwrap_or_else(|| "missing".into());

            // Write a value visible to the parent
            state.set("child_wrote", true);
            state.set("child_output", "from child agent");

            Ok(format!("Parent said: {parent_val}"))
        }
    }

    /// Agent that always fails.
    struct FailingTextAgent;

    #[async_trait]
    impl TextAgent for FailingTextAgent {
        fn name(&self) -> &str {
            "failing"
        }
        async fn run(&self, _state: &State) -> Result<String, AgentError> {
            Err(AgentError::Other("intentional failure".into()))
        }
    }

    #[tokio::test]
    async fn basic_dispatch() {
        let state = State::new();
        let tool = TextAgentTool::new("echo", "Echo tool", EchoTextAgent, state);

        let result = tool.call(json!({"request": "hello"})).await.unwrap();
        assert_eq!(result["result"], "Echo: hello");
    }

    #[tokio::test]
    async fn tool_metadata() {
        let state = State::new();
        let tool = TextAgentTool::new("my_tool", "Does things", EchoTextAgent, state);

        assert_eq!(tool.name(), "my_tool");
        assert_eq!(tool.description(), "Does things");
        assert!(tool.parameters().is_some());
        let params = tool.parameters().unwrap();
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["request"].is_object());
    }

    #[tokio::test]
    async fn state_shared_bidirectionally() {
        let state = State::new();
        state.set("parent_key", "hello from parent");

        let tool = TextAgentTool::new("stateful", "Stateful tool", StatefulAgent, state.clone());

        let result = tool.call(json!({"request": "test"})).await.unwrap();
        assert_eq!(result["result"], "Parent said: hello from parent");

        // Verify child's state mutations are visible to parent
        assert_eq!(state.get::<bool>("child_wrote"), Some(true));
        assert_eq!(
            state.get::<String>("child_output"),
            Some("from child agent".into())
        );
    }

    #[tokio::test]
    async fn error_propagation() {
        let state = State::new();
        let tool = TextAgentTool::new("failing", "Fails", FailingTextAgent, state);

        let result = tool.call(json!({"request": "test"})).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::ExecutionFailed(msg) => {
                assert!(msg.contains("intentional failure"));
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn custom_parameters() {
        let state = State::new();
        let params = json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "limit": { "type": "integer" }
            }
        });
        let tool = TextAgentTool::new("custom", "Custom params", EchoTextAgent, state)
            .with_parameters(params.clone());

        assert_eq!(tool.parameters().unwrap(), params);
    }

    #[tokio::test]
    async fn args_injected_into_state() {
        let state = State::new();
        let tool = TextAgentTool::new("echo", "Echo", EchoTextAgent, state.clone());

        let _ = tool.call(json!({"request": "injected"})).await.unwrap();

        // Verify args were injected
        assert_eq!(state.get::<String>("input"), Some("injected".into()));
        let args = state.get::<serde_json::Value>("agent_tool_args").unwrap();
        assert_eq!(args["request"], "injected");
    }

    #[tokio::test]
    async fn from_arc_constructor() {
        let state = State::new();
        let agent: Arc<dyn TextAgent> = Arc::new(EchoTextAgent);
        let tool = TextAgentTool::from_arc("echo", "Echo tool", agent, state);

        let result = tool.call(json!({"request": "arc test"})).await.unwrap();
        assert_eq!(result["result"], "Echo: arc test");
    }
}
