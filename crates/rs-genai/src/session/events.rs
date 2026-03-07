//! Session events, commands, and turn tracking.
//!
//! [`SessionEvent`] — events emitted by the server for application consumption.
//! [`SessionCommand`] — commands sent from the application to the transport.
//! [`Turn`] — tracking for a single model response turn.
//! [`recv_event`] — broadcast lag-tolerant event receiver.

use super::state::SessionPhase;
use crate::protocol::{Content, FunctionCall, FunctionResponse, UsageMetadata};
use std::time::Instant;
use tokio::sync::broadcast;

// ---------------------------------------------------------------------------
// Events (server -> application)
// ---------------------------------------------------------------------------

/// Events emitted by the session, consumed by application code.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// Session connected and setup complete.
    Connected,
    /// Incremental text from model response.
    TextDelta(String),
    /// Complete text of a finished model turn.
    TextComplete(String),
    /// Audio data from model response (PCM16 samples, base64-decoded).
    ///
    /// Uses [`bytes::Bytes`] for zero-copy fan-out: cloning a `Bytes` handle
    /// bumps an `Arc` refcount instead of copying the underlying data.
    AudioData(bytes::Bytes),
    /// Input transcription from server.
    InputTranscription(String),
    /// Output transcription from server.
    OutputTranscription(String),
    /// Model requested tool calls.
    ToolCall(Vec<FunctionCall>),
    /// Server cancelled pending tool calls.
    ToolCallCancelled(Vec<String>),
    /// Model turn is complete (it's the user's turn now).
    TurnComplete,
    /// Model finished generating its full response.
    ///
    /// Fires even if the generation was interrupted — tells you the model's
    /// internal generation pipeline has stopped. Distinct from `TurnComplete`
    /// which is the turn-taking signal.
    GenerationComplete,
    /// Model was interrupted by barge-in.
    Interrupted,
    /// Session phase changed.
    PhaseChanged(SessionPhase),
    /// Server sent GoAway signal with optional time remaining.
    GoAway(Option<String>),
    /// Session disconnected (with optional reason).
    Disconnected(Option<String>),
    /// Non-fatal error.
    Error(String),
    /// Session resumption update with handle, resumability, and consumed index.
    SessionResumeUpdate(ResumeInfo),
    /// Server-side voice activity detected (user started speaking).
    VoiceActivityStart,
    /// Server-side voice activity ended (user stopped speaking).
    VoiceActivityEnd,
    /// Token usage metadata from server (for context window tracking).
    ///
    /// Contains full token breakdown: prompt, response, cached, tool-use,
    /// thinking tokens, plus per-modality details.
    Usage(UsageMetadata),
}

/// Session resumption information from the server.
#[derive(Debug, Clone)]
pub struct ResumeInfo {
    /// Opaque handle for session resumption.
    pub handle: String,
    /// Whether the session is currently resumable.
    pub resumable: bool,
    /// Index of the last client message consumed by the server.
    pub last_consumed_index: Option<String>,
}

// ---------------------------------------------------------------------------
// Commands (application -> server)
// ---------------------------------------------------------------------------

/// Commands sent from application code to the session transport.
#[derive(Debug, Clone)]
pub enum SessionCommand {
    /// Send audio data (raw PCM16 bytes, will be base64-encoded).
    SendAudio(Vec<u8>),
    /// Send a text message.
    SendText(String),
    /// Send tool responses.
    SendToolResponse(Vec<FunctionResponse>),
    /// Signal activity start (client VAD detected speech).
    ActivityStart,
    /// Signal activity end (client VAD detected silence).
    ActivityEnd,
    /// Send client content (conversation history or context injection).
    SendClientContent {
        /// Conversation turns to include.
        turns: Vec<Content>,
        /// Whether this completes the client's turn.
        turn_complete: bool,
    },
    /// Send video/image data (raw JPEG bytes, will be base64-encoded).
    SendVideo(Vec<u8>),
    /// Update system instruction mid-session (sends client_content with role=system).
    UpdateInstruction(String),
    /// Gracefully disconnect.
    Disconnect,
}

// ---------------------------------------------------------------------------
// Turn tracking
// ---------------------------------------------------------------------------

/// Represents a single model response turn.
#[derive(Debug, Clone)]
pub struct Turn {
    /// Unique turn identifier.
    pub id: String,
    /// Accumulated text parts.
    pub text: String,
    /// Whether this turn included audio.
    pub has_audio: bool,
    /// Tool calls requested in this turn.
    pub tool_calls: Vec<FunctionCall>,
    /// When the turn started.
    pub started_at: Instant,
    /// When the turn completed (if complete).
    pub completed_at: Option<Instant>,
    /// Whether the turn was interrupted.
    pub interrupted: bool,
}

impl Turn {
    /// Create a new turn.
    pub fn new() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            text: String::new(),
            has_audio: false,
            tool_calls: Vec::new(),
            started_at: Instant::now(),
            completed_at: None,
            interrupted: false,
        }
    }

    /// Duration of the turn.
    pub fn duration(&self) -> std::time::Duration {
        let end = self.completed_at.unwrap_or_else(Instant::now);
        end.duration_since(self.started_at)
    }
}

impl Default for Turn {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Broadcast lag helper
// ---------------------------------------------------------------------------

/// Receive the next event from a broadcast receiver, handling lag gracefully.
///
/// If the receiver falls behind (too slow to keep up with the sender), the
/// skipped events are logged and the next available event is returned.
/// Returns `None` when the channel is closed.
///
/// # Example
///
/// ```ignore
/// let mut events = handle.subscribe();
/// while let Some(event) = recv_event(&mut events).await {
///     // handle event
/// }
/// ```
pub async fn recv_event(rx: &mut broadcast::Receiver<SessionEvent>) -> Option<SessionEvent> {
    loop {
        match rx.recv().await {
            Ok(event) => return Some(event),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                #[cfg(feature = "tracing-support")]
                tracing::warn!(skipped = n, "Event subscriber lagged, skipped {n} events");
                // Without tracing, silently continue
                let _ = n;
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recv_event_returns_events_normally() {
        let (tx, mut rx) = broadcast::channel(16);

        tx.send(SessionEvent::Connected).unwrap();
        tx.send(SessionEvent::TurnComplete).unwrap();

        let event = recv_event(&mut rx).await;
        assert!(matches!(event, Some(SessionEvent::Connected)));

        let event = recv_event(&mut rx).await;
        assert!(matches!(event, Some(SessionEvent::TurnComplete)));
    }

    #[tokio::test]
    async fn recv_event_returns_none_on_closed_channel() {
        let (tx, mut rx) = broadcast::channel::<SessionEvent>(16);
        drop(tx);

        let event = recv_event(&mut rx).await;
        assert!(event.is_none(), "should return None when channel is closed");
    }

    #[tokio::test]
    async fn recv_event_handles_lag() {
        // Create a tiny broadcast channel (capacity 2)
        let (tx, mut rx) = broadcast::channel(2);

        // Send 4 events — the receiver will lag behind
        for i in 0..4 {
            let _ = tx.send(SessionEvent::TextDelta(format!("msg{i}")));
        }

        // recv_event should skip the lagged events and return the next available
        let event = recv_event(&mut rx).await;
        assert!(event.is_some(), "should get an event after lag");
    }
}
