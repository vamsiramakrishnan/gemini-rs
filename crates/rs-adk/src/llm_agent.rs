//! LlmAgent — concrete Agent implementation with builder pattern.
//!
//! The builder freezes tools at `build()` time (respecting Gemini Live's
//! constraint that tools are fixed at session setup). Auto-registers
//! `transfer_to_{name}` tools for each sub-agent.
//!
//! The event loop subscribes to SessionEvents, auto-dispatches tool calls,
//! detects transfers via `__transfer_to` signal in tool results, and handles
//! streaming/input-streaming tools.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::broadcast;

use rs_genai::prelude::{recv_event, FunctionResponse, Tool};
use rs_genai::session::SessionEvent;

use crate::agent::Agent;
use crate::context::{AgentEvent, InvocationContext};
use crate::error::{AgentError, ToolError};
use crate::middleware::MiddlewareChain;
use crate::tool::{
    ActiveStreamingTool, InputStreamingTool, SimpleTool, StreamingTool, ToolClass, ToolDispatcher,
    ToolFunction, ToolKind, TypedTool,
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

    /// Core event loop -- processes SessionEvents, dispatches tools, detects transfers.
    async fn event_loop(
        &self,
        ctx: &mut InvocationContext,
        events: &mut broadcast::Receiver<SessionEvent>,
        agent_name: &str,
    ) -> Result<(), AgentError> {
        loop {
            let event = match recv_event(events).await {
                Some(e) => e,
                None => break, // channel closed
            };

            match event {
                SessionEvent::ToolCall(calls) => {
                    let mut responses = Vec::new();
                    let mut transfer_target = None;

                    for call in &calls {
                        // Emit events + middleware hooks
                        ctx.emit(AgentEvent::ToolCallStarted {
                            name: call.name.clone(),
                            args: call.args.clone(),
                        });
                        let _ = ctx.middleware.run_before_tool(call).await;

                        let tool_start = std::time::Instant::now();
                        let tool_class = self.dispatcher.classify(&call.name);

                        match tool_class {
                            Some(ToolClass::Regular) => {
                                crate::telemetry::logging::log_tool_dispatch(
                                    agent_name,
                                    &call.name,
                                    "function",
                                );
                                crate::telemetry::metrics::record_agent_tool_dispatched(
                                    agent_name,
                                    &call.name,
                                );

                                let result = self
                                    .dispatcher
                                    .call_function(&call.name, call.args.clone())
                                    .await;
                                let elapsed = tool_start.elapsed();

                                match &result {
                                    Ok(value) => {
                                        // Check for transfer signal
                                        if let Some(target) =
                                            value.get("__transfer_to").and_then(|v| v.as_str())
                                        {
                                            transfer_target = Some(target.to_string());
                                        }

                                        let _ = ctx.middleware.run_after_tool(call, value).await;
                                        ctx.emit(AgentEvent::ToolCallCompleted {
                                            name: call.name.clone(),
                                            result: value.clone(),
                                            duration: elapsed,
                                        });
                                        crate::telemetry::logging::log_tool_result(
                                            agent_name,
                                            &call.name,
                                            true,
                                            elapsed.as_millis() as f64,
                                        );
                                        crate::telemetry::metrics::record_agent_tool_duration(
                                            agent_name,
                                            &call.name,
                                            elapsed.as_millis() as f64,
                                        );
                                    }
                                    Err(e) => {
                                        let _ =
                                            ctx.middleware.run_on_tool_error(call, e).await;
                                        ctx.emit(AgentEvent::ToolCallFailed {
                                            name: call.name.clone(),
                                            error: e.to_string(),
                                        });
                                        crate::telemetry::logging::log_tool_result(
                                            agent_name,
                                            &call.name,
                                            false,
                                            elapsed.as_millis() as f64,
                                        );
                                    }
                                }

                                responses.push(ToolDispatcher::build_response(call, result));
                            }
                            Some(ToolClass::Streaming) | Some(ToolClass::InputStream) => {
                                let class_str =
                                    if tool_class == Some(ToolClass::Streaming) {
                                        "streaming"
                                    } else {
                                        "input_stream"
                                    };
                                crate::telemetry::logging::log_tool_dispatch(
                                    agent_name,
                                    &call.name,
                                    class_str,
                                );

                                self.spawn_streaming_tool(call, ctx, agent_name).await;

                                responses.push(FunctionResponse {
                                    name: call.name.clone(),
                                    response: json!({"status": "streaming"}),
                                    id: call.id.clone(),
                                });
                            }
                            None => {
                                ctx.emit(AgentEvent::ToolCallFailed {
                                    name: call.name.clone(),
                                    error: format!("Tool not found: {}", call.name),
                                });
                                responses.push(ToolDispatcher::build_response(
                                    call,
                                    Err(ToolError::NotFound(call.name.clone())),
                                ));
                            }
                        }
                    }

                    // Send all responses back to Gemini
                    ctx.agent_session.send_tool_response(responses).await?;

                    // Handle transfer AFTER sending response
                    if let Some(target) = transfer_target {
                        ctx.emit(AgentEvent::AgentTransfer {
                            from: agent_name.to_string(),
                            to: target.clone(),
                        });
                        crate::telemetry::metrics::record_agent_transfer(agent_name, &target);
                        crate::telemetry::logging::log_agent_transfer(agent_name, &target);
                        return Err(AgentError::TransferRequested(target));
                    }
                }
                SessionEvent::ToolCallCancelled(ids) => {
                    self.dispatcher.cancel_by_ids(&ids).await;
                }
                SessionEvent::TurnComplete => {
                    ctx.emit(AgentEvent::Session(SessionEvent::TurnComplete));
                    break;
                }
                SessionEvent::Disconnected(reason) => {
                    ctx.emit(AgentEvent::Session(SessionEvent::Disconnected(reason)));
                    break;
                }
                SessionEvent::Error(ref e) => {
                    ctx.emit(AgentEvent::Session(event.clone()));
                    crate::telemetry::metrics::record_agent_error(agent_name, "session_error");
                    crate::telemetry::logging::log_agent_error(agent_name, e);
                }
                other => {
                    // Pass through all other events (TextDelta, AudioData, etc.)
                    ctx.emit(AgentEvent::Session(other));
                }
            }
        }
        Ok(())
    }

    /// Spawn a streaming or input-streaming tool as a background task.
    async fn spawn_streaming_tool(
        &self,
        call: &rs_genai::prelude::FunctionCall,
        ctx: &InvocationContext,
        _agent_name: &str,
    ) {
        let tool_kind = match self.dispatcher.get_tool(&call.name) {
            Some(kind) => kind,
            None => return,
        };

        let (yield_tx, mut yield_rx) = tokio::sync::mpsc::channel::<serde_json::Value>(32);
        let cancel = tokio_util::sync::CancellationToken::new();

        let tool_name = call.name.clone();
        let call_id = call.id.clone();
        let args = call.args.clone();
        let event_tx = ctx.event_tx.clone();
        let agent_session = ctx.agent_session.clone();

        match tool_kind {
            ToolKind::Streaming(tool) => {
                let tool = tool.clone();
                let cancel_clone = cancel.clone();
                let tool_name_err = tool_name.clone();
                let event_tx_err = event_tx.clone();

                let tool_task = tokio::spawn(async move {
                    tokio::select! {
                        result = tool.run(args, yield_tx) => {
                            if let Err(e) = result {
                                let _ = event_tx_err.send(AgentEvent::ToolCallFailed {
                                    name: tool_name_err,
                                    error: e.to_string(),
                                });
                            }
                        }
                        _ = cancel_clone.cancelled() => {}
                    }
                });

                let active = ActiveStreamingTool {
                    task: tool_task,
                    cancel,
                };
                let id = call_id
                    .clone()
                    .unwrap_or_else(|| tool_name.clone());
                self.dispatcher.store_active(id, active).await;
            }
            ToolKind::InputStream(tool) => {
                let tool = tool.clone();
                let input_rx = ctx.agent_session.subscribe_input();
                let cancel_clone = cancel.clone();
                let tool_name_err = tool_name.clone();
                let event_tx_err = event_tx.clone();

                let tool_task = tokio::spawn(async move {
                    tokio::select! {
                        result = tool.run(args, input_rx, yield_tx) => {
                            if let Err(e) = result {
                                let _ = event_tx_err.send(AgentEvent::ToolCallFailed {
                                    name: tool_name_err,
                                    error: e.to_string(),
                                });
                            }
                        }
                        _ = cancel_clone.cancelled() => {}
                    }
                });

                let active = ActiveStreamingTool {
                    task: tool_task,
                    cancel,
                };
                let id = call_id
                    .clone()
                    .unwrap_or_else(|| tool_name.clone());
                self.dispatcher.store_active(id, active).await;
            }
            ToolKind::Function(_) => {} // shouldn't reach here
        }

        // Spawn collector: reads yields and forwards as events + sends final FunctionResponse
        let yield_tool_name = call.name.clone();
        let yield_call_id = call.id.clone();

        tokio::spawn(async move {
            let mut all_yields = Vec::new();
            while let Some(value) = yield_rx.recv().await {
                let _ = event_tx.send(AgentEvent::StreamingToolYield {
                    name: yield_tool_name.clone(),
                    value: value.clone(),
                });
                all_yields.push(value);
            }

            // Send final response when tool completes
            let final_response = if all_yields.is_empty() {
                json!({"status": "completed"})
            } else if all_yields.len() == 1 {
                all_yields.into_iter().next().unwrap()
            } else {
                json!({"results": all_yields})
            };

            let resp = FunctionResponse {
                name: yield_tool_name,
                response: final_response,
                id: yield_call_id,
            };
            let _ = agent_session.send_tool_response(vec![resp]).await;
        });
    }
}

/// Builder for LlmAgent -- fluent API for declaring tools, middleware, sub-agents.
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

    async fn run_live(&self, ctx: &mut InvocationContext) -> Result<(), AgentError> {
        let agent_name = self.name.clone();
        let start = std::time::Instant::now();

        // Telemetry + middleware
        crate::telemetry::logging::log_agent_started(&agent_name, self.dispatcher.len());
        crate::telemetry::metrics::record_agent_started(&agent_name);
        ctx.middleware.run_before_agent(ctx).await?;
        ctx.emit(AgentEvent::AgentStarted {
            name: agent_name.clone(),
        });

        let mut events = ctx.agent_session.subscribe_events();

        let result = self.event_loop(ctx, &mut events, &agent_name).await;

        // Cleanup
        let elapsed = start.elapsed();
        ctx.middleware.run_after_agent(ctx).await?;
        ctx.emit(AgentEvent::AgentCompleted {
            name: agent_name.clone(),
        });
        crate::telemetry::logging::log_agent_completed(&agent_name, elapsed.as_millis() as f64);
        crate::telemetry::metrics::record_agent_completed(
            &agent_name,
            elapsed.as_millis() as f64,
        );

        result
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
    use rs_genai::prelude::FunctionCall;
    use rs_genai::session::{SessionError, SessionWriter};
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

    /// Mock writer that accepts all commands without error.
    struct MockWriter;

    #[async_trait]
    impl SessionWriter for MockWriter {
        async fn send_audio(&self, _data: Vec<u8>) -> Result<(), SessionError> {
            Ok(())
        }
        async fn send_text(&self, _text: String) -> Result<(), SessionError> {
            Ok(())
        }
        async fn send_tool_response(
            &self,
            _responses: Vec<FunctionResponse>,
        ) -> Result<(), SessionError> {
            Ok(())
        }
        async fn send_client_content(
            &self,
            _turns: Vec<rs_genai::prelude::Content>,
            _turn_complete: bool,
        ) -> Result<(), SessionError> {
            Ok(())
        }
        async fn signal_activity_start(&self) -> Result<(), SessionError> {
            Ok(())
        }
        async fn signal_activity_end(&self) -> Result<(), SessionError> {
            Ok(())
        }
        async fn disconnect(&self) -> Result<(), SessionError> {
            Ok(())
        }
    }

    /// Create a mock AgentSession backed by MockWriter, returning the session
    /// and the event sender so tests can inject SessionEvents.
    fn mock_agent_session() -> (crate::agent_session::AgentSession, broadcast::Sender<SessionEvent>) {
        let (evt_tx, _) = broadcast::channel(64);
        let writer: Arc<dyn SessionWriter> = Arc::new(MockWriter);
        let session = crate::agent_session::AgentSession::from_writer(writer, evt_tx.clone());
        (session, evt_tx)
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

    // ── Event loop tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn event_loop_breaks_on_turn_complete() {
        let agent = LlmAgent::builder("test").build();
        let (session, evt_tx) = mock_agent_session();
        let mut ctx = InvocationContext::new(session);

        // Send TurnComplete after a short delay
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = evt_tx.send(SessionEvent::TurnComplete);
        });

        let result = agent.run_live(&mut ctx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn event_loop_breaks_on_disconnect() {
        let agent = LlmAgent::builder("test").build();
        let (session, evt_tx) = mock_agent_session();
        let mut ctx = InvocationContext::new(session);

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = evt_tx.send(SessionEvent::Disconnected(Some("bye".to_string())));
        });

        let result = agent.run_live(&mut ctx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn event_loop_dispatches_tool_call() {
        let tool = SimpleTool::new("get_weather", "Get weather", None, |_| async {
            Ok(json!({"temp": 22}))
        });
        let agent = LlmAgent::builder("test").tool(tool).build();
        let (session, evt_tx) = mock_agent_session();
        let mut ctx = InvocationContext::new(session);
        let mut agent_events = ctx.subscribe();

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = evt_tx.send(SessionEvent::ToolCall(vec![FunctionCall {
                name: "get_weather".to_string(),
                args: json!({"city": "London"}),
                id: Some("call-1".to_string()),
            }]));
            // The tool response will be sent back; then end the turn.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = evt_tx.send(SessionEvent::TurnComplete);
        });

        let result = agent.run_live(&mut ctx).await;
        assert!(result.is_ok());

        // Check that we got ToolCallStarted and ToolCallCompleted events
        let mut saw_tool_started = false;
        let mut saw_tool_completed = false;
        while let Ok(event) = agent_events.try_recv() {
            match event {
                AgentEvent::ToolCallStarted { name, .. } if name == "get_weather" => {
                    saw_tool_started = true;
                }
                AgentEvent::ToolCallCompleted { name, result, .. } if name == "get_weather" => {
                    assert_eq!(result["temp"], 22);
                    saw_tool_completed = true;
                }
                _ => {}
            }
        }
        assert!(saw_tool_started, "should have emitted ToolCallStarted");
        assert!(saw_tool_completed, "should have emitted ToolCallCompleted");
    }

    #[tokio::test]
    async fn event_loop_handles_unknown_tool() {
        let agent = LlmAgent::builder("test").build();
        let (session, evt_tx) = mock_agent_session();
        let mut ctx = InvocationContext::new(session);
        let mut agent_events = ctx.subscribe();

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = evt_tx.send(SessionEvent::ToolCall(vec![FunctionCall {
                name: "nonexistent_tool".to_string(),
                args: json!({}),
                id: Some("call-1".to_string()),
            }]));
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = evt_tx.send(SessionEvent::TurnComplete);
        });

        let result = agent.run_live(&mut ctx).await;
        assert!(result.is_ok());

        // Check that we got a ToolCallFailed event
        let mut saw_tool_failed = false;
        while let Ok(event) = agent_events.try_recv() {
            if let AgentEvent::ToolCallFailed { name, error } = event {
                if name == "nonexistent_tool" {
                    assert!(error.contains("not found") || error.contains("Not found"));
                    saw_tool_failed = true;
                }
            }
        }
        assert!(saw_tool_failed, "should have emitted ToolCallFailed for unknown tool");
    }

    #[tokio::test]
    async fn event_loop_detects_transfer() {
        let sub = NoopAgent {
            name: "billing".to_string(),
        };
        let agent = LlmAgent::builder("root").sub_agent(sub).build();

        let (session, evt_tx) = mock_agent_session();
        let mut ctx = InvocationContext::new(session);
        let mut agent_events = ctx.subscribe();

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = evt_tx.send(SessionEvent::ToolCall(vec![FunctionCall {
                name: "transfer_to_billing".to_string(),
                args: json!({}),
                id: Some("call-1".to_string()),
            }]));
        });

        let result = agent.run_live(&mut ctx).await;
        match result {
            Err(AgentError::TransferRequested(target)) => assert_eq!(target, "billing"),
            other => panic!("expected TransferRequested, got: {:?}", other),
        }

        // Check that AgentTransfer event was emitted
        let mut saw_transfer = false;
        while let Ok(event) = agent_events.try_recv() {
            if let AgentEvent::AgentTransfer { from, to } = event {
                assert_eq!(from, "root");
                assert_eq!(to, "billing");
                saw_transfer = true;
            }
        }
        assert!(saw_transfer, "should have emitted AgentTransfer event");
    }

    #[tokio::test]
    async fn event_loop_passes_through_events() {
        let agent = LlmAgent::builder("test").build();
        let (session, evt_tx) = mock_agent_session();
        let mut ctx = InvocationContext::new(session);
        let mut agent_events = ctx.subscribe();

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = evt_tx.send(SessionEvent::TextDelta("hello".to_string()));
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = evt_tx.send(SessionEvent::TurnComplete);
        });

        agent.run_live(&mut ctx).await.unwrap();

        // Check that we got AgentStarted, TextDelta passthrough, TurnComplete, AgentCompleted
        let mut saw_text_delta = false;
        let mut saw_started = false;
        let mut saw_completed = false;
        while let Ok(event) = agent_events.try_recv() {
            match event {
                AgentEvent::AgentStarted { .. } => saw_started = true,
                AgentEvent::AgentCompleted { .. } => saw_completed = true,
                AgentEvent::Session(SessionEvent::TextDelta(t)) if t == "hello" => {
                    saw_text_delta = true;
                }
                _ => {}
            }
        }
        assert!(saw_started, "should have emitted AgentStarted");
        assert!(saw_text_delta, "should have passed through TextDelta");
        assert!(saw_completed, "should have emitted AgentCompleted");
    }

    #[tokio::test]
    async fn event_loop_handles_error_event() {
        let agent = LlmAgent::builder("test").build();
        let (session, evt_tx) = mock_agent_session();
        let mut ctx = InvocationContext::new(session);
        let mut agent_events = ctx.subscribe();

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = evt_tx.send(SessionEvent::Error("something broke".to_string()));
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = evt_tx.send(SessionEvent::TurnComplete);
        });

        agent.run_live(&mut ctx).await.unwrap();

        // Check that the error event was passed through
        let mut saw_error = false;
        while let Ok(event) = agent_events.try_recv() {
            if let AgentEvent::Session(SessionEvent::Error(e)) = event {
                assert_eq!(e, "something broke");
                saw_error = true;
            }
        }
        assert!(saw_error, "should have passed through Error event");
    }

    #[tokio::test]
    async fn event_loop_emits_lifecycle_events() {
        let agent = LlmAgent::builder("lifecycle_test").build();
        let (session, evt_tx) = mock_agent_session();
        let mut ctx = InvocationContext::new(session);
        let mut agent_events = ctx.subscribe();

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = evt_tx.send(SessionEvent::TurnComplete);
        });

        agent.run_live(&mut ctx).await.unwrap();

        let mut events = Vec::new();
        while let Ok(event) = agent_events.try_recv() {
            events.push(event);
        }

        // First event should be AgentStarted
        assert!(
            matches!(&events[0], AgentEvent::AgentStarted { name } if name == "lifecycle_test"),
            "first event should be AgentStarted, got: {:?}",
            events[0]
        );

        // Last event should be AgentCompleted
        let last = events.last().unwrap();
        assert!(
            matches!(last, AgentEvent::AgentCompleted { name } if name == "lifecycle_test"),
            "last event should be AgentCompleted, got: {:?}",
            last
        );
    }

    #[tokio::test]
    async fn event_loop_tool_failure_emits_failed_event() {
        let tool = SimpleTool::new("failing_tool", "Always fails", None, |_| async {
            Err(ToolError::ExecutionFailed("kaboom".to_string()))
        });
        let agent = LlmAgent::builder("test").tool(tool).build();
        let (session, evt_tx) = mock_agent_session();
        let mut ctx = InvocationContext::new(session);
        let mut agent_events = ctx.subscribe();

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = evt_tx.send(SessionEvent::ToolCall(vec![FunctionCall {
                name: "failing_tool".to_string(),
                args: json!({}),
                id: Some("call-1".to_string()),
            }]));
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = evt_tx.send(SessionEvent::TurnComplete);
        });

        agent.run_live(&mut ctx).await.unwrap();

        let mut saw_tool_failed = false;
        while let Ok(event) = agent_events.try_recv() {
            if let AgentEvent::ToolCallFailed { name, error } = event {
                if name == "failing_tool" {
                    assert!(error.contains("kaboom"));
                    saw_tool_failed = true;
                }
            }
        }
        assert!(saw_tool_failed, "should have emitted ToolCallFailed");
    }
}
