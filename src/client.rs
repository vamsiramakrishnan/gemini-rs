//! Client SDK protocol — types for browser/mobile ↔ server communication.
//!
//! This module defines the event protocol between a client application
//! (browser, mobile, SIP gateway) and a server built with `gemini-live-rs`.
//!
//! # Design Principles
//!
//! - **Audio as binary WebSocket frames**: Audio data is sent as raw binary
//!   frames (not base64-encoded JSON) for zero overhead. Text frames carry
//!   JSON-encoded control messages.
//!
//! - **Codec negotiation**: Client and server negotiate audio format
//!   (codec, sample rate, channels) at connection time via [`AudioNegotiation`].
//!
//! - **Thin relay pattern**: The server acts as a relay between the client
//!   WebSocket and the Gemini Live API. See the [relay pattern](#relay-pattern)
//!   section for implementation guidance.
//!
//! # Relay Pattern
//!
//! The recommended server architecture:
//!
//! ```text
//! Browser/Mobile                    Your Server                    Gemini Live API
//! ┌─────────────┐   WebSocket    ┌──────────────┐   WebSocket   ┌───────────────┐
//! │ Client SDK  │◄──────────────►│  Relay       │◄─────────────►│ BidiGenerate  │
//! │ (JS/Swift)  │  binary=audio  │  Handler     │  JSON+base64  │   Content     │
//! │             │  text=events   │              │               │               │
//! └─────────────┘                └──────────────┘               └───────────────┘
//! ```
//!
//! 1. Client connects via WebSocket to your server
//! 2. Server creates a [`GeminiAgent`](crate::app::GeminiAgent) and
//!    [`CallSession`](crate::call::CallSession)
//! 3. Server implements [`AudioSource`](crate::pipeline::AudioSource) backed by
//!    client WebSocket binary frames
//! 4. Server implements [`AudioSink`](crate::pipeline::AudioSink) that sends
//!    binary frames back to the client
//! 5. Server relays [`SessionEvent`](crate::session::SessionEvent) →
//!    [`ServerEvent`] as JSON text frames
//!
//! ## Example (Axum pseudocode)
//!
//! ```rust,ignore
//! async fn ws_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
//!     ws.on_upgrade(|socket| async {
//!         let (ws_sink, ws_source) = socket.split();
//!         let audio_source = WebSocketAudioSource::new(ws_source);
//!         let audio_sink = WebSocketAudioSink::new(ws_sink.clone());
//!
//!         let agent = GeminiAgent::builder()
//!             .api_key(key)
//!             .system_instruction("You are a helpful assistant.")
//!             .build().await.unwrap();
//!
//!         let call = CallSession::inbound(
//!             agent.session().clone(),
//!             agent.pipeline_config.clone(),
//!             Box::new(audio_source),
//!             Box::new(audio_sink),
//!         ).await.unwrap();
//!
//!         // Relay session events to client as JSON
//!         let mut events = agent.subscribe();
//!         while let Ok(event) = events.recv().await {
//!             if let Some(server_event) = ServerEvent::from_session_event(event) {
//!                 let json = serde_json::to_string(&server_event).unwrap();
//!                 ws_sink.send(TextMessage(json)).await.ok();
//!             }
//!         }
//!     })
//! }
//! ```

use serde::{Deserialize, Serialize};

use crate::session::SessionEvent;

// ---------------------------------------------------------------------------
// Audio negotiation
// ---------------------------------------------------------------------------

/// Audio format negotiation between client and server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioNegotiation {
    /// Codec name (e.g., "pcm16", "opus").
    pub codec: String,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Number of audio channels.
    pub channels: u8,
    /// Frame duration in milliseconds.
    pub frame_duration_ms: u32,
}

impl Default for AudioNegotiation {
    fn default() -> Self {
        Self {
            codec: "pcm16".into(),
            sample_rate: 16_000,
            channels: 1,
            frame_duration_ms: 30,
        }
    }
}

// ---------------------------------------------------------------------------
// Client → Server events
// ---------------------------------------------------------------------------

/// Events sent from the client to the server over WebSocket.
///
/// Audio data should be sent as **binary WebSocket frames** (not in this JSON
/// envelope) for efficiency. These JSON events handle control signaling.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientEvent {
    /// Text message from client.
    #[serde(rename = "text")]
    Text { text: String },

    /// Client-side VAD event.
    #[serde(rename = "vad")]
    Vad { speaking: bool },

    /// Audio format negotiation.
    #[serde(rename = "negotiate")]
    Negotiate { audio: AudioNegotiation },

    /// Client requests hangup.
    #[serde(rename = "hangup")]
    Hangup,
}

// ---------------------------------------------------------------------------
// Server → Client events
// ---------------------------------------------------------------------------

/// Events sent from the server to the client over WebSocket.
///
/// Audio data should be sent as **binary WebSocket frames** (not in this JSON
/// envelope). These JSON events handle everything else.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    /// Session is ready.
    #[serde(rename = "ready")]
    Ready { session_id: String },

    /// Negotiated audio format.
    #[serde(rename = "negotiated")]
    Negotiated { audio: AudioNegotiation },

    /// Incremental text from model.
    #[serde(rename = "text_delta")]
    TextDelta { text: String },

    /// Complete text response from model.
    #[serde(rename = "text_complete")]
    TextComplete { text: String },

    /// Input transcription (what the user said).
    #[serde(rename = "input_transcription")]
    InputTranscription { text: String },

    /// Output transcription (what the model said).
    #[serde(rename = "output_transcription")]
    OutputTranscription { text: String },

    /// Model turn complete.
    #[serde(rename = "turn_complete")]
    TurnComplete,

    /// Model was interrupted.
    #[serde(rename = "interrupted")]
    Interrupted,

    /// Error from server.
    #[serde(rename = "error")]
    Error { message: String },

    /// Session ended.
    #[serde(rename = "ended")]
    Ended { reason: Option<String> },
}

impl ServerEvent {
    /// Convert a [`SessionEvent`] to a [`ServerEvent`] for client relay.
    ///
    /// Returns `None` for events that should not be relayed (e.g., `Connected`
    /// is handled separately as `Ready`, and `AudioData` is sent as binary frames).
    pub fn from_session_event(event: SessionEvent) -> Option<Self> {
        match event {
            SessionEvent::Connected => None, // Use Ready with session_id instead
            SessionEvent::TextDelta(t) => Some(ServerEvent::TextDelta { text: t }),
            SessionEvent::TextComplete(t) => Some(ServerEvent::TextComplete { text: t }),
            SessionEvent::AudioData(_) => None, // Sent as binary WebSocket frame
            SessionEvent::InputTranscription(t) => {
                Some(ServerEvent::InputTranscription { text: t })
            }
            SessionEvent::OutputTranscription(t) => {
                Some(ServerEvent::OutputTranscription { text: t })
            }
            SessionEvent::TurnComplete => Some(ServerEvent::TurnComplete),
            SessionEvent::Interrupted => Some(ServerEvent::Interrupted),
            SessionEvent::Error(e) => Some(ServerEvent::Error { message: e }),
            SessionEvent::Disconnected(r) => Some(ServerEvent::Ended { reason: r }),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_event_serialization() {
        let event = ClientEvent::Text {
            text: "Hello".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"text\":\"Hello\""));

        let parsed: ClientEvent = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientEvent::Text { text } => assert_eq!(text, "Hello"),
            _ => panic!("Expected Text"),
        }
    }

    #[test]
    fn client_event_vad() {
        let event = ClientEvent::Vad { speaking: true };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"vad\""));
        assert!(json.contains("\"speaking\":true"));
    }

    #[test]
    fn client_event_negotiate() {
        let event = ClientEvent::Negotiate {
            audio: AudioNegotiation::default(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"negotiate\""));
        assert!(json.contains("\"pcm16\""));
    }

    #[test]
    fn client_event_hangup() {
        let event = ClientEvent::Hangup;
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"hangup\""));
    }

    #[test]
    fn server_event_serialization() {
        let event = ServerEvent::TextDelta {
            text: "Hello".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"text_delta\""));

        let event = ServerEvent::Ready {
            session_id: "abc".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"ready\""));
        assert!(json.contains("\"session_id\":\"abc\""));
    }

    #[test]
    fn server_event_ended() {
        let event = ServerEvent::Ended {
            reason: Some("timeout".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"ended\""));
        assert!(json.contains("\"timeout\""));
    }

    #[test]
    fn session_event_to_server_event() {
        // Text delta maps
        assert!(matches!(
            ServerEvent::from_session_event(SessionEvent::TextDelta("hi".into())),
            Some(ServerEvent::TextDelta { .. })
        ));

        // Audio data is None (sent as binary)
        assert!(ServerEvent::from_session_event(SessionEvent::AudioData(vec![1, 2, 3])).is_none());

        // Connected is None (use Ready instead)
        assert!(ServerEvent::from_session_event(SessionEvent::Connected).is_none());

        // Disconnected maps to Ended
        assert!(matches!(
            ServerEvent::from_session_event(SessionEvent::Disconnected(Some("bye".into()))),
            Some(ServerEvent::Ended {
                reason: Some(r),
            }) if r == "bye"
        ));
    }

    #[test]
    fn audio_negotiation_defaults() {
        let n = AudioNegotiation::default();
        assert_eq!(n.codec, "pcm16");
        assert_eq!(n.sample_rate, 16_000);
        assert_eq!(n.channels, 1);
        assert_eq!(n.frame_duration_ms, 30);
    }
}
