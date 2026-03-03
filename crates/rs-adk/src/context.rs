//! InvocationContext — the session state container flowing through agent execution.
//!
//! Also provides `CallbackContext` and `ToolContext` wrappers for richer
//! access patterns in callbacks and tool execution.

use tokio::sync::broadcast;

use crate::agent_session::AgentSession;
use crate::confirmation::ToolConfirmation;
use crate::events::EventActions;
use crate::middleware::MiddlewareChain;
use crate::run_config::RunConfig;

/// Events emitted by agents during live execution.
/// Wraps SessionEvent (Layer 0) and adds agent-specific events.
/// No duplicate variants — use AgentEvent::Session(_) for wire-level events.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Passthrough of wire-level session events (text, audio, turn lifecycle).
    Session(rs_genai::session::SessionEvent),
    /// Agent lifecycle.
    /// An agent has started execution.
    AgentStarted {
        /// Name of the agent that started.
        name: String,
    },
    /// An agent has completed execution.
    AgentCompleted {
        /// Name of the agent that completed.
        name: String,
    },
    /// A tool call has started execution.
    ToolCallStarted {
        /// Tool name.
        name: String,
        /// Tool call arguments.
        args: serde_json::Value,
    },
    /// A tool call completed successfully.
    ToolCallCompleted {
        /// Tool name.
        name: String,
        /// The tool's return value.
        result: serde_json::Value,
        /// How long the tool call took.
        duration: std::time::Duration,
    },
    /// A tool call failed.
    ToolCallFailed {
        /// Tool name.
        name: String,
        /// Error description.
        error: String,
    },
    /// A streaming tool yielded an intermediate value.
    StreamingToolYield {
        /// Tool name.
        name: String,
        /// The yielded value.
        value: serde_json::Value,
    },
    /// An agent transferred control to another agent.
    AgentTransfer {
        /// Source agent name.
        from: String,
        /// Target agent name.
        to: String,
    },
    /// A state key was changed.
    StateChanged {
        /// The key that was modified.
        key: String,
    },
}

/// The context object that flows through agent execution.
/// Holds everything a running agent needs.
///
/// Note: State is accessed via agent_session.state() — single source of truth.
pub struct InvocationContext {
    /// AgentSession wraps SessionHandle with fan-out + middleware.
    /// Replaces LiveSender — sends go directly through SessionHandle (one hop).
    pub agent_session: AgentSession,

    /// Event bus — agents emit events here, application code subscribes.
    pub event_tx: broadcast::Sender<AgentEvent>,

    /// Middleware chain for lifecycle hooks.
    pub middleware: MiddlewareChain,

    /// Configuration for this run.
    pub run_config: RunConfig,

    /// Session ID for session-aware runs.
    pub session_id: Option<String>,
}

impl InvocationContext {
    /// Create a new invocation context with an empty middleware chain.
    pub fn new(agent_session: AgentSession) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            agent_session,
            event_tx,
            middleware: MiddlewareChain::new(),
            run_config: RunConfig::default(),
            session_id: None,
        }
    }

    /// Create a new invocation context with a pre-configured middleware chain.
    pub fn with_middleware(agent_session: AgentSession, middleware: MiddlewareChain) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            agent_session,
            event_tx,
            middleware,
            run_config: RunConfig::default(),
            session_id: None,
        }
    }

    /// Emit an event to all subscribers.
    pub fn emit(&self, event: AgentEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Subscribe to agent events.
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.event_tx.subscribe()
    }

    /// Convenience: access the state container.
    pub fn state(&self) -> &crate::state::State {
        self.agent_session.state()
    }
}

// ── CallbackContext ────────────────────────────────────────────────────────

/// Rich context for callbacks — provides access to state, artifacts, memory,
/// and event actions for mutation.
pub struct CallbackContext<'a> {
    ctx: &'a InvocationContext,
    /// Event actions that the callback can populate (e.g., state_delta, transfer).
    pub event_actions: EventActions,
}

impl<'a> CallbackContext<'a> {
    /// Create a new callback context wrapping an invocation context.
    pub fn new(ctx: &'a InvocationContext) -> Self {
        Self {
            ctx,
            event_actions: EventActions::default(),
        }
    }

    /// Access the state container.
    pub fn state(&self) -> &crate::state::State {
        self.ctx.state()
    }

    /// Get the invocation context's session ID, if any.
    pub fn session_id(&self) -> Option<&str> {
        self.ctx.session_id.as_deref()
    }

    /// Access the underlying invocation context.
    pub fn invocation_context(&self) -> &InvocationContext {
        self.ctx
    }
}

// ── ToolContext ─────────────────────────────────────────────────────────────

/// Extended context for tool execution — adds function call ID and confirmation.
pub struct ToolContext<'a> {
    /// The underlying callback context (provides state, event_actions, etc.).
    pub callback: CallbackContext<'a>,
    /// The ID of the function call being executed.
    pub function_call_id: Option<String>,
    /// User confirmation for this tool call, if applicable.
    pub confirmation: Option<ToolConfirmation>,
}

impl<'a> ToolContext<'a> {
    /// Create a new tool context.
    pub fn new(ctx: &'a InvocationContext, function_call_id: Option<String>) -> Self {
        Self {
            callback: CallbackContext::new(ctx),
            function_call_id,
            confirmation: None,
        }
    }

    /// Access the state container.
    pub fn state(&self) -> &crate::state::State {
        self.callback.state()
    }

    /// Set the confirmation for this tool call.
    pub fn with_confirmation(mut self, confirmation: ToolConfirmation) -> Self {
        self.confirmation = Some(confirmation);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_event_is_send_and_clone() {
        fn assert_send_clone<T: Send + Clone>() {}
        assert_send_clone::<AgentEvent>();
    }

    #[test]
    fn invocation_context_has_default_run_config() {
        use std::sync::Arc;
        use tokio::sync::broadcast;

        let (evt_tx, _) = broadcast::channel(16);
        let writer: Arc<dyn rs_genai::session::SessionWriter> =
            Arc::new(crate::test_helpers::MockWriter);
        let session = crate::agent_session::AgentSession::from_writer(writer, evt_tx);
        let ctx = InvocationContext::new(session);

        assert_eq!(ctx.run_config.max_llm_calls, 500);
        assert!(ctx.session_id.is_none());
    }

    #[test]
    fn callback_context_state_access() {
        use std::sync::Arc;
        use tokio::sync::broadcast;

        let (evt_tx, _) = broadcast::channel(16);
        let writer: Arc<dyn rs_genai::session::SessionWriter> =
            Arc::new(crate::test_helpers::MockWriter);
        let session = crate::agent_session::AgentSession::from_writer(writer, evt_tx);
        let ctx = InvocationContext::new(session);
        ctx.state().set("key", "value");

        let cb_ctx = CallbackContext::new(&ctx);
        assert_eq!(cb_ctx.state().get::<String>("key"), Some("value".to_string()));
    }

    #[test]
    fn tool_context_wraps_callback_context() {
        use std::sync::Arc;
        use tokio::sync::broadcast;

        let (evt_tx, _) = broadcast::channel(16);
        let writer: Arc<dyn rs_genai::session::SessionWriter> =
            Arc::new(crate::test_helpers::MockWriter);
        let session = crate::agent_session::AgentSession::from_writer(writer, evt_tx);
        let ctx = InvocationContext::new(session);
        ctx.state().set("x", 42);

        let tool_ctx = ToolContext::new(&ctx, Some("call-1".to_string()));
        assert_eq!(tool_ctx.state().get::<i32>("x"), Some(42));
        assert_eq!(tool_ctx.function_call_id.as_deref(), Some("call-1"));
        assert!(tool_ctx.confirmation.is_none());
    }

    #[test]
    fn tool_context_with_confirmation() {
        use std::sync::Arc;
        use tokio::sync::broadcast;

        let (evt_tx, _) = broadcast::channel(16);
        let writer: Arc<dyn rs_genai::session::SessionWriter> =
            Arc::new(crate::test_helpers::MockWriter);
        let session = crate::agent_session::AgentSession::from_writer(writer, evt_tx);
        let ctx = InvocationContext::new(session);

        let tool_ctx = ToolContext::new(&ctx, None)
            .with_confirmation(ToolConfirmation::confirmed());
        assert!(tool_ctx.confirmation.as_ref().unwrap().confirmed);
    }
}
