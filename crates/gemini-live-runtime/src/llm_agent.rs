//! LlmAgent — concrete Agent implementation with builder pattern.
//!
//! The builder freezes tools at `build()` time (respecting Gemini Live's
//! constraint that tools are fixed at session setup). Auto-registers
//! `transfer_to_{name}` tools for each sub-agent.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;

use gemini_live_wire::prelude::Tool;

use crate::agent::Agent;
use crate::context::InvocationContext;
use crate::error::AgentError;
use crate::middleware::MiddlewareChain;
use crate::tool::{
    InputStreamingTool, SimpleTool, StreamingTool, ToolDispatcher, ToolFunction, TypedTool,
};

/// Concrete Agent implementation that runs a Gemini Live event loop.
///
/// Tools are declared at build time and sent during session setup.
/// The event loop subscribes to SessionEvents, auto-dispatches tool calls,
/// detects transfers, and emits AgentEvents.
pub struct LlmAgent {
    name: String,
    dispatcher: ToolDispatcher,
    middleware: MiddlewareChain,
    sub_agents: Vec<Arc<dyn Agent>>,
}

impl LlmAgent {
    /// Start building a new LlmAgent.
    pub fn builder(name: impl Into<String>) -> LlmAgentBuilder {
        LlmAgentBuilder {
            name: name.into(),
            dispatcher: ToolDispatcher::new(),
            middleware: MiddlewareChain::new(),
            sub_agents: Vec::new(),
        }
    }

    /// Access the tool dispatcher (for testing/introspection).
    pub fn dispatcher(&self) -> &ToolDispatcher {
        &self.dispatcher
    }

    /// Access the middleware chain.
    pub fn middleware(&self) -> &MiddlewareChain {
        &self.middleware
    }
}

/// Builder for LlmAgent — fluent API for declaring tools, middleware, sub-agents.
pub struct LlmAgentBuilder {
    name: String,
    dispatcher: ToolDispatcher,
    middleware: MiddlewareChain,
    sub_agents: Vec<Arc<dyn Agent>>,
}

impl LlmAgentBuilder {
    /// Register a regular function tool.
    pub fn tool(mut self, tool: impl ToolFunction + 'static) -> Self {
        self.dispatcher.register_function(Arc::new(tool));
        self
    }

    /// Register a typed tool with auto-generated JSON Schema.
    pub fn typed_tool<T>(mut self, tool: TypedTool<T>) -> Self
    where
        T: serde::de::DeserializeOwned + schemars::JsonSchema + Send + Sync + 'static,
    {
        self.dispatcher.register_function(Arc::new(tool));
        self
    }

    /// Register a streaming tool.
    pub fn streaming_tool(mut self, tool: impl StreamingTool + 'static) -> Self {
        self.dispatcher.register_streaming(Arc::new(tool));
        self
    }

    /// Register an input-streaming tool.
    pub fn input_streaming_tool(mut self, tool: impl InputStreamingTool + 'static) -> Self {
        self.dispatcher.register_input_streaming(Arc::new(tool));
        self
    }

    /// Add middleware to the agent.
    pub fn middleware(mut self, mw: impl crate::middleware::Middleware + 'static) -> Self {
        self.middleware.add(Arc::new(mw));
        self
    }

    /// Register a sub-agent (enables transfer_to_{name} tool).
    pub fn sub_agent(mut self, agent: impl Agent + 'static) -> Self {
        self.sub_agents.push(Arc::new(agent));
        self
    }

    /// Set the default timeout for tool execution.
    pub fn tool_timeout(mut self, timeout: Duration) -> Self {
        self.dispatcher = self.dispatcher.with_timeout(timeout);
        self
    }

    /// Build the LlmAgent, freezing all tool declarations.
    ///
    /// This:
    /// 1. Auto-registers `transfer_to_{name}` SimpleTool for each sub_agent
    /// 2. Prepends TelemetryMiddleware
    /// 3. Returns the frozen LlmAgent
    pub fn build(mut self) -> LlmAgent {
        // Auto-register transfer tools for sub-agents
        for sub in &self.sub_agents {
            let target_name = sub.name().to_string();
            let tool_name = format!("transfer_to_{}", target_name);
            let transfer_tool = SimpleTool::new(
                tool_name,
                format!("Transfer conversation to the {} agent", target_name),
                Some(json!({
                    "type": "object",
                    "properties": {},
                })),
                move |_args| {
                    let name = target_name.clone();
                    async move { Ok(json!({"__transfer_to": name})) }
                },
            );
            self.dispatcher.register_function(Arc::new(transfer_tool));
        }

        // Prepend TelemetryMiddleware so it runs first
        self.middleware.prepend(Arc::new(
            crate::telemetry::TelemetryMiddleware::new(&self.name),
        ));

        LlmAgent {
            name: self.name,
            dispatcher: self.dispatcher,
            middleware: self.middleware,
            sub_agents: self.sub_agents,
        }
    }
}

#[async_trait]
impl Agent for LlmAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
        // Stub — implemented in Task 3
        todo!("LlmAgent event loop implemented in Task 3")
    }

    fn tools(&self) -> Vec<Tool> {
        self.dispatcher.to_tool_declarations()
    }

    fn sub_agents(&self) -> Vec<Arc<dyn Agent>> {
        self.sub_agents.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct NoopAgent {
        name: String,
    }

    #[async_trait]
    impl Agent for NoopAgent {
        fn name(&self) -> &str {
            &self.name
        }
        async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
            Ok(())
        }
    }

    #[test]
    fn builder_creates_agent_with_name() {
        let agent = LlmAgent::builder("test_agent").build();
        assert_eq!(agent.name(), "test_agent");
    }

    #[test]
    fn builder_registers_tools() {
        let tool = SimpleTool::new("my_tool", "desc", None, |_| async { Ok(json!({})) });
        let agent = LlmAgent::builder("test").tool(tool).build();
        // my_tool is the only user tool (TelemetryMiddleware doesn't add tools)
        assert_eq!(agent.dispatcher().len(), 1);
    }

    #[test]
    fn builder_auto_registers_transfer_tools() {
        let sub = NoopAgent {
            name: "billing".to_string(),
        };
        let agent = LlmAgent::builder("root").sub_agent(sub).build();

        // Should have transfer_to_billing auto-registered
        assert!(agent.dispatcher().classify("transfer_to_billing").is_some());
    }

    #[test]
    fn builder_with_multiple_sub_agents() {
        let sub1 = NoopAgent {
            name: "billing".to_string(),
        };
        let sub2 = NoopAgent {
            name: "tech".to_string(),
        };
        let agent = LlmAgent::builder("root")
            .sub_agent(sub1)
            .sub_agent(sub2)
            .build();

        assert!(agent.dispatcher().classify("transfer_to_billing").is_some());
        assert!(agent.dispatcher().classify("transfer_to_tech").is_some());
        assert_eq!(agent.sub_agents().len(), 2);
    }

    #[test]
    fn tools_returns_declarations() {
        let tool = SimpleTool::new("my_tool", "desc", None, |_| async { Ok(json!({})) });
        let agent = LlmAgent::builder("test").tool(tool).build();
        let tools = agent.tools();
        assert!(!tools.is_empty());
    }

    #[test]
    fn transfer_requested_error() {
        let err = AgentError::TransferRequested("billing".to_string());
        assert!(err.to_string().contains("billing"));
    }

    #[test]
    fn builder_prepends_telemetry_middleware() {
        let agent = LlmAgent::builder("test").build();
        // TelemetryMiddleware is auto-prepended
        assert_eq!(agent.middleware().len(), 1);
    }

    #[test]
    fn builder_with_user_middleware_and_telemetry() {
        use crate::middleware::LogMiddleware;

        let agent = LlmAgent::builder("test")
            .middleware(LogMiddleware::new())
            .build();
        // TelemetryMiddleware (prepended) + LogMiddleware (user-added)
        assert_eq!(agent.middleware().len(), 2);
    }

    #[test]
    fn get_tool_returns_tool_kind() {
        let tool = SimpleTool::new("lookup", "desc", None, |_| async { Ok(json!({})) });
        let agent = LlmAgent::builder("test").tool(tool).build();
        assert!(agent.dispatcher().get_tool("lookup").is_some());
        assert!(agent.dispatcher().get_tool("nonexistent").is_none());
    }
}
