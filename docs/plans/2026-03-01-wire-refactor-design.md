# Wire Crate Refactor — Approach A: Trait-Boundary Refactor

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `gemini-genai-wire` production-grade, extensible, and SDK-parity by introducing 3 trait boundaries (Codec, Transport, Auth) plus type-safety fixes, enabling upper layers (runtime + fluent) to plug in custom behavior without forking.

**Architecture:** Three phases, each independently shippable. Phase 1 fixes invalid-state-representability (typed errors, Role enum, Content builder). Phase 2 extracts Codec/Transport/SessionWriter traits from the monolithic `connection.rs`. Phase 3 adds AuthProvider trait and SDK-parity features (ToolProvider, Platform abstraction, session resumption).

**Tech Stack:** Rust (tokio, tokio-tungstenite, serde, async-trait, thiserror), TDD with proptest + tokio-test.

---

## Phase 1: Make Invalid States Unrepresentable

### Task 1: Add Role enum and Content builders

**Files:**
- Modify: `crates/gemini-genai-wire/src/protocol/types.rs:214-220` (Content struct)
- Test: `crates/gemini-genai-wire/src/protocol/types.rs` (existing test module at bottom)

**Step 1: Write failing tests for Role enum and Content builders**

Add to the test module at the bottom of `types.rs`:

```rust
#[test]
fn role_serialization() {
    assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"user\"");
    assert_eq!(serde_json::to_string(&Role::Model).unwrap(), "\"model\"");
}

#[test]
fn role_deserialization() {
    let role: Role = serde_json::from_str("\"user\"").unwrap();
    assert_eq!(role, Role::User);
    let role: Role = serde_json::from_str("\"model\"").unwrap();
    assert_eq!(role, Role::Model);
}

#[test]
fn content_user_builder() {
    let c = Content::user("Hello");
    assert_eq!(c.role, Some(Role::User));
    assert_eq!(c.parts.len(), 1);
    match &c.parts[0] {
        Part::Text { text } => assert_eq!(text, "Hello"),
        _ => panic!("expected text part"),
    }
}

#[test]
fn content_model_builder() {
    let c = Content::model("Hi there");
    assert_eq!(c.role, Some(Role::Model));
}

#[test]
fn content_from_parts_builder() {
    let c = Content::from_parts(Role::User, vec![Part::text("test")]);
    assert_eq!(c.parts.len(), 1);
}

#[test]
fn part_text_builder() {
    let p = Part::text("hello");
    match p {
        Part::Text { text } => assert_eq!(text, "hello"),
        _ => panic!("expected text"),
    }
}

#[test]
fn part_inline_data_builder() {
    let p = Part::inline_data("audio/pcm", "AAAA");
    match p {
        Part::InlineData { inline_data } => {
            assert_eq!(inline_data.mime_type, "audio/pcm");
        }
        _ => panic!("expected inline data"),
    }
}

#[test]
fn part_function_call_builder() {
    let call = FunctionCall { name: "test".into(), args: serde_json::json!({}), id: None };
    let p = Part::function_call(call);
    assert!(matches!(p, Part::FunctionCall { .. }));
}

#[test]
fn content_role_backward_compat_serialization() {
    // Verify Content with Role enum serializes to same JSON as old String-based approach
    let c = Content {
        role: Some(Role::User),
        parts: vec![Part::Text { text: "hi".into() }],
    };
    let json = serde_json::to_string(&c).unwrap();
    assert!(json.contains("\"role\":\"user\""));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p gemini-genai-wire -- role_ content_user content_model content_from_parts part_text part_inline part_function content_role_backward`
Expected: Compilation failure — `Role` type doesn't exist, `Content::user()` etc. don't exist

**Step 3: Implement Role enum and Content/Part builders**

In `types.rs`, above the `Content` struct (around line 214):

```rust
/// Role in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Model,
    System,
}
```

Change `Content` struct:

```rust
/// A content message containing a role and a sequence of parts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Content {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    pub parts: Vec<Part>,
}

impl Content {
    /// Create user content with a single text part.
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Some(Role::User),
            parts: vec![Part::text(text)],
        }
    }

    /// Create model content with a single text part.
    pub fn model(text: impl Into<String>) -> Self {
        Self {
            role: Some(Role::Model),
            parts: vec![Part::text(text)],
        }
    }

    /// Create a function response content message.
    pub fn function_response(name: impl Into<String>, response: serde_json::Value) -> Self {
        Self {
            role: Some(Role::User),
            parts: vec![Part::FunctionResponse {
                function_response: FunctionResponse {
                    name: name.into(),
                    response,
                    id: None,
                },
            }],
        }
    }

    /// Create content from a role and a list of parts.
    pub fn from_parts(role: Role, parts: Vec<Part>) -> Self {
        Self {
            role: Some(role),
            parts,
        }
    }
}
```

Add `Part` builder methods:

```rust
impl Part {
    /// Create a text part.
    pub fn text(s: impl Into<String>) -> Self {
        Part::Text { text: s.into() }
    }

    /// Create an inline data part.
    pub fn inline_data(mime_type: impl Into<String>, data: impl Into<String>) -> Self {
        Part::InlineData {
            inline_data: Blob {
                mime_type: mime_type.into(),
                data: data.into(),
            },
        }
    }

    /// Create a function call part.
    pub fn function_call(call: FunctionCall) -> Self {
        Part::FunctionCall {
            function_call: call,
        }
    }
}
```

**Step 4: Fix all compilation errors across workspace**

The `Content.role` change from `Option<String>` to `Option<Role>` breaks:
- `crates/gemini-genai-wire/src/transport/connection.rs:320` — `role: Some("user".to_string())` → `role: Some(Role::User)`
- `crates/gemini-genai-wire/src/protocol/messages.rs:560` (test) — `role: Some("user".to_string())` → `role: Some(Role::User)`
- Any runtime/fluent references to `Content { role: Some("user"...) }`

Search across all crates for `role: Some("` and update each occurrence.

**Step 5: Run all tests**

Run: `cargo test --workspace`
Expected: All tests pass (87 wire + 66 runtime + 32 fluent + doc-tests)

**Step 6: Commit**

```bash
git add crates/gemini-genai-wire/src/protocol/types.rs crates/gemini-genai-wire/src/transport/connection.rs crates/gemini-genai-wire/src/protocol/messages.rs
# Also add any runtime/fluent files that needed Content.role fixes
git commit -m "refactor(wire): add Role enum, Content/Part builders"
```

---

### Task 2: Structured error types

**Files:**
- Modify: `crates/gemini-genai-wire/src/session/mod.rs:20-54` (SessionError enum)
- Modify: `crates/gemini-genai-wire/src/transport/connection.rs` (error construction sites)
- Test: `crates/gemini-genai-wire/src/session/mod.rs` (add tests)
- Modify: `crates/gemini-genai-runtime/src/error.rs:1-27` (AgentError::Session derives)

**Step 1: Write failing tests for new error types**

```rust
#[test]
fn websocket_error_display() {
    let err = SessionError::WebSocket(WebSocketError::ConnectionRefused("host unreachable".into()));
    assert!(err.to_string().contains("host unreachable"));
}

#[test]
fn setup_error_display() {
    let err = SessionError::SetupFailed(SetupError::AuthenticationFailed("bad token".into()));
    assert!(err.to_string().contains("bad token"));
}

#[test]
fn auth_error_display() {
    let err = SessionError::Auth(AuthError::TokenExpired);
    assert!(err.to_string().contains("expired"));
}

#[test]
fn timeout_error_includes_phase() {
    let err = SessionError::Timeout {
        phase: SessionPhase::SetupSent,
        elapsed: std::time::Duration::from_secs(15),
    };
    let s = err.to_string();
    assert!(s.contains("SetupSent"));
}

#[test]
fn goaway_error_with_duration() {
    let err = SessionError::GoAway { time_left: Some(std::time::Duration::from_secs(30)) };
    let s = err.to_string();
    assert!(s.contains("30"));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p gemini-genai-wire -- websocket_error setup_error auth_error timeout_error goaway_error`
Expected: Compilation failure

**Step 3: Implement structured error types**

Replace `SessionError` in `session/mod.rs`:

```rust
/// Errors that can occur during a session.
#[derive(Debug, Error, Clone)]
pub enum SessionError {
    /// WebSocket-level error (transient, may be retried).
    #[error("WebSocket error: {0}")]
    WebSocket(WebSocketError),

    /// Timeout waiting for a phase.
    #[error("Timeout in {phase} after {elapsed:?}")]
    Timeout {
        phase: SessionPhase,
        elapsed: std::time::Duration,
    },

    /// Attempted an invalid phase transition.
    #[error("Invalid transition from {from} to {to}")]
    InvalidTransition { from: SessionPhase, to: SessionPhase },

    /// Operation requires an active connection.
    #[error("Not connected")]
    NotConnected,

    /// Server rejected the setup configuration.
    #[error("Setup failed: {0}")]
    SetupFailed(SetupError),

    /// Server requested graceful disconnect.
    #[error("Server sent GoAway (time left: {time_left:?})")]
    GoAway { time_left: Option<std::time::Duration> },

    /// Internal channel was closed unexpectedly.
    #[error("Internal channel closed")]
    ChannelClosed,

    /// Send queue is full.
    #[error("Send queue full")]
    SendQueueFull,

    /// Authentication error.
    #[error("Auth error: {0}")]
    Auth(AuthError),
}

/// WebSocket-level error details.
#[derive(Debug, Error, Clone)]
pub enum WebSocketError {
    #[error("Connection refused: {0}")]
    ConnectionRefused(String),
    #[error("Protocol error: {0}")]
    ProtocolError(String),
    #[error("Connection closed (code={code}, reason={reason})")]
    Closed { code: u16, reason: String },
}

/// Setup failure details.
#[derive(Debug, Error, Clone)]
pub enum SetupError {
    #[error("Invalid model: {0}")]
    InvalidModel(String),
    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),
    #[error("Server rejected: {message}")]
    ServerRejected { code: Option<String>, message: String },
    #[error("Setup timed out")]
    Timeout,
}

/// Authentication error details.
#[derive(Debug, Error, Clone)]
pub enum AuthError {
    #[error("Token expired")]
    TokenExpired,
    #[error("Token fetch failed: {0}")]
    TokenFetchFailed(String),
    #[error("Insufficient scopes: {0}")]
    InsufficientScopes(String),
}
```

**Step 4: Update connection.rs error construction sites**

Update every `SessionError::WebSocket(string)` to `SessionError::WebSocket(WebSocketError::ProtocolError(string))` and every `SessionError::SetupFailed(string)` to appropriate `SetupError` variant. Update `SessionError::Timeout` to include phase and elapsed. Update `SessionError::GoAway` to include `time_left` as `Option<Duration>`.

Key locations in `connection.rs`:
- Line 109: `SessionError::WebSocket(e.to_string())` → `SessionError::WebSocket(WebSocketError::ProtocolError(e.to_string()))`
- Line 163-164: request build error → `WebSocketError::ProtocolError`
- Line 171: bearer token error → `SetupError::AuthenticationFailed`
- Line 183: connect error → `WebSocketError::ConnectionRefused`
- Line 182: timeout → `SessionError::Timeout { phase: SessionPhase::Connecting, elapsed: Duration::from_secs(config.connect_timeout_secs) }`
- Line 207-209: close during setup → `SetupError::ServerRejected`
- Line 222: stream ended → `SetupError::Timeout`
- Line 226: setup timeout → `SessionError::Timeout { phase: SessionPhase::SetupSent, elapsed: ... }`

**Step 5: Update runtime error handling**

In `crates/gemini-genai-runtime/src/error.rs`, `AgentError::Session` already uses `#[from] SessionError` — it will auto-adapt. But check any match arms on `SessionError` variants in the runtime crate.

**Step 6: Run all tests**

Run: `cargo test --workspace`
Expected: All pass

**Step 7: Commit**

```bash
git add crates/gemini-genai-wire/src/session/mod.rs crates/gemini-genai-wire/src/transport/connection.rs crates/gemini-genai-runtime/src/error.rs
git commit -m "refactor(wire): structured SessionError, WebSocketError, SetupError, AuthError"
```

---

### Task 3: Emit PhaseChanged events

**Files:**
- Modify: `crates/gemini-genai-wire/src/session/mod.rs:208-215` (SessionState::transition_to)
- Modify: `crates/gemini-genai-wire/src/transport/connection.rs:25-50` (connect fn — pass event_tx to state)
- Test: `crates/gemini-genai-wire/src/session/mod.rs`

**Step 1: Write failing test**

```rust
#[tokio::test]
async fn phase_changed_event_emitted_on_transition() {
    let (phase_tx, _phase_rx) = watch::channel(SessionPhase::Disconnected);
    let (event_tx, mut event_rx) = broadcast::channel(16);
    let state = SessionState::with_events(phase_tx, event_tx);

    state.transition_to(SessionPhase::Connecting).unwrap();

    match event_rx.try_recv() {
        Ok(SessionEvent::PhaseChanged(SessionPhase::Connecting)) => {},
        other => panic!("expected PhaseChanged(Connecting), got {:?}", other),
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p gemini-genai-wire -- phase_changed_event`
Expected: Fail — `SessionState::with_events` doesn't exist

**Step 3: Implement**

Add `event_tx: Option<broadcast::Sender<SessionEvent>>` field to `SessionState`. Add `with_events` constructor. In `transition_to()`, after successful transition, broadcast `PhaseChanged(to)`.

```rust
impl SessionState {
    pub fn new(phase_tx: watch::Sender<SessionPhase>) -> Self {
        Self {
            phase_tx,
            event_tx: None,
            session_id: uuid::Uuid::new_v4().to_string(),
            resume_handle: parking_lot::Mutex::new(None),
            turns: parking_lot::Mutex::new(Vec::new()),
            current_turn: parking_lot::Mutex::new(None),
        }
    }

    pub fn with_events(
        phase_tx: watch::Sender<SessionPhase>,
        event_tx: broadcast::Sender<SessionEvent>,
    ) -> Self {
        Self {
            phase_tx,
            event_tx: Some(event_tx),
            session_id: uuid::Uuid::new_v4().to_string(),
            resume_handle: parking_lot::Mutex::new(None),
            turns: parking_lot::Mutex::new(Vec::new()),
            current_turn: parking_lot::Mutex::new(None),
        }
    }

    pub fn transition_to(&self, to: SessionPhase) -> Result<SessionPhase, SessionError> {
        let from = self.phase();
        if !from.can_transition_to(&to) {
            return Err(SessionError::InvalidTransition { from, to });
        }
        self.phase_tx.send_replace(to);
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(SessionEvent::PhaseChanged(to));
        }
        Ok(to)
    }
}
```

Update `connect()` in `connection.rs` to use `SessionState::with_events(phase_tx, event_tx.clone())`.

**Step 4: Run all tests**

Run: `cargo test --workspace`
Expected: All pass

**Step 5: Commit**

```bash
git add crates/gemini-genai-wire/src/session/mod.rs crates/gemini-genai-wire/src/transport/connection.rs
git commit -m "feat(wire): emit PhaseChanged events on state transitions"
```

---

## Phase 2: Trait Boundaries

### Task 4: Extract Codec trait and JsonCodec

**Files:**
- Create: `crates/gemini-genai-wire/src/transport/codec.rs`
- Modify: `crates/gemini-genai-wire/src/transport/mod.rs`
- Modify: `crates/gemini-genai-wire/src/transport/connection.rs` (extract inline serialization)
- Test: `crates/gemini-genai-wire/src/transport/codec.rs`

**Step 1: Write failing tests for Codec trait**

In new file `crates/gemini-genai-wire/src/transport/codec.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::*;
    use crate::session::SessionCommand;

    #[test]
    fn json_codec_encode_setup() {
        let codec = JsonCodec;
        let config = SessionConfig::new("test-key")
            .model(GeminiModel::Gemini2_0FlashLive);
        let bytes = codec.encode_setup(&config).unwrap();
        let json = String::from_utf8(bytes).unwrap();
        assert!(json.contains("\"setup\""));
        assert!(json.contains("gemini-2.0-flash-live"));
    }

    #[test]
    fn json_codec_encode_send_text() {
        let codec = JsonCodec;
        let config = SessionConfig::new("test-key");
        let cmd = SessionCommand::SendText("hello".into());
        let bytes = codec.encode_command(&cmd, &config).unwrap();
        let json = String::from_utf8(bytes).unwrap();
        assert!(json.contains("\"clientContent\""));
        assert!(json.contains("hello"));
    }

    #[test]
    fn json_codec_encode_send_audio() {
        let codec = JsonCodec;
        let config = SessionConfig::new("test-key");
        let cmd = SessionCommand::SendAudio(vec![1, 2, 3, 4]);
        let bytes = codec.encode_command(&cmd, &config).unwrap();
        let json = String::from_utf8(bytes).unwrap();
        assert!(json.contains("\"realtimeInput\""));
        assert!(json.contains("\"audio\""));
    }

    #[test]
    fn json_codec_decode_setup_complete() {
        let codec = JsonCodec;
        let input = br#"{"setupComplete":{}}"#;
        let msg = codec.decode_message(input).unwrap();
        assert!(matches!(msg, ServerMessage::SetupComplete(_)));
    }

    #[test]
    fn json_codec_decode_server_content() {
        let codec = JsonCodec;
        let input = br#"{"serverContent":{"modelTurn":{"parts":[{"text":"hi"}]},"turnComplete":true}}"#;
        let msg = codec.decode_message(input).unwrap();
        assert!(matches!(msg, ServerMessage::ServerContent(_)));
    }

    #[test]
    fn json_codec_decode_invalid_utf8() {
        let codec = JsonCodec;
        let input = &[0xFF, 0xFE];
        let result = codec.decode_message(input);
        assert!(result.is_err());
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p gemini-genai-wire -- json_codec`
Expected: Compilation failure — module doesn't exist

**Step 3: Implement Codec trait and JsonCodec**

```rust
//! Message codec — encode commands, decode server messages.

use crate::protocol::messages::*;
use crate::protocol::types::*;
use crate::session::SessionCommand;

/// Error during encoding or decoding.
#[derive(Debug, thiserror::Error, Clone)]
pub enum CodecError {
    #[error("Serialization error: {0}")]
    Serialize(String),
    #[error("Deserialization error: {0}")]
    Deserialize(String),
    #[error("Invalid UTF-8")]
    InvalidUtf8,
}

/// Encodes client commands into wire bytes and decodes server bytes into messages.
pub trait Codec: Send + Sync + 'static {
    fn encode_setup(&self, config: &SessionConfig) -> Result<Vec<u8>, CodecError>;
    fn encode_command(&self, cmd: &SessionCommand, config: &SessionConfig) -> Result<Vec<u8>, CodecError>;
    fn decode_message(&self, data: &[u8]) -> Result<ServerMessage, CodecError>;
}

/// Default JSON codec — current behavior extracted from connection.rs.
pub struct JsonCodec;

impl Codec for JsonCodec {
    fn encode_setup(&self, config: &SessionConfig) -> Result<Vec<u8>, CodecError> {
        serde_json::to_vec(&config.to_setup_message())
            .map_err(|e| CodecError::Serialize(e.to_string()))
    }

    fn encode_command(&self, cmd: &SessionCommand, config: &SessionConfig) -> Result<Vec<u8>, CodecError> {
        let value = match cmd {
            SessionCommand::SendAudio(data) => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(data);
                serde_json::to_vec(&RealtimeInputMessage {
                    realtime_input: RealtimeInputPayload {
                        media_chunks: Vec::new(),
                        audio: Some(Blob {
                            mime_type: config.input_audio_format.mime_type().to_string(),
                            data: encoded,
                        }),
                        video: None,
                        audio_stream_end: None,
                        text: None,
                    },
                })
            }
            SessionCommand::SendText(text) => {
                serde_json::to_vec(&ClientContentMessage {
                    client_content: ClientContentPayload {
                        turns: vec![Content::user(text)],
                        turn_complete: Some(true),
                    },
                })
            }
            SessionCommand::SendToolResponse(responses) => {
                serde_json::to_vec(&ToolResponseMessage {
                    tool_response: ToolResponsePayload {
                        function_responses: responses.clone(),
                    },
                })
            }
            SessionCommand::ActivityStart => {
                serde_json::to_vec(&ActivitySignalMessage {
                    realtime_input: ActivitySignalPayload {
                        activity_start: Some(ActivityStart {}),
                        activity_end: None,
                    },
                })
            }
            SessionCommand::ActivityEnd => {
                serde_json::to_vec(&ActivitySignalMessage {
                    realtime_input: ActivitySignalPayload {
                        activity_start: None,
                        activity_end: Some(ActivityEnd {}),
                    },
                })
            }
            SessionCommand::SendClientContent { turns, turn_complete } => {
                serde_json::to_vec(&ClientContentMessage {
                    client_content: ClientContentPayload {
                        turns: turns.clone(),
                        turn_complete: Some(*turn_complete),
                    },
                })
            }
            SessionCommand::Disconnect => return Ok(Vec::new()), // Handled at transport level
        };
        value.map_err(|e| CodecError::Serialize(e.to_string()))
    }

    fn decode_message(&self, data: &[u8]) -> Result<ServerMessage, CodecError> {
        let text = std::str::from_utf8(data).map_err(|_| CodecError::InvalidUtf8)?;
        ServerMessage::parse(text).map_err(|e| CodecError::Deserialize(e.to_string()))
    }
}
```

**Step 4: Wire codec into transport/mod.rs exports**

Add `pub mod codec;` and `pub use codec::{Codec, JsonCodec, CodecError};` to `transport/mod.rs`.

**Step 5: Run all tests**

Run: `cargo test --workspace`
Expected: All pass

**Step 6: Commit**

```bash
git add crates/gemini-genai-wire/src/transport/codec.rs crates/gemini-genai-wire/src/transport/mod.rs
git commit -m "feat(wire): extract Codec trait and JsonCodec from connection.rs"
```

---

### Task 5: Extract Transport trait and TungsteniteTransport

**Files:**
- Create: `crates/gemini-genai-wire/src/transport/ws.rs`
- Modify: `crates/gemini-genai-wire/src/transport/mod.rs`
- Test: `crates/gemini-genai-wire/src/transport/ws.rs`

**Step 1: Write failing tests for Transport trait**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_transport_round_trip() {
        // MockTransport records sent data and replays scripted responses
        let mut transport = MockTransport::new();
        transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            transport.connect("wss://example.com", Default::default()).await.unwrap();
            transport.send(b"hello".to_vec()).await.unwrap();
            let data = transport.recv().await.unwrap();
            assert!(data.is_some());
            assert!(String::from_utf8(data.unwrap()).unwrap().contains("setupComplete"));
        });
    }

    #[test]
    fn mock_transport_records_sent() {
        let mut transport = MockTransport::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            transport.connect("wss://example.com", Default::default()).await.unwrap();
            transport.send(b"msg1".to_vec()).await.unwrap();
            transport.send(b"msg2".to_vec()).await.unwrap();
        });
        let sent = transport.take_sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0], b"msg1");
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p gemini-genai-wire -- mock_transport`
Expected: Compilation failure

**Step 3: Implement Transport trait, TungsteniteTransport, and MockTransport**

```rust
//! Transport abstraction — bidirectional message transport.

use async_trait::async_trait;
use tokio_tungstenite::tungstenite::http::HeaderMap;

/// A bidirectional message transport.
/// WebSocket is the default; mock transports enable unit testing.
#[async_trait]
pub trait Transport: Send + 'static {
    type Error: std::error::Error + Send + Sync + 'static;

    async fn connect(&mut self, url: &str, headers: HeaderMap) -> Result<(), Self::Error>;
    async fn send(&mut self, data: Vec<u8>) -> Result<(), Self::Error>;
    async fn recv(&mut self) -> Result<Option<Vec<u8>>, Self::Error>;
    async fn close(&mut self) -> Result<(), Self::Error>;
}

/// WebSocket transport using tokio-tungstenite (the current default).
pub struct TungsteniteTransport { /* ... wraps split sink/stream */ }

/// Mock transport for unit tests — records sent data, replays scripted responses.
#[cfg(any(test, feature = "test-utils"))]
pub struct MockTransport {
    sent: Vec<Vec<u8>>,
    recv_queue: std::collections::VecDeque<Vec<u8>>,
    connected: bool,
}
```

Full implementation for `TungsteniteTransport` wraps the existing `tokio_tungstenite::connect_async` + `SplitSink`/`SplitStream` pattern from `connection.rs`. Handles Binary-vs-Text frame conversion (Vertex AI binary frame fix) inside `recv()`.

`MockTransport` stores sent messages in a `Vec<Vec<u8>>` and returns scripted messages from a `VecDeque`.

**Step 4: Run all tests**

Run: `cargo test --workspace`
Expected: All pass

**Step 5: Commit**

```bash
git add crates/gemini-genai-wire/src/transport/ws.rs crates/gemini-genai-wire/src/transport/mod.rs
git commit -m "feat(wire): extract Transport trait, TungsteniteTransport, MockTransport"
```

---

### Task 6: SessionWriter and SessionReader traits

**Files:**
- Modify: `crates/gemini-genai-wire/src/session/mod.rs:270-395` (SessionHandle)
- Modify: `crates/gemini-genai-wire/src/lib.rs` (prelude exports)
- Test: `crates/gemini-genai-wire/src/session/mod.rs`

**Step 1: Write failing tests**

```rust
#[test]
fn session_handle_implements_session_writer() {
    fn assert_impl<T: SessionWriter>() {}
    assert_impl::<SessionHandle>();
}

#[test]
fn session_handle_implements_session_reader() {
    fn assert_impl<T: SessionReader>() {}
    assert_impl::<SessionHandle>();
}

#[test]
fn session_writer_is_object_safe() {
    fn _assert(_: &dyn SessionWriter) {}
}

#[test]
fn session_reader_is_object_safe() {
    fn _assert(_: &dyn SessionReader) {}
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p gemini-genai-wire -- session_handle_implements session_writer_is session_reader_is`
Expected: Compilation failure

**Step 3: Implement traits**

```rust
/// Write-side of a session — send commands without owning the full handle.
#[async_trait::async_trait]
pub trait SessionWriter: Send + Sync + 'static {
    async fn send_audio(&self, data: Vec<u8>) -> Result<(), SessionError>;
    async fn send_text(&self, text: String) -> Result<(), SessionError>;
    async fn send_tool_response(&self, responses: Vec<FunctionResponse>) -> Result<(), SessionError>;
    async fn send_client_content(&self, turns: Vec<Content>, turn_complete: bool) -> Result<(), SessionError>;
    async fn signal_activity_start(&self) -> Result<(), SessionError>;
    async fn signal_activity_end(&self) -> Result<(), SessionError>;
    async fn disconnect(&self) -> Result<(), SessionError>;
}

/// Read-side of a session — subscribe to events and observe phase.
pub trait SessionReader: Send + Sync + 'static {
    fn subscribe(&self) -> broadcast::Receiver<SessionEvent>;
    fn phase(&self) -> SessionPhase;
    fn session_id(&self) -> &str;
}
```

Implement both traits for `SessionHandle` — delegates to existing methods. `send_text` signature changes from `impl Into<String>` to `String` for object safety.

Add `async-trait` to wire Cargo.toml dependencies.

**Step 4: Update prelude to export traits**

In `lib.rs` prelude, add: `pub use crate::session::{SessionWriter, SessionReader};`

**Step 5: Run all tests**

Run: `cargo test --workspace`
Expected: All pass

**Step 6: Commit**

```bash
git add crates/gemini-genai-wire/src/session/mod.rs crates/gemini-genai-wire/src/lib.rs crates/gemini-genai-wire/Cargo.toml
git commit -m "feat(wire): add SessionWriter and SessionReader traits"
```

---

### Task 7: Refactor connection.rs to use Codec + Transport

**Files:**
- Modify: `crates/gemini-genai-wire/src/transport/connection.rs` (the big refactor)
- Modify: `crates/gemini-genai-wire/src/transport/mod.rs` (update connect signature)
- Test: existing tests + new unit tests using MockTransport

**Step 1: Write failing test with MockTransport**

```rust
#[tokio::test]
async fn connect_with_mock_transport_and_codec() {
    let mut transport = MockTransport::new();
    // Script setupComplete response
    transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());
    // Script a server content then close
    transport.script_recv(br#"{"serverContent":{"modelTurn":{"parts":[{"text":"hello"}]},"turnComplete":true}}"#.to_vec());

    let config = SessionConfig::new("test-key")
        .model(GeminiModel::Gemini2_0FlashLive);

    let handle = connect_with(config, TransportConfig::default(), transport, JsonCodec).await.unwrap();
    let mut events = handle.subscribe();
    handle.wait_for_phase(SessionPhase::Active).await;

    // Should receive Connected event
    // ... assert events
}
```

**Step 2: Run test to verify it fails**

Expected: `connect_with` doesn't exist

**Step 3: Implement `connect_with` function**

Refactor `connection.rs` to accept generic `transport: T` and `codec: C`:

```rust
pub async fn connect_with<T: Transport, C: Codec>(
    config: SessionConfig,
    transport_config: TransportConfig,
    transport: T,
    codec: C,
) -> Result<SessionHandle, SessionError> { /* ... */ }
```

The existing `connect()` function becomes a thin wrapper:

```rust
pub async fn connect(
    config: SessionConfig,
    transport_config: TransportConfig,
) -> Result<SessionHandle, SessionError> {
    connect_with(config, transport_config, TungsteniteTransport::new(), JsonCodec).await
}
```

Refactor `connection_loop`, `establish_connection`, and `run_session` to use `codec.encode_command()` / `codec.decode_message()` / `transport.send()` / `transport.recv()` instead of inline serialization and `ws_write.send(Message::Text(...))`.

**Step 4: Run all tests**

Run: `cargo test --workspace`
Expected: All pass

**Step 5: Commit**

```bash
git add crates/gemini-genai-wire/src/transport/connection.rs crates/gemini-genai-wire/src/transport/mod.rs
git commit -m "refactor(wire): connection.rs uses Codec + Transport traits"
```

---

### Task 8: Update runtime AgentSession to use SessionWriter trait

**Files:**
- Modify: `crates/gemini-genai-runtime/src/agent_session.rs:39-47` (AgentSession struct)
- Test: `crates/gemini-genai-runtime/src/agent_session.rs` (existing tests)

**Step 1: Write failing test**

```rust
#[tokio::test]
async fn agent_session_accepts_dyn_session_writer() {
    let handle = mock_session_handle();
    let writer: Arc<dyn SessionWriter> = Arc::new(handle.clone());
    // AgentSession should accept Arc<dyn SessionWriter>
    let session = AgentSession::from_writer(writer, handle.subscribe());
    assert_eq!(session.input_subscriber_count(), 0);
}
```

**Step 2: Run test**

Expected: Fail — `AgentSession::from_writer` doesn't exist

**Step 3: Implement**

Add `from_writer` constructor that accepts `Arc<dyn SessionWriter>`:

```rust
impl AgentSession {
    /// Create from a trait-object writer (enables mock testing and middleware injection).
    pub fn from_writer(
        writer: Arc<dyn SessionWriter>,
        events: broadcast::Receiver<SessionEvent>,
    ) -> Self {
        let (input_broadcast, _) = broadcast::channel(256);
        Self {
            writer,
            input_broadcast,
            state: State::new(),
        }
    }
}
```

Change `session: SessionHandle` field to `writer: Arc<dyn SessionWriter>`. Update existing `new()` to wrap `SessionHandle` in `Arc`. Update all method delegations from `self.session.send_audio()` to `self.writer.send_audio()`.

The `wire()` accessor that returns `&SessionHandle` can be kept by also storing an optional `SessionHandle` reference, or removed in favor of trait-only access.

**Step 4: Run all tests**

Run: `cargo test --workspace`
Expected: All pass

**Step 5: Commit**

```bash
git add crates/gemini-genai-runtime/src/agent_session.rs
git commit -m "refactor(runtime): AgentSession uses Arc<dyn SessionWriter>"
```

---

## Phase 3: Auth Strategy + SDK Parity

### Task 9: AuthProvider trait and built-in implementations

**Files:**
- Create: `crates/gemini-genai-wire/src/transport/auth.rs`
- Modify: `crates/gemini-genai-wire/src/transport/mod.rs`
- Modify: `crates/gemini-genai-wire/src/transport/connection.rs` (use AuthProvider)
- Test: `crates/gemini-genai-wire/src/transport/auth.rs`

**Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::GeminiModel;

    #[test]
    fn google_ai_auth_url() {
        let auth = GoogleAIAuth::new("test-key");
        let url = auth.ws_url(&GeminiModel::Gemini2_0FlashLive);
        assert!(url.contains("generativelanguage.googleapis.com"));
        assert!(url.contains("key=test-key"));
    }

    #[test]
    fn vertex_ai_auth_url_regional() {
        let auth = VertexAIAuth::new("my-project", "us-central1", "token123".into());
        let url = auth.ws_url(&GeminiModel::Gemini2_0FlashLive);
        assert!(url.contains("us-central1-aiplatform.googleapis.com"));
        assert!(url.contains("v1beta1"));
    }

    #[test]
    fn vertex_ai_auth_url_global() {
        let auth = VertexAIAuth::new("my-project", "global", "token123".into());
        let url = auth.ws_url(&GeminiModel::Gemini2_0FlashLive);
        assert!(url.starts_with("wss://aiplatform.googleapis.com"));
        assert!(!url.contains("global-aiplatform"));
    }

    #[tokio::test]
    async fn vertex_ai_auth_headers() {
        let auth = VertexAIAuth::new("my-project", "us-central1", "mytoken".into());
        let headers = auth.auth_headers().await.unwrap();
        let bearer = headers.get("Authorization").unwrap().to_str().unwrap();
        assert_eq!(bearer, "Bearer mytoken");
    }

    #[test]
    fn google_ai_auth_query_params() {
        let auth = GoogleAIAuth::new("my-api-key");
        let params = auth.query_params();
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].0, "key");
        assert_eq!(params[0].1, "my-api-key");
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p gemini-genai-wire -- google_ai_auth vertex_ai_auth`
Expected: Compilation failure

**Step 3: Implement AuthProvider trait**

```rust
//! Authentication providers for Gemini API connections.

use async_trait::async_trait;
use tokio_tungstenite::tungstenite::http::HeaderMap;

use crate::protocol::types::GeminiModel;
use crate::session::AuthError;

/// Provides authentication credentials and URL construction.
#[async_trait]
pub trait AuthProvider: Send + Sync + 'static {
    /// Build the WebSocket URL for the given model.
    fn ws_url(&self, model: &GeminiModel) -> String;

    /// HTTP headers for the WebSocket upgrade request.
    async fn auth_headers(&self) -> Result<HeaderMap, AuthError>;

    /// Query params to append to the URL (e.g., API key).
    fn query_params(&self) -> Vec<(String, String)> { vec![] }

    /// Called on auth failure to allow token refresh. Default: no-op.
    async fn refresh(&self) -> Result<(), AuthError> { Ok(()) }
}

pub struct GoogleAIAuth { api_key: String }
pub struct GoogleAITokenAuth { access_token: String }
pub struct VertexAIAuth { project: String, location: String, token: parking_lot::Mutex<String> }
```

Implement each: move URL construction logic from `SessionConfig::ws_url()` into the auth providers. `SessionConfig` will still have `ws_url()` for backward compat but it delegates to the auth provider internally.

**Step 4: Run all tests**

Run: `cargo test --workspace`
Expected: All pass

**Step 5: Commit**

```bash
git add crates/gemini-genai-wire/src/transport/auth.rs crates/gemini-genai-wire/src/transport/mod.rs
git commit -m "feat(wire): add AuthProvider trait with GoogleAI/VertexAI implementations"
```

---

### Task 10: Platform abstraction

**Files:**
- Create: `crates/gemini-genai-wire/src/protocol/platform.rs`
- Modify: `crates/gemini-genai-wire/src/protocol/mod.rs`
- Test: `crates/gemini-genai-wire/src/protocol/platform.rs`

**Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::GeminiModel;

    #[test]
    fn google_ai_api_version() {
        assert_eq!(Platform::GoogleAI.api_version(), "v1beta");
    }

    #[test]
    fn vertex_ai_api_version() {
        let p = Platform::VertexAI { project: "p".into(), location: "us-central1".into() };
        assert_eq!(p.api_version(), "v1beta1");
    }

    #[test]
    fn vertex_ai_base_host_regional() {
        let p = Platform::VertexAI { project: "p".into(), location: "us-central1".into() };
        assert_eq!(p.base_host(), "us-central1-aiplatform.googleapis.com");
    }

    #[test]
    fn vertex_ai_base_host_global() {
        let p = Platform::VertexAI { project: "p".into(), location: "global".into() };
        assert_eq!(p.base_host(), "aiplatform.googleapis.com");
    }

    #[test]
    fn google_ai_model_uri() {
        let uri = Platform::GoogleAI.model_uri(&GeminiModel::Gemini2_0FlashLive);
        assert_eq!(uri, "models/gemini-2.0-flash-live-001");
    }

    #[test]
    fn vertex_ai_model_uri() {
        let p = Platform::VertexAI { project: "my-proj".into(), location: "us-central1".into() };
        let uri = p.model_uri(&GeminiModel::Gemini2_0FlashLive);
        assert!(uri.contains("publishers/google/models/gemini-2.0-flash-live-001"));
    }
}
```

**Step 2: Run tests → fail**

**Step 3: Implement**

```rust
//! Platform abstraction — Google AI vs Vertex AI URL/version logic.

use crate::protocol::types::GeminiModel;

/// Which platform variant to use.
pub enum Platform {
    GoogleAI,
    VertexAI { project: String, location: String },
}

impl Platform {
    pub fn base_host(&self) -> String {
        match self {
            Platform::GoogleAI => "generativelanguage.googleapis.com".to_string(),
            Platform::VertexAI { location, .. } => {
                if location == "global" {
                    "aiplatform.googleapis.com".to_string()
                } else {
                    format!("{location}-aiplatform.googleapis.com")
                }
            }
        }
    }

    pub fn api_version(&self) -> &str {
        match self {
            Platform::GoogleAI => "v1beta",
            Platform::VertexAI { .. } => "v1beta1",
        }
    }

    pub fn model_uri(&self, model: &GeminiModel) -> String {
        match self {
            Platform::GoogleAI => model.to_string(), // "models/..."
            Platform::VertexAI { project, location } => {
                format!(
                    "projects/{project}/locations/{location}/publishers/google/models/{}",
                    model.to_string().trim_start_matches("models/")
                )
            }
        }
    }

    pub fn ws_path(&self) -> &str {
        match self {
            Platform::GoogleAI => "google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent",
            Platform::VertexAI { .. } => "google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent",
        }
    }
}
```

**Step 4: Run all tests**

Run: `cargo test --workspace`

**Step 5: Commit**

```bash
git add crates/gemini-genai-wire/src/protocol/platform.rs crates/gemini-genai-wire/src/protocol/mod.rs
git commit -m "feat(wire): add Platform abstraction for Google AI / Vertex AI URL logic"
```

---

### Task 11: ToolProvider trait

**Files:**
- Add trait to: `crates/gemini-genai-wire/src/protocol/types.rs`
- Modify: `crates/gemini-genai-runtime/src/tool.rs:169-192` (implement for ToolDispatcher)
- Test: both crates

**Step 1: Write failing tests**

In wire `types.rs` test module:
```rust
#[test]
fn vec_tool_implements_tool_provider() {
    fn assert_impl<T: ToolProvider>() {}
    assert_impl::<Vec<Tool>>();
}
```

In runtime `tool.rs` test module:
```rust
#[test]
fn tool_dispatcher_implements_tool_provider() {
    fn assert_impl<T: gemini_genai_rs_wire::prelude::ToolProvider>() {}
    assert_impl::<ToolDispatcher>();
}
```

**Step 2: Run tests → fail**

**Step 3: Implement**

In `types.rs`:
```rust
/// Declares tools for a Gemini session setup message.
pub trait ToolProvider: Send + Sync + 'static {
    fn declarations(&self) -> Vec<Tool>;
}

impl ToolProvider for Vec<Tool> {
    fn declarations(&self) -> Vec<Tool> {
        self.clone()
    }
}
```

In runtime `tool.rs`:
```rust
impl gemini_genai_rs_wire::prelude::ToolProvider for ToolDispatcher {
    fn declarations(&self) -> Vec<Tool> {
        self.to_tool_declarations()
    }
}
```

**Step 4: Run all tests**

Run: `cargo test --workspace`

**Step 5: Commit**

```bash
git add crates/gemini-genai-wire/src/protocol/types.rs crates/gemini-genai-runtime/src/tool.rs
git commit -m "feat(wire): add ToolProvider trait, implement for Vec<Tool> and ToolDispatcher"
```

---

### Task 12: ConnectBuilder for advanced configuration

**Files:**
- Create: `crates/gemini-genai-wire/src/transport/builder.rs`
- Modify: `crates/gemini-genai-wire/src/transport/mod.rs`
- Test: `crates/gemini-genai-wire/src/transport/builder.rs`

**Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::*;

    #[test]
    fn builder_default_compiles() {
        let config = SessionConfig::new("key")
            .model(GeminiModel::Gemini2_0FlashLive);
        let _builder = ConnectBuilder::new(config);
    }

    #[test]
    fn builder_with_custom_transport_config() {
        let config = SessionConfig::new("key");
        let _builder = ConnectBuilder::new(config)
            .transport_config(TransportConfig {
                connect_timeout_secs: 30,
                ..Default::default()
            });
    }
}
```

**Step 2: Implement ConnectBuilder**

```rust
pub struct ConnectBuilder<T = TungsteniteTransport, C = JsonCodec> {
    config: SessionConfig,
    transport_config: TransportConfig,
    transport: T,
    codec: C,
}

impl ConnectBuilder {
    pub fn new(config: SessionConfig) -> Self { /* defaults */ }
}

impl<T: Transport, C: Codec> ConnectBuilder<T, C> {
    pub fn transport_config(mut self, tc: TransportConfig) -> Self { ... }
    pub fn transport<T2: Transport>(self, t: T2) -> ConnectBuilder<T2, C> { ... }
    pub fn codec<C2: Codec>(self, c: C2) -> ConnectBuilder<T, C2> { ... }
    pub async fn build(self) -> Result<SessionHandle, SessionError> {
        connect_with(self.config, self.transport_config, self.transport, self.codec).await
    }
}
```

**Step 3: Run all tests**

Run: `cargo test --workspace`

**Step 4: Commit**

```bash
git add crates/gemini-genai-wire/src/transport/builder.rs crates/gemini-genai-wire/src/transport/mod.rs
git commit -m "feat(wire): add ConnectBuilder for advanced transport/codec configuration"
```

---

### Task 13: Update prelude and integration test

**Files:**
- Modify: `crates/gemini-genai-wire/src/lib.rs` (prelude additions)
- Modify: `crates/gemini-genai-wire/src/transport/mod.rs`
- Test: integration test verifying all exports

**Step 1: Write integration test**

```rust
#[test]
fn prelude_exports_all_new_types() {
    // Traits
    fn _codec<T: gemini_genai_rs_wire::prelude::Codec>() {}
    fn _transport<T: gemini_genai_rs_wire::prelude::Transport>() {}
    fn _writer<T: gemini_genai_rs_wire::prelude::SessionWriter>() {}
    fn _reader<T: gemini_genai_rs_wire::prelude::SessionReader>() {}
    fn _tool_provider<T: gemini_genai_rs_wire::prelude::ToolProvider>() {}

    // Implementations
    let _ = gemini_genai_rs_wire::prelude::JsonCodec;

    // Error types
    fn _err1(_: gemini_genai_rs_wire::prelude::WebSocketError) {}
    fn _err2(_: gemini_genai_rs_wire::prelude::SetupError) {}
    fn _err3(_: gemini_genai_rs_wire::prelude::AuthError) {}
    fn _err4(_: gemini_genai_rs_wire::prelude::CodecError) {}

    // Builders
    fn _role(_: gemini_genai_rs_wire::prelude::Role) {}
    fn _platform(_: gemini_genai_rs_wire::prelude::Platform) {}
}
```

**Step 2: Update prelude exports**

Add to `lib.rs` prelude:
```rust
// Traits
pub use crate::session::{SessionWriter, SessionReader};
pub use crate::transport::{Codec, JsonCodec, CodecError};
pub use crate::transport::ws::{Transport, TungsteniteTransport};
pub use crate::transport::auth::{AuthProvider, GoogleAIAuth, VertexAIAuth};
pub use crate::transport::builder::ConnectBuilder;

// Type safety
pub use crate::protocol::types::{Role, ToolProvider};
pub use crate::protocol::platform::Platform;

// Structured errors
pub use crate::session::{WebSocketError, SetupError, AuthError};
```

**Step 3: Run all tests**

Run: `cargo test --workspace`
Expected: All pass — full integration verified

**Step 4: Commit**

```bash
git add crates/gemini-genai-wire/src/lib.rs
git commit -m "feat(wire): update prelude with all new trait and type exports"
```

---

### Task 14: Final workspace-wide verification

**Step 1: Run full test suite**

Run: `cargo test --workspace`
Expected: All tests pass across all 3 crates

**Step 2: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: No warnings

**Step 3: Run doc tests**

Run: `cargo doc --workspace --no-deps`
Expected: Clean documentation build

**Step 4: Verify examples compile**

Run: `cargo build --examples -p gemini-genai-wire`
Expected: All examples compile

**Step 5: Commit any final fixes**

```bash
git add -A
git commit -m "chore: final cleanup for wire crate refactor"
```
