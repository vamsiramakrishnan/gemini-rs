//! Semantic events emitted by the L1 processor.
//!
//! Subscribe via `LiveHandle::events()`. Zero-cost when no subscribers.

use std::time::Duration;

use bytes::Bytes;

/// Semantic events emitted by the Live session processor.
///
/// The L1 equivalent of L0's [`SessionEvent`](rs_genai::prelude::SessionEvent).
/// L0 events are wire-level; LiveEvents are semantic (extractions completed,
/// phases transitioned, tools executed).
///
/// Subscribe via [`LiveHandle::events()`](super::handle::LiveHandle::events).
/// Multiple independent subscribers supported. Zero-cost when no subscribers
/// exist (`broadcast::send` with 0 receivers is a no-op).
#[derive(Debug, Clone)]
pub enum LiveEvent {
    // -- Fast-lane events (high frequency, sync emission) --
    /// Raw PCM audio from model. Uses `Bytes` (refcounted) — clone is
    /// a pointer increment (~2ns), not a deep copy.
    Audio(Bytes),
    /// Incremental text token from model.
    TextDelta(String),
    /// Complete text response (all deltas concatenated).
    TextComplete(String),
    /// User speech transcription.
    InputTranscript {
        /// The transcribed text content.
        text: String,
        /// Whether this is the final transcription for the utterance.
        is_final: bool,
    },
    /// Model speech transcription.
    OutputTranscript {
        /// The transcribed text content.
        text: String,
        /// Whether this is the final transcription for the utterance.
        is_final: bool,
    },
    /// Model reasoning/thinking content.
    Thought(String),
    /// Voice activity detected — user started speaking.
    VadStart,
    /// Voice activity ended — user stopped speaking.
    VadEnd,

    // -- Control-lane events (lower frequency, async emission) --
    /// Extraction completed. Emitted for both the top-level result
    /// AND each flattened key (e.g., "order.items", "order.phase").
    Extraction {
        /// Extractor name, or `"extractor.field"` for flattened keys.
        name: String,
        /// The extracted JSON value.
        value: serde_json::Value,
    },
    /// Extraction failed.
    ExtractionError {
        /// Name of the extractor that failed.
        name: String,
        /// Human-readable error description.
        error: String,
    },
    /// Phase machine transitioned.
    PhaseTransition {
        /// Phase the machine transitioned from.
        from: String,
        /// Phase the machine transitioned to.
        to: String,
        /// Human-readable reason for the transition.
        reason: String,
    },
    /// Tool dispatched and result obtained.
    ToolExecution {
        /// Name of the tool that was called.
        name: String,
        /// Arguments passed to the tool.
        args: serde_json::Value,
        /// Result returned by the tool.
        result: serde_json::Value,
    },
    /// Model completed a conversational turn.
    TurnComplete,
    /// Model output interrupted by user speech.
    Interrupted,
    /// Session connected to Gemini.
    Connected,
    /// Session disconnected.
    Disconnected {
        /// Optional reason for disconnection (server-provided or error message).
        reason: Option<String>,
    },
    /// Unrecoverable error.
    Error(String),
    /// Server requesting session wind-down.
    GoAway {
        /// Time remaining before the server closes the connection.
        time_left: Duration,
    },

    // -- Periodic events --
    /// Aggregated session telemetry snapshot.
    Telemetry(serde_json::Value),
    /// Per-turn latency and token metrics.
    TurnMetrics {
        /// Turn number (1-indexed).
        turn: u32,
        /// End-to-end latency for this turn in milliseconds.
        latency_ms: u32,
        /// Number of prompt tokens consumed.
        prompt_tokens: u32,
        /// Number of response tokens generated.
        response_tokens: u32,
    },
}
