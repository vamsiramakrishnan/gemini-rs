//! Integration tests for gemini-adk.
//!
//! These tests exercise multiple components working together through the
//! public API only.  They use mock SessionWriters so no real WebSocket or
//! Gemini Live connection is required.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::broadcast;

use gemini_live::prelude::{Content, FunctionCall, FunctionResponse};
use gemini_live::session::{SessionError, SessionEvent, SessionWriter};

use gemini_adk::agent::Agent;
use gemini_adk::agent_session::AgentSession;
use gemini_adk::agent_tool::AgentTool;
use gemini_adk::context::{AgentEvent, InvocationContext};
use gemini_adk::error::AgentError;
use gemini_adk::middleware::{Middleware, MiddlewareChain};
use gemini_adk::runner::Runner;
use gemini_adk::tool::{SimpleTool, ToolFunction};
use gemini_adk::LlmAgent;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// A SessionWriter that accepts all commands without error.
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
        _turns: Vec<Content>,
        _turn_complete: bool,
    ) -> Result<(), SessionError> {
        Ok(())
    }
    async fn send_video(&self, _jpeg_data: Vec<u8>) -> Result<(), SessionError> {
        Ok(())
    }
    async fn update_instruction(&self, _instruction: String) -> Result<(), SessionError> {
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
fn mock_agent_session() -> (AgentSession, broadcast::Sender<SessionEvent>) {
    let (evt_tx, _) = broadcast::channel(64);
    let writer: Arc<dyn SessionWriter> = Arc::new(MockWriter);
    let session = AgentSession::from_writer(writer, evt_tx.clone());
    (session, evt_tx)
}

/// A no-op agent that returns immediately.
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

/// An agent that transfers to another named agent.
struct TransferAgent {
    name: String,
    target: String,
}

#[async_trait]
impl Agent for TransferAgent {
    fn name(&self) -> &str {
        &self.name
    }
    async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> {
        Err(AgentError::TransferRequested(self.target.clone()))
    }
}

// ---------------------------------------------------------------------------
// (a) full_tool_call_roundtrip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_tool_call_roundtrip() {
    // Build an LlmAgent with a SimpleTool.
    let tool = SimpleTool::new("get_weather", "Get weather info", None, |_args| async {
        Ok(json!({"temp": 22, "unit": "celsius"}))
    });
    let agent = LlmAgent::builder("weather_agent").tool(tool).build();

    // Create a mock session and context.
    let (session, evt_tx) = mock_agent_session();
    let mut ctx = InvocationContext::new(session);

    // Subscribe to AgentEvents BEFORE running.
    let mut agent_events = ctx.subscribe();

    // Inject a ToolCall event followed by TurnComplete from a background task.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let _ = evt_tx.send(SessionEvent::ToolCall(vec![FunctionCall {
            name: "get_weather".to_string(),
            args: json!({"city": "London"}),
            id: Some("call-1".to_string()),
        }]));
        // Give the agent time to dispatch the tool and send the response.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = evt_tx.send(SessionEvent::TurnComplete);
    });

    // Run the agent — it should process the tool call and terminate on TurnComplete.
    let result = agent.run_live(&mut ctx).await;
    assert!(result.is_ok(), "agent should complete without error");

    // Verify we got ToolCallStarted and ToolCallCompleted events.
    let mut saw_tool_started = false;
    let mut saw_tool_completed = false;
    let mut completed_result = None;

    while let Ok(event) = agent_events.try_recv() {
        match event {
            AgentEvent::ToolCallStarted { name, .. } if name == "get_weather" => {
                saw_tool_started = true;
            }
            AgentEvent::ToolCallCompleted {
                name,
                result,
                duration,
                ..
            } if name == "get_weather" => {
                saw_tool_completed = true;
                completed_result = Some(result);
                assert!(duration.as_millis() < 5000, "tool should complete quickly");
            }
            _ => {}
        }
    }

    assert!(saw_tool_started, "should have emitted ToolCallStarted");
    assert!(saw_tool_completed, "should have emitted ToolCallCompleted");

    // Verify the tool result content.
    let result_val = completed_result.expect("should have a result");
    assert_eq!(result_val["temp"], 22);
    assert_eq!(result_val["unit"], "celsius");
}

// ---------------------------------------------------------------------------
// (b) agent_tool_in_llm_agent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn agent_tool_in_llm_agent() {
    // Create a simple inner agent.
    let inner_agent = NoopAgent {
        name: "summarizer".to_string(),
    };

    // Wrap it as an AgentTool.
    let agent_tool = AgentTool::new(inner_agent);

    // Register it in an LlmAgent via the .tool() builder method.
    let agent = LlmAgent::builder("orchestrator").tool(agent_tool).build();

    // Verify the AgentTool appears in tool declarations.
    let tools = agent.tools();
    assert!(!tools.is_empty(), "should have tool declarations");

    // The dispatcher should have the "summarizer" tool registered.
    assert!(
        agent.dispatcher().classify("summarizer").is_some(),
        "should have 'summarizer' tool registered via AgentTool"
    );
}

// ---------------------------------------------------------------------------
// (c) runner_transfer_cycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn runner_transfer_cycle() {
    // Root agent transfers to "sub_agent".
    let root = TransferAgent {
        name: "root".to_string(),
        target: "sub_agent".to_string(),
    };

    let sub = NoopAgent {
        name: "sub_agent".to_string(),
    };

    let mut runner = Runner::new(root);
    runner.register(Arc::new(sub));

    let connect_count = Arc::new(AtomicU32::new(0));
    let count = connect_count.clone();

    let result = runner
        .run(move |_agent| {
            let c = count.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                let (session, _evt_tx) = mock_agent_session();
                Ok(session)
            }
        })
        .await;

    assert!(result.is_ok(), "runner should complete successfully");
    // Should have connected twice: once for root, once for sub_agent.
    assert_eq!(
        connect_count.load(Ordering::SeqCst),
        2,
        "runner should connect once per agent (root + sub_agent)"
    );
}

// ---------------------------------------------------------------------------
// (d) middleware_chain_called_in_order
// ---------------------------------------------------------------------------

/// Middleware that increments a shared counter in `before_agent`.
struct CountingMiddleware {
    label: String,
    order_log: Arc<parking_lot::Mutex<Vec<String>>>,
}

#[async_trait]
impl Middleware for CountingMiddleware {
    fn name(&self) -> &str {
        &self.label
    }
    async fn before_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> {
        self.order_log.lock().push(self.label.clone());
        Ok(())
    }
}

#[tokio::test]
async fn middleware_chain_called_in_order() {
    let order_log = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));

    // Build a simple agent (no middleware on the agent itself).
    let agent = LlmAgent::builder("mw_test").build();

    // Build a MiddlewareChain with two counting middleware and pass it
    // to the InvocationContext.  (This mirrors what Runner does: middleware
    // is applied at the context level, not baked into the agent.)
    let mut chain = MiddlewareChain::new();
    chain.add(Arc::new(CountingMiddleware {
        label: "first".to_string(),
        order_log: order_log.clone(),
    }));
    chain.add(Arc::new(CountingMiddleware {
        label: "second".to_string(),
        order_log: order_log.clone(),
    }));

    let (session, evt_tx) = mock_agent_session();
    let mut ctx = InvocationContext::with_middleware(session, chain);

    // End the turn immediately so the agent completes.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let _ = evt_tx.send(SessionEvent::TurnComplete);
    });

    agent.run_live(&mut ctx).await.unwrap();

    // The middleware chain should have been called: first, then second.
    let log = order_log.lock().clone();
    assert!(
        log.len() >= 2,
        "both counting middleware should have been called, got {:?}",
        log
    );
    let first_idx = log.iter().position(|s| s == "first").unwrap();
    let second_idx = log.iter().position(|s| s == "second").unwrap();
    assert!(
        first_idx < second_idx,
        "'first' middleware should run before 'second'"
    );
}

// ---------------------------------------------------------------------------
// (e) events_emitted_in_order
// ---------------------------------------------------------------------------

#[tokio::test]
async fn events_emitted_in_order() {
    let agent = LlmAgent::builder("ordered_agent").build();

    let (session, evt_tx) = mock_agent_session();
    let mut ctx = InvocationContext::new(session);
    let mut agent_events = ctx.subscribe();

    // Send TurnComplete after a short delay.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let _ = evt_tx.send(SessionEvent::TurnComplete);
    });

    agent.run_live(&mut ctx).await.unwrap();

    // Collect all events.
    let mut events = Vec::new();
    while let Ok(event) = agent_events.try_recv() {
        events.push(event);
    }

    assert!(
        events.len() >= 3,
        "should have at least AgentStarted, TurnComplete, AgentCompleted; got {} events",
        events.len()
    );

    // Verify ordering: AgentStarted must come first.
    assert!(
        matches!(&events[0], AgentEvent::AgentStarted { name } if name == "ordered_agent"),
        "first event should be AgentStarted, got: {:?}",
        events[0]
    );

    // TurnComplete should come before AgentCompleted.
    let turn_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Session(SessionEvent::TurnComplete)))
        .expect("should have TurnComplete event");

    let completed_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::AgentCompleted { .. }))
        .expect("should have AgentCompleted event");

    assert!(
        turn_idx < completed_idx,
        "TurnComplete (idx={}) should come before AgentCompleted (idx={})",
        turn_idx,
        completed_idx
    );

    // AgentCompleted should be the last event.
    assert!(
        matches!(events.last().unwrap(), AgentEvent::AgentCompleted { name } if name == "ordered_agent"),
        "last event should be AgentCompleted, got: {:?}",
        events.last().unwrap()
    );
}

// ---------------------------------------------------------------------------
// (f) state_preserved_across_agent_tool
// ---------------------------------------------------------------------------

/// An agent that reads state["parent_data"] and emits it as text.
struct StateReadingAgent;

#[async_trait]
impl Agent for StateReadingAgent {
    fn name(&self) -> &str {
        "state_reader"
    }
    async fn run_live(&self, ctx: &mut InvocationContext) -> Result<(), AgentError> {
        // Read the request from state (injected by AgentTool).
        let request = ctx
            .state()
            .get::<String>("request_text")
            .unwrap_or_else(|| "none".to_string());

        // Emit the text back as output.
        ctx.emit(AgentEvent::Session(SessionEvent::TextDelta(format!(
            "got: {}",
            request
        ))));
        Ok(())
    }
}

#[tokio::test]
async fn state_preserved_across_agent_tool() {
    // The AgentTool wraps an agent that reads state injected via args.
    let agent_tool = AgentTool::new(StateReadingAgent);

    // Call the tool with a request containing data the parent would set.
    let result = agent_tool
        .call(json!({"request": "parent_value_42"}))
        .await
        .unwrap();

    // The wrapped agent should have read the injected state and echoed it.
    assert_eq!(
        result["result"], "got: parent_value_42",
        "AgentTool should inject args into state and the wrapped agent should read them"
    );
}
