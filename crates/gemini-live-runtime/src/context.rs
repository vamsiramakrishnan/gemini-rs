//! InvocationContext — the session state container flowing through agent execution.

use tokio::sync::broadcast;

use crate::agent_session::AgentSession;

/// Events emitted by agents during live execution.
/// Wraps SessionEvent (Layer 0) and adds agent-specific events.
/// No duplicate variants — use AgentEvent::Session(_) for wire-level events.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Passthrough of wire-level session events (text, audio, turn lifecycle).
    Session(gemini_live_wire::session::SessionEvent),
    /// Agent lifecycle.
    AgentStarted { name: String },
    AgentCompleted { name: String },
    /// Tool lifecycle (not in SessionEvent).
    ToolCallStarted {
        name: String,
        args: serde_json::Value,
    },
    ToolCallCompleted {
        name: String,
        result: serde_json::Value,
        duration: std::time::Duration,
    },
    ToolCallFailed {
        name: String,
        error: String,
    },
    StreamingToolYield {
        name: String,
        value: serde_json::Value,
    },
    /// Multi-agent lifecycle.
    AgentTransfer { from: String, to: String },
    /// State changes.
    StateChanged { key: String },
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
}

impl InvocationContext {
    /// Create a new invocation context.
    pub fn new(agent_session: AgentSession) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            agent_session,
            event_tx,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_event_is_send_and_clone() {
        fn assert_send_clone<T: Send + Clone>() {}
        assert_send_clone::<AgentEvent>();
    }
}
