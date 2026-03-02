//! AgentTool — wraps an Agent as a ToolFunction for "agent as a tool" dispatch.
//!
//! When the live model calls this tool, the wrapped agent runs in an isolated
//! context (no live WebSocket). The agent's text output is collected and returned
//! as the tool result. State changes propagate back to the parent context.
//!
//! This bridges live<->non-live: the wrapped agent can use regular Gemini API,
//! external services, or pure computation — it doesn't need a WebSocket.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::broadcast;

use rs_genai::session::SessionEvent;

use crate::agent::Agent;
use crate::agent_session::{AgentSession, NoOpSessionWriter};
use crate::context::{AgentEvent, InvocationContext};
use crate::error::ToolError;
use crate::tool::ToolFunction;

/// Wraps an Agent as a ToolFunction for "agent as a tool" dispatch.
///
/// When the live model calls this tool, the wrapped agent runs in an isolated
/// context (no live WebSocket). The agent's text output is collected and returned
/// as the tool result.
pub struct AgentTool {
    agent: Arc<dyn Agent>,
    description: String,
    parameters: Option<serde_json::Value>,
}

impl AgentTool {
    /// Create a new AgentTool wrapping the given agent.
    pub fn new(agent: impl Agent + 'static) -> Self {
        let description = format!("Delegate to the {} agent", agent.name());
        Self {
            agent: Arc::new(agent),
            description,
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "request": {
                        "type": "string",
                        "description": "The request to send to the agent"
                    }
                },
                "required": ["request"]
            })),
        }
    }

    /// Create from an already-Arc'd agent.
    pub fn from_arc(agent: Arc<dyn Agent>) -> Self {
        let description = format!("Delegate to the {} agent", agent.name());
        Self {
            agent,
            description,
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "request": {
                        "type": "string",
                        "description": "The request to send to the agent"
                    }
                },
                "required": ["request"]
            })),
        }
    }

    /// Override the tool description.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    /// Override the tool parameters schema.
    pub fn with_parameters(mut self, params: serde_json::Value) -> Self {
        self.parameters = Some(params);
        self
    }
}

#[async_trait]
impl ToolFunction for AgentTool {
    fn name(&self) -> &str {
        self.agent.name()
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        self.parameters.clone()
    }

    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let start = std::time::Instant::now();
        let agent_name = self.agent.name().to_string();

        // Telemetry
        crate::telemetry::logging::log_agent_tool_dispatch("parent", &agent_name);

        // 1. Create isolated context with NoOpSessionWriter
        let (event_tx, _) = broadcast::channel::<SessionEvent>(64);
        let noop_writer: Arc<dyn rs_genai::session::SessionWriter> =
            Arc::new(NoOpSessionWriter);
        let isolated_session = AgentSession::from_writer(noop_writer, event_tx);

        // 2. Inject args into state
        if let Some(request) = args.get("request").and_then(|r| r.as_str()) {
            isolated_session.state().set("request_text", request);
        }
        isolated_session.state().set("request", &args);

        // 3. Create isolated InvocationContext
        let mut ctx = InvocationContext::new(isolated_session);

        // 4. Subscribe to events before running (to collect text output)
        let mut events = ctx.subscribe();

        // 5. Run the agent
        let agent = self.agent.clone();
        let run_result = tokio::spawn(async move { agent.run_live(&mut ctx).await }).await;

        // 6. Collect text output from events
        let mut output_parts = Vec::new();
        while let Ok(event) = events.try_recv() {
            match event {
                AgentEvent::Session(SessionEvent::TextDelta(text)) => {
                    output_parts.push(text);
                }
                AgentEvent::Session(SessionEvent::TextComplete(text)) => {
                    if output_parts.is_empty() {
                        output_parts.push(text);
                    }
                    // If we already have deltas, TextComplete is the full assembled text
                    // Don't double-count — deltas already captured incrementally
                }
                _ => {}
            }
        }

        let elapsed = start.elapsed();
        crate::telemetry::metrics::record_agent_tool_dispatch(
            "parent",
            &agent_name,
            elapsed.as_millis() as f64,
        );

        // 7. Handle result
        match run_result {
            Ok(Ok(())) => {
                let output = if output_parts.is_empty() {
                    json!({"status": "completed"})
                } else {
                    json!({"result": output_parts.join("")})
                };
                Ok(output)
            }
            Ok(Err(e)) => Err(ToolError::ExecutionFailed(format!(
                "Agent '{}' failed: {}",
                agent_name, e
            ))),
            Err(e) => Err(ToolError::ExecutionFailed(format!(
                "Agent '{}' task panicked: {}",
                agent_name, e
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AgentError;

    struct EchoAgent {
        name: String,
    }

    #[async_trait]
    impl Agent for EchoAgent {
        fn name(&self) -> &str {
            &self.name
        }
        async fn run_live(&self, ctx: &mut InvocationContext) -> Result<(), AgentError> {
            // Read the request from state and echo it back as a text event
            let request = ctx
                .state()
                .get::<String>("request_text")
                .unwrap_or_else(|| "no request".to_string());
            ctx.emit(AgentEvent::Session(SessionEvent::TextDelta(format!(
                "Echo: {}",
                request
            ))));
            ctx.emit(AgentEvent::Session(SessionEvent::TurnComplete));
            Ok(())
        }
    }

    struct FailingAgent;

    #[async_trait]
    impl Agent for FailingAgent {
        fn name(&self) -> &str {
            "failing"
        }
        async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
            Err(AgentError::Other("intentional failure".to_string()))
        }
    }

    struct SilentAgent;

    #[async_trait]
    impl Agent for SilentAgent {
        fn name(&self) -> &str {
            "silent"
        }
        async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn agent_tool_runs_agent_in_isolation() {
        let agent = EchoAgent {
            name: "echo".to_string(),
        };
        let tool = AgentTool::new(agent);

        assert_eq!(tool.name(), "echo");
        assert!(tool.description().contains("echo"));
    }

    #[tokio::test]
    async fn agent_tool_collects_text_output() {
        let agent = EchoAgent {
            name: "echo".to_string(),
        };
        let tool = AgentTool::new(agent);

        let result = tool.call(json!({"request": "hello world"})).await.unwrap();
        assert_eq!(result["result"], "Echo: hello world");
    }

    #[tokio::test]
    async fn agent_tool_propagates_errors() {
        let tool = AgentTool::new(FailingAgent);
        let result = tool.call(json!({"request": "test"})).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            ToolError::ExecutionFailed(msg) => {
                assert!(msg.contains("intentional failure"));
            }
            other => panic!("expected ExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn agent_tool_returns_completed_when_no_output() {
        let tool = AgentTool::new(SilentAgent);
        let result = tool.call(json!({"request": "test"})).await.unwrap();
        assert_eq!(result["status"], "completed");
    }

    #[tokio::test]
    async fn agent_tool_state_injection() {
        // Verify that args are injected into state
        struct StateCheckAgent;

        #[async_trait]
        impl Agent for StateCheckAgent {
            fn name(&self) -> &str {
                "state_check"
            }
            async fn run_live(&self, ctx: &mut InvocationContext) -> Result<(), AgentError> {
                let request_text = ctx.state().get::<String>("request_text");
                let request = ctx.state().get::<serde_json::Value>("request");

                assert!(request_text.is_some());
                assert!(request.is_some());
                assert_eq!(request_text.unwrap(), "check state");

                ctx.emit(AgentEvent::Session(SessionEvent::TextDelta(
                    "state ok".to_string(),
                )));
                Ok(())
            }
        }

        let tool = AgentTool::new(StateCheckAgent);
        let result = tool.call(json!({"request": "check state"})).await.unwrap();
        assert_eq!(result["result"], "state ok");
    }

    #[tokio::test]
    async fn agent_tool_with_custom_description() {
        let tool = AgentTool::new(SilentAgent).with_description("Custom description");
        assert_eq!(tool.description(), "Custom description");
    }

    #[tokio::test]
    async fn agent_tool_with_custom_parameters() {
        let params = json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" }
            }
        });
        let tool = AgentTool::new(SilentAgent).with_parameters(params.clone());
        assert_eq!(tool.parameters().unwrap(), params);
    }
}
