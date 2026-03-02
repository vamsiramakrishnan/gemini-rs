# Layered ADK Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Restructure gemini-live-rs from a monolithic crate into a three-layer workspace (wire + runtime + fluent DX) with PyO3 Python bindings, implementing the ADK-equivalent agent runtime and adk-fluent-style composition algebra.

**Architecture:** Three crates in a Cargo workspace — `gemini-live-wire` (Layer 0: raw protocol + transport), `gemini-live-runtime` (Layer 1: agent runtime with AgentSession, streaming tools, agent transfer), `gemini-live` (Layer 2: fluent builder API with operator overloading and composition modules). Plus `gemini-live-python` for PyO3 bindings.

**Key Design Decisions (see design doc §10-11):**
- `AgentSession` wraps `SessionHandle` instead of ADK's `LiveRequestQueue` (avoids double-queuing)
- `AgentEvent` wraps `SessionEvent` (no duplicate variants)
- Operators compile directly to `Agent` impls (no IR — single backend)
- `FnTool::typed<T>()` with `schemars` for auto-generated schemas (no proc macro, type-safe args)
- `ToolSpec` enum replacing all-optional `Tool` struct (illegal states unrepresentable)
- `Bytes` (Arc refcount) for audio fan-out instead of `Vec<u8>` clone (zero-copy broadcast)
- `CancellationToken` on all tool calls with configurable timeout
- Tentative barge-in (duck → confirm → flush) to prevent false-positive silence
- `bytemuck::cast_slice` for zero-copy i16↔u8 on audio hot path
- Drop-oldest backpressure for audio send queue

**Tech Stack:** Rust 2021, Tokio 1.x, tokio-tungstenite, serde/serde_json, PyO3 0.23, maturin, DashMap, parking_lot, async-trait, tokio-util (CancellationToken)

**Design Doc:** `docs/plans/2026-03-01-gemini-live-adk-design.md`

---

## Phase 1: Workspace Scaffold + Layer 0 Protocol Fixes

### Task 1: Create workspace structure and move existing code to `gemini-live-wire`

**Files:**
- Create: `Cargo.toml` (workspace root — replaces existing)
- Create: `crates/gemini-live-wire/Cargo.toml`
- Create: `crates/gemini-live-wire/src/lib.rs`
- Move: `src/protocol/` → `crates/gemini-live-wire/src/protocol/`
- Move: `src/transport/` → `crates/gemini-live-wire/src/transport/`
- Move: `src/buffer/` → `crates/gemini-live-wire/src/buffer/`
- Move: `src/vad/` → `crates/gemini-live-wire/src/vad/`
- Move: `src/session/` → `crates/gemini-live-wire/src/session/`
- Move: `src/telemetry/` → `crates/gemini-live-wire/src/telemetry/`
- Move: `src/flow/` → `crates/gemini-live-wire/src/flow/`

**Step 1: Create workspace root Cargo.toml**

```toml
# Cargo.toml (workspace root)
[workspace]
resolver = "2"
members = [
    "crates/gemini-live-wire",
]

[workspace.package]
edition = "2021"
license = "Apache-2.0"
```

**Step 2: Create crates directory and move source files**

```bash
mkdir -p crates/gemini-live-wire/src
# Move core wire modules
cp -r src/protocol crates/gemini-live-wire/src/
cp -r src/transport crates/gemini-live-wire/src/
cp -r src/buffer crates/gemini-live-wire/src/
cp -r src/vad crates/gemini-live-wire/src/
cp -r src/session crates/gemini-live-wire/src/
cp -r src/telemetry crates/gemini-live-wire/src/
cp -r src/flow crates/gemini-live-wire/src/
```

**Step 3: Create `crates/gemini-live-wire/Cargo.toml`**

```toml
[package]
name = "gemini-live-wire"
version = "0.1.0"
edition.workspace = true
license.workspace = true
description = "Raw wire protocol and transport for the Gemini Multimodal Live API"

[features]
default = ["vad", "tracing-support"]
vad = []
opus = ["dep:audiopus"]
tracing-support = ["dep:tracing", "dep:tracing-subscriber"]
metrics = ["dep:metrics", "dep:metrics-exporter-prometheus"]

[dependencies]
tokio = { version = "1", features = ["full"] }
tokio-tungstenite = { version = "0.24", features = ["native-tls"] }
futures-util = "0.3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
base64 = "0.22"
thiserror = "2"
uuid = { version = "1", features = ["v4"] }
bytes = "1"
parking_lot = "0.12"
dashmap = "6"
arc-swap = "1"
url = "2"
audiopus = { version = "0.3.0-rc.0", optional = true }
tracing = { version = "0.1", optional = true }
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"], optional = true }
metrics = { version = "0.24", optional = true }
metrics-exporter-prometheus = { version = "0.16", optional = true }

[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }
proptest = "1"
tokio-test = "0.4"
```

**Step 4: Create `crates/gemini-live-wire/src/lib.rs`**

```rust
//! # gemini-live-wire
//!
//! Raw wire protocol and transport for the Gemini Multimodal Live API.
//! This crate provides zero-abstraction access to the Gemini Live WebSocket API.

pub mod protocol;
pub mod transport;
pub mod buffer;
#[cfg(feature = "vad")]
pub mod vad;
pub mod session;
pub mod flow;
pub mod telemetry;

/// Convenient re-exports for wire-level usage.
pub mod prelude {
    pub use crate::protocol::types::*;
    pub use crate::protocol::messages::*;
    pub use crate::transport::{connect, TransportConfig};
    pub use crate::session::{
        SessionCommand, SessionError, SessionEvent, SessionHandle, SessionPhase,
    };
    pub use crate::buffer::{AudioJitterBuffer, JitterConfig, SpscRing};
    #[cfg(feature = "vad")]
    pub use crate::vad::{VadConfig, VadEvent, VoiceActivityDetector};
}
```

**Step 5: Fix all `use crate::` paths in moved files**

Every file in `crates/gemini-live-wire/src/` that references `use crate::protocol`, `use crate::session`, etc. stays unchanged since they're now within the same crate. The key change is removing any references to modules that did NOT move (`app`, `call`, `client`, `context`, `prompt`, `state`, `pipeline`, `agent`).

Grep for any cross-references:
```bash
grep -r "use crate::" crates/gemini-live-wire/src/ | grep -E "(app|call|client|context|prompt|state::ConversationState|pipeline|agent)"
```

Fix any found references (these modules belong to higher layers).

**Step 6: Verify it compiles**

```bash
cd crates/gemini-live-wire && cargo check
```

Expected: Compiles clean. All existing tests in `protocol/messages.rs` and `protocol/types.rs` pass.

**Step 7: Run existing tests**

```bash
cd crates/gemini-live-wire && cargo test
```

Expected: All existing unit tests pass.

**Step 8: Commit**

```bash
git add -A
git commit -m "refactor: extract gemini-live-wire crate from monolith"
```

---

### Task 2: Fix `Tool` type — add built-in tools (urlContext, googleSearch, codeExecution)

**Files:**
- Modify: `crates/gemini-live-wire/src/protocol/types.rs`
- Modify: `crates/gemini-live-wire/src/protocol/messages.rs`
- Test: inline in `types.rs` and `messages.rs`

**Step 1: Write failing tests for the new Tool type**

Add to `crates/gemini-live-wire/src/protocol/types.rs` tests:

```rust
#[test]
fn tool_url_context_serialization() {
    let tool = Tool::url_context();
    let json = serde_json::to_string(&tool).unwrap();
    assert!(json.contains("\"urlContext\""));
    assert!(!json.contains("\"functionDeclarations\""));
    assert!(!json.contains("\"googleSearch\""));
}

#[test]
fn tool_google_search_serialization() {
    let tool = Tool::google_search();
    let json = serde_json::to_string(&tool).unwrap();
    assert!(json.contains("\"googleSearch\""));
}

#[test]
fn tool_code_execution_serialization() {
    let tool = Tool::code_execution();
    let json = serde_json::to_string(&tool).unwrap();
    assert!(json.contains("\"codeExecution\""));
}

#[test]
fn tool_function_declarations_serialization() {
    let tool = Tool::functions(vec![FunctionDeclaration {
        name: "get_weather".to_string(),
        description: "Get weather".to_string(),
        parameters: None,
    }]);
    let json = serde_json::to_string(&tool).unwrap();
    assert!(json.contains("\"functionDeclarations\""));
    assert!(json.contains("\"get_weather\""));
}

#[test]
fn tool_mixed_not_allowed() {
    // Each Tool object should have exactly one field set
    let tool = Tool::url_context();
    let json = serde_json::to_string(&tool).unwrap();
    // url_context is an empty object
    assert_eq!(json, r#"{"urlContext":{}}"#);
}
```

**Step 2: Run tests to verify they fail**

```bash
cd crates/gemini-live-wire && cargo test tool_url_context
```

Expected: FAIL — `Tool` type doesn't exist yet.

**Step 3: Implement the new `Tool` type**

In `crates/gemini-live-wire/src/protocol/types.rs`, add the new type and replace `ToolDeclaration`:

```rust
// Replace the existing ToolDeclaration with:

/// A tool declaration sent in the setup message.
/// Each Tool object can contain one of: function declarations, urlContext,
/// googleSearch, codeExecution, or googleSearchRetrieval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_declarations: Option<Vec<FunctionDeclaration>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url_context: Option<UrlContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub google_search: Option<GoogleSearch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_execution: Option<ToolCodeExecution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub google_search_retrieval: Option<GoogleSearchRetrieval>,
}

impl Tool {
    /// Create a tool with function declarations.
    pub fn functions(declarations: Vec<FunctionDeclaration>) -> Self {
        Self {
            function_declarations: Some(declarations),
            url_context: None,
            google_search: None,
            code_execution: None,
            google_search_retrieval: None,
        }
    }

    /// Create a URL context tool (enables the model to fetch and use web content).
    pub fn url_context() -> Self {
        Self {
            function_declarations: None,
            url_context: Some(UrlContext {}),
            google_search: None,
            code_execution: None,
            google_search_retrieval: None,
        }
    }

    /// Create a Google Search tool (enables grounded search).
    pub fn google_search() -> Self {
        Self {
            function_declarations: None,
            url_context: None,
            google_search: Some(GoogleSearch {}),
            code_execution: None,
            google_search_retrieval: None,
        }
    }

    /// Create a code execution tool.
    pub fn code_execution() -> Self {
        Self {
            function_declarations: None,
            url_context: None,
            google_search: None,
            code_execution: Some(ToolCodeExecution {}),
            google_search_retrieval: None,
        }
    }
}

/// URL context tool configuration (empty — no options).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlContext {}

/// Google Search tool configuration (empty — no options).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleSearch {}

/// Code execution tool configuration (empty — no options).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCodeExecution {}

/// Google Search retrieval tool configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleSearchRetrieval {}

// Keep ToolDeclaration as a type alias for backward compatibility
/// Deprecated: use `Tool::functions()` instead.
pub type ToolDeclaration = Tool;
```

**Step 4: Update `SetupPayload` and `SessionConfig` to use `Tool`**

In `messages.rs`, change `SetupPayload`:
```rust
pub tools: Vec<Tool>,  // was Vec<ToolDeclaration>
```

In `types.rs`, change `SessionConfig`:
```rust
pub tools: Vec<Tool>,  // was Vec<ToolDeclaration>
```

Update the `add_tool` builder method:
```rust
pub fn add_tool(mut self, tool: Tool) -> Self {
    self.tools.push(tool);
    self
}
```

Add convenience methods to SessionConfig:
```rust
/// Enable URL context tool.
pub fn url_context(mut self) -> Self {
    self.tools.push(Tool::url_context());
    self
}

/// Enable Google Search grounding.
pub fn google_search(mut self) -> Self {
    self.tools.push(Tool::google_search());
    self
}

/// Enable code execution.
pub fn code_execution(mut self) -> Self {
    self.tools.push(Tool::code_execution());
    self
}
```

**Step 5: Run all tests**

```bash
cd crates/gemini-live-wire && cargo test
```

Expected: ALL tests pass (existing + new).

**Step 6: Commit**

```bash
git add -A
git commit -m "feat(wire): add built-in tool types (urlContext, googleSearch, codeExecution)"
```

---

### Task 3: Add missing GenerationConfig fields (thinkingConfig, affectiveDialog, mediaResolution, seed)

**Files:**
- Modify: `crates/gemini-live-wire/src/protocol/types.rs`

**Step 1: Write failing tests**

```rust
#[test]
fn thinking_config_serialization() {
    let config = SessionConfig::new("key")
        .thinking(1024);
    let json = config.to_setup_json();
    assert!(json.contains("\"thinkingConfig\""));
    assert!(json.contains("\"thinkingBudget\""));
}

#[test]
fn affective_dialog_serialization() {
    let config = SessionConfig::new("key")
        .affective_dialog(true);
    let json = config.to_setup_json();
    assert!(json.contains("\"enableAffectiveDialog\""));
}

#[test]
fn seed_serialization() {
    let config = SessionConfig::new("key")
        .seed(42);
    let json = config.to_setup_json();
    assert!(json.contains("\"seed\""));
}
```

**Step 2: Run tests to verify they fail**

```bash
cd crates/gemini-live-wire && cargo test thinking_config
```

Expected: FAIL

**Step 3: Add the new types and fields**

In `types.rs`:

```rust
/// Configuration for model thinking/reasoning (Gemini 2.5+).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingConfig {
    /// Token budget for thinking/reasoning steps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u32>,
}

/// Media resolution for image/video inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MediaResolution {
    Low,
    Medium,
    High,
}
```

Add to `GenerationConfig`:
```rust
pub struct GenerationConfig {
    // ... existing fields ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<ThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_affective_dialog: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_resolution: Option<MediaResolution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u32>,
}
```

Add builder methods to `SessionConfig`:
```rust
pub fn thinking(mut self, budget: u32) -> Self {
    self.generation_config.thinking_config = Some(ThinkingConfig {
        thinking_budget: Some(budget),
    });
    self
}

pub fn affective_dialog(mut self, enabled: bool) -> Self {
    self.generation_config.enable_affective_dialog = Some(enabled);
    self
}

pub fn seed(mut self, seed: u32) -> Self {
    self.generation_config.seed = Some(seed);
    self
}

pub fn media_resolution(mut self, res: MediaResolution) -> Self {
    self.generation_config.media_resolution = Some(res);
    self
}
```

Update `SessionConfig::from_endpoint` to initialize the new fields to `None`.

**Step 4: Run tests**

```bash
cd crates/gemini-live-wire && cargo test
```

Expected: ALL pass.

**Step 5: Commit**

```bash
git add -A
git commit -m "feat(wire): add thinkingConfig, affectiveDialog, mediaResolution, seed to GenerationConfig"
```

---

### Task 4: Add `send_client_content` to SessionHandle (missing from current API)

The current `SessionHandle` has `send_text()` which wraps content, but no direct `send_client_content()` for sending arbitrary conversation history — needed by the runtime layer.

**Files:**
- Modify: `crates/gemini-live-wire/src/session/mod.rs`
- Modify: `crates/gemini-live-wire/src/transport/connection.rs`

**Step 1: Write failing test**

```rust
#[test]
fn session_command_has_client_content_variant() {
    let content = Content {
        role: Some("user".to_string()),
        parts: vec![Part::Text { text: "hello".to_string() }],
    };
    let cmd = SessionCommand::SendClientContent {
        turns: vec![content],
        turn_complete: true,
    };
    // Just verify the variant compiles
    match cmd {
        SessionCommand::SendClientContent { turns, turn_complete } => {
            assert_eq!(turns.len(), 1);
            assert!(turn_complete);
        }
        _ => panic!("wrong variant"),
    }
}
```

**Step 2: Run test to verify it fails**

```bash
cd crates/gemini-live-wire && cargo test session_command_has_client_content
```

Expected: FAIL — variant doesn't exist.

**Step 3: Add the `SendClientContent` variant and method**

In `session/mod.rs`, add to `SessionCommand`:
```rust
/// Send client content (conversation history or context injection).
SendClientContent {
    turns: Vec<Content>,
    turn_complete: bool,
},
```

Add method to `SessionHandle`:
```rust
/// Send client content (turns + turn_complete flag).
/// Used for injecting conversation history, context, or multi-turn text.
pub async fn send_client_content(
    &self,
    turns: Vec<Content>,
    turn_complete: bool,
) -> Result<(), SessionError> {
    self.command_tx
        .send(SessionCommand::SendClientContent { turns, turn_complete })
        .await
        .map_err(|_| SessionError::ChannelClosed)
}
```

In `transport/connection.rs`, handle the new variant in `run_session()`:
```rust
SessionCommand::SendClientContent { turns, turn_complete } => {
    let msg = ClientContentMessage {
        client_content: ClientContentPayload {
            turns,
            turn_complete: Some(turn_complete),
        },
    };
    let json = serde_json::to_string(&msg)
        .expect("client content serialization is infallible");
    ws_write.send(Message::Text(json)).await
        .map_err(|e| SessionError::WebSocket(e.to_string()))?;
}
```

**Step 4: Run tests**

```bash
cd crates/gemini-live-wire && cargo test
```

Expected: ALL pass.

**Step 5: Commit**

```bash
git add -A
git commit -m "feat(wire): add send_client_content to SessionHandle"
```

---

## Phase 2: Layer 1 — Agent Runtime (`gemini-live-runtime`)

### Task 5: Scaffold `gemini-live-runtime` crate with core types

**Files:**
- Create: `crates/gemini-live-runtime/Cargo.toml`
- Create: `crates/gemini-live-runtime/src/lib.rs`
- Create: `crates/gemini-live-runtime/src/agent_session.rs`
- Create: `crates/gemini-live-runtime/src/state.rs`
- Create: `crates/gemini-live-runtime/src/context.rs`
- Modify: workspace `Cargo.toml`

**Step 1: Add to workspace**

In root `Cargo.toml`:
```toml
[workspace]
resolver = "2"
members = [
    "crates/gemini-live-wire",
    "crates/gemini-live-runtime",
]
```

**Step 2: Create `crates/gemini-live-runtime/Cargo.toml`**

```toml
[package]
name = "gemini-live-runtime"
version = "0.1.0"
edition.workspace = true
license.workspace = true
description = "Agent runtime for Gemini Live — tools, streaming, agent transfer, middleware"

[dependencies]
gemini-live-wire = { path = "../gemini-live-wire" }
tokio = { version = "1", features = ["full"] }
tokio-util = "0.7"
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
dashmap = "6"
parking_lot = "0.12"
uuid = { version = "1", features = ["v4"] }
tracing = { version = "0.1", optional = true }

[dev-dependencies]
tokio-test = "0.4"

[features]
default = []
tracing-support = ["dep:tracing", "gemini-live-wire/tracing-support"]
```

**Step 3: Create `crates/gemini-live-runtime/src/agent_session.rs`**

```rust
//! AgentSession — intercepting wrapper around SessionHandle.
//!
//! Replaces ADK Python's LiveRequestQueue. Instead of adding a second queue
//! on top of SessionHandle's existing mpsc channel, this wraps SessionHandle
//! and intercepts sends for: (1) input fan-out to streaming tools,
//! (2) middleware hooks, (3) state tracking.
//!
//! Data flow: App → AgentSession → SessionHandle → WebSocket
//!                                ↘ broadcast to input-streaming tools
//!
//! ONE queue, ONE consumer task, zero-copy on the hot path.

use gemini_live_wire::prelude::{FunctionResponse};
use gemini_live_wire::session::{SessionEvent, SessionHandle};
use tokio::sync::broadcast;

use crate::error::AgentError;
use crate::state::State;

/// Input events broadcast to input-streaming tools.
/// Distinct from SessionCommand — this is observation-only.
#[derive(Debug, Clone)]
pub enum InputEvent {
    /// Raw PCM16 audio bytes.
    Audio(Vec<u8>),
    /// Text content.
    Text(String),
    /// User started speaking.
    ActivityStart,
    /// User stopped speaking.
    ActivityEnd,
}

/// Intercepting wrapper around SessionHandle.
///
/// Adds input fan-out, middleware hooks, and state tracking without
/// introducing a second queue (avoids double-queuing).
#[derive(Clone)]
pub struct AgentSession {
    /// The underlying wire-level session (Layer 0).
    session: SessionHandle,
    /// Fan-out for input-streaming tools.
    /// Zero-cost when no tools are subscribed (receiver_count == 0).
    input_broadcast: broadcast::Sender<InputEvent>,
    /// Conversation state container.
    state: State,
}

impl AgentSession {
    /// Create a new AgentSession wrapping a SessionHandle.
    pub fn new(session: SessionHandle) -> Self {
        let (input_broadcast, _) = broadcast::channel(256);
        Self {
            session,
            input_broadcast,
            state: State::new(),
        }
    }

    /// Send audio data. Fans out to input-streaming tools ONLY if listeners exist.
    pub async fn send_audio(&self, data: Vec<u8>) -> Result<(), AgentError> {
        // Fan-out ONLY if input-streaming tools are listening (zero-copy when not)
        if self.input_broadcast.receiver_count() > 0 {
            let _ = self.input_broadcast.send(InputEvent::Audio(data.clone()));
        }
        // Forward directly to Layer 0 (ONE hop to WebSocket)
        self.session.send_audio(data).await.map_err(AgentError::Session)
    }

    /// Send a text message.
    pub async fn send_text(&self, text: impl Into<String>) -> Result<(), AgentError> {
        let t = text.into();
        if self.input_broadcast.receiver_count() > 0 {
            let _ = self.input_broadcast.send(InputEvent::Text(t.clone()));
        }
        self.session.send_text(t).await.map_err(AgentError::Session)
    }

    /// Send tool responses.
    pub async fn send_tool_response(
        &self,
        responses: Vec<FunctionResponse>,
    ) -> Result<(), AgentError> {
        self.session.send_tool_response(responses).await.map_err(AgentError::Session)
    }

    /// Signal activity start (user started speaking).
    pub async fn signal_activity_start(&self) -> Result<(), AgentError> {
        if self.input_broadcast.receiver_count() > 0 {
            let _ = self.input_broadcast.send(InputEvent::ActivityStart);
        }
        self.session.signal_activity_start().await.map_err(AgentError::Session)
    }

    /// Signal activity end (user stopped speaking).
    pub async fn signal_activity_end(&self) -> Result<(), AgentError> {
        if self.input_broadcast.receiver_count() > 0 {
            let _ = self.input_broadcast.send(InputEvent::ActivityEnd);
        }
        self.session.signal_activity_end().await.map_err(AgentError::Session)
    }

    /// Gracefully disconnect.
    pub async fn disconnect(&self) -> Result<(), AgentError> {
        self.session.disconnect().await.map_err(AgentError::Session)
    }

    /// Subscribe to input events (for input-streaming tools).
    pub fn subscribe_input(&self) -> broadcast::Receiver<InputEvent> {
        self.input_broadcast.subscribe()
    }

    /// Subscribe to session events (delegates to SessionHandle).
    pub fn subscribe_events(&self) -> broadcast::Receiver<SessionEvent> {
        self.session.subscribe()
    }

    /// Access the underlying SessionHandle for advanced wire-level control.
    pub fn wire(&self) -> &SessionHandle {
        &self.session
    }

    /// Access conversation state.
    pub fn state(&self) -> &State {
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::{broadcast, mpsc, watch};
    use gemini_live_wire::session::{SessionHandle, SessionState, SessionPhase};
    use std::sync::Arc;

    fn mock_session_handle() -> SessionHandle {
        let (cmd_tx, _cmd_rx) = mpsc::channel(16);
        let (evt_tx, _) = broadcast::channel(16);
        let (phase_tx, phase_rx) = watch::channel(SessionPhase::Disconnected);
        let state = Arc::new(SessionState::new(phase_tx));
        SessionHandle::new(cmd_tx, evt_tx, state, phase_rx)
    }

    #[tokio::test]
    async fn send_audio_without_subscribers_no_clone() {
        let handle = mock_session_handle();
        let session = AgentSession::new(handle);
        // No subscribers — input_broadcast.receiver_count() == 0
        // send_audio should NOT clone data for broadcast
        assert_eq!(session.input_broadcast.receiver_count(), 0);
    }

    #[tokio::test]
    async fn send_audio_with_subscriber_broadcasts() {
        let handle = mock_session_handle();
        let session = AgentSession::new(handle);
        let mut input_rx = session.subscribe_input();
        assert_eq!(session.input_broadcast.receiver_count(), 1);

        // send_audio will fail at SessionHandle level (no real WS), but
        // the broadcast should still fire
        let data = vec![1, 2, 3, 4];
        let _ = session.send_audio(data.clone()).await;

        match input_rx.try_recv() {
            Ok(InputEvent::Audio(received)) => assert_eq!(received, data),
            other => panic!("expected Audio, got {:?}", other),
        }
    }

    #[test]
    fn agent_session_is_clone() {
        let handle = mock_session_handle();
        let session = AgentSession::new(handle);
        let _clone = session.clone();
    }

    #[test]
    fn state_accessible() {
        let handle = mock_session_handle();
        let session = AgentSession::new(handle);
        session.state().set("key", "value");
        assert_eq!(session.state().get::<String>("key"), Some("value".to_string()));
    }
}
```

**Step 4: Create `crates/gemini-live-runtime/src/state.rs`**

```rust
//! Typed key-value state container for agents.

use dashmap::DashMap;
use serde_json::Value;
use std::sync::Arc;

/// A concurrent, type-safe state container that agents read from and write to.
#[derive(Debug, Clone, Default)]
pub struct State {
    inner: Arc<DashMap<String, Value>>,
}

impl State {
    pub fn new() -> Self {
        Self { inner: Arc::new(DashMap::new()) }
    }

    /// Get a value by key, attempting to deserialize to the requested type.
    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.inner.get(key).and_then(|v| serde_json::from_value(v.value().clone()).ok())
    }

    /// Get a raw JSON value by key.
    pub fn get_raw(&self, key: &str) -> Option<Value> {
        self.inner.get(key).map(|v| v.value().clone())
    }

    /// Set a value by key.
    pub fn set(&self, key: impl Into<String>, value: impl serde::Serialize) {
        let v = serde_json::to_value(value).expect("value must be serializable");
        self.inner.insert(key.into(), v);
    }

    /// Check if a key exists.
    pub fn contains(&self, key: &str) -> bool {
        self.inner.contains_key(key)
    }

    /// Remove a key.
    pub fn remove(&self, key: &str) -> Option<Value> {
        self.inner.remove(key).map(|(_, v)| v)
    }

    /// Get all keys.
    pub fn keys(&self) -> Vec<String> {
        self.inner.iter().map(|r| r.key().clone()).collect()
    }

    /// Create a new State containing only the specified keys.
    pub fn pick(&self, keys: &[&str]) -> State {
        let new = State::new();
        for key in keys {
            if let Some(v) = self.get_raw(key) {
                new.set(*key, v);
            }
        }
        new
    }

    /// Merge another state into this one (other's values overwrite on conflict).
    pub fn merge(&self, other: &State) {
        for entry in other.inner.iter() {
            self.inner.insert(entry.key().clone(), entry.value().clone());
        }
    }

    /// Rename a key.
    pub fn rename(&self, from: &str, to: &str) {
        if let Some(v) = self.remove(from) {
            self.inner.insert(to.to_string(), v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_string() {
        let state = State::new();
        state.set("name", "Alice");
        assert_eq!(state.get::<String>("name"), Some("Alice".to_string()));
    }

    #[test]
    fn set_and_get_json() {
        let state = State::new();
        state.set("data", serde_json::json!({"temp": 22}));
        let v: Value = state.get("data").unwrap();
        assert_eq!(v["temp"], 22);
    }

    #[test]
    fn pick_subset() {
        let state = State::new();
        state.set("a", 1);
        state.set("b", 2);
        state.set("c", 3);
        let picked = state.pick(&["a", "c"]);
        assert!(picked.contains("a"));
        assert!(!picked.contains("b"));
        assert!(picked.contains("c"));
    }

    #[test]
    fn merge_states() {
        let s1 = State::new();
        s1.set("a", 1);
        let s2 = State::new();
        s2.set("b", 2);
        s1.merge(&s2);
        assert!(s1.contains("a"));
        assert!(s1.contains("b"));
    }

    #[test]
    fn rename_key() {
        let state = State::new();
        state.set("old", "value");
        state.rename("old", "new");
        assert!(!state.contains("old"));
        assert_eq!(state.get::<String>("new"), Some("value".to_string()));
    }
}
```

**Step 5: Create `crates/gemini-live-runtime/src/lib.rs`**

```rust
//! # gemini-live-runtime
//!
//! Agent runtime for the Gemini Multimodal Live API.
//! Provides the Agent trait, AgentSession (intercepting wrapper around SessionHandle),
//! tool dispatch, streaming tools, agent transfer, and middleware.

pub mod agent_session;
pub mod state;

// Re-export wire types that runtime users need
pub use gemini_live_wire;
```

**Step 6: Verify it compiles and tests pass**

```bash
cargo test -p gemini-live-runtime
```

Expected: All tests pass.

**Step 7: Commit**

```bash
git add -A
git commit -m "feat(runtime): scaffold gemini-live-runtime with AgentSession and State"
```

---

### Task 6: Implement the Agent trait and AgentError

**Files:**
- Create: `crates/gemini-live-runtime/src/agent.rs`
- Create: `crates/gemini-live-runtime/src/error.rs`
- Modify: `crates/gemini-live-runtime/src/lib.rs`

**Step 1: Create `crates/gemini-live-runtime/src/error.rs`**

```rust
//! Error types for the agent runtime.

use gemini_live_wire::session::SessionError;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("Session error: {0}")]
    Session(#[from] SessionError),

    #[error("Tool error: {0}")]
    Tool(#[from] ToolError),

    #[error("Unknown agent: {0}")]
    UnknownAgent(String),

    #[error("Agent transfer failed: {0}")]
    TransferFailed(String),

    #[error("Agent session closed")]
    SessionClosed,

    #[error("Timeout")]
    Timeout,

    #[error("{0}")]
    Other(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Tool execution failed: {0}")]
    ExecutionFailed(String),

    #[error("Tool not found: {0}")]
    NotFound(String),

    #[error("Invalid arguments: {0}")]
    InvalidArgs(String),

    #[error("Tool cancelled")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}
```

**Step 2: Create `crates/gemini-live-runtime/src/agent.rs`**

```rust
//! The core Agent trait and AgentEvent type.

use std::time::Duration;

use async_trait::async_trait;
use gemini_live_wire::prelude::{FunctionCall, Tool};

use crate::context::InvocationContext;
use crate::error::AgentError;

/// The fundamental agent trait. Everything that can process a live session
/// implements this — LLM agents, function agents, pipelines, routers.
#[async_trait]
pub trait Agent: Send + Sync + 'static {
    /// Human-readable name for routing, logging, and debugging.
    fn name(&self) -> &str;

    /// Run this agent on a live session. Returns when the agent is done
    /// (turn complete, transfer, or disconnect).
    async fn run_live(&self, ctx: &mut InvocationContext) -> Result<(), AgentError>;

    /// Declare tools this agent provides (sent in the setup message).
    fn tools(&self) -> Vec<Tool> {
        vec![]
    }

    /// Sub-agents this agent can transfer control to.
    fn sub_agents(&self) -> Vec<std::sync::Arc<dyn Agent>> {
        vec![]
    }
}

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
    ToolCallStarted { name: String, args: serde_json::Value },
    ToolCallCompleted { name: String, result: serde_json::Value, duration: Duration },
    ToolCallFailed { name: String, error: String },
    StreamingToolYield { name: String, value: serde_json::Value },
    /// Multi-agent lifecycle.
    AgentTransfer { from: String, to: String },
    /// State changes.
    StateChanged { key: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verify the trait is object-safe
    fn _assert_object_safe(_: &dyn Agent) {}

    #[test]
    fn agent_event_is_send_and_clone() {
        fn assert_send_clone<T: Send + Clone>() {}
        assert_send_clone::<AgentEvent>();
    }
}
```

**Step 3: Create a stub `crates/gemini-live-runtime/src/context.rs`**

```rust
//! InvocationContext — the session state container flowing through agent execution.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::broadcast;

use crate::agent::AgentEvent;
use crate::agent_session::AgentSession;

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
    /// Emit an event to all subscribers.
    pub fn emit(&self, event: AgentEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Convenience: access the state container.
    pub fn state(&self) -> &crate::state::State {
        self.agent_session.state()
    }
}
```

**Step 4: Update `lib.rs`**

```rust
pub mod agent_session;
pub mod state;
pub mod agent;
pub mod error;
pub mod context;

pub use gemini_live_wire;
```

**Step 5: Verify compilation**

```bash
cargo test -p gemini-live-runtime
```

Expected: ALL pass.

**Step 6: Commit**

```bash
git add -A
git commit -m "feat(runtime): add Agent trait, AgentEvent, AgentError, InvocationContext"
```

---

### Task 7: Implement ToolDispatcher with three tool types

**Files:**
- Create: `crates/gemini-live-runtime/src/tool.rs`
- Modify: `crates/gemini-live-runtime/src/lib.rs`

**Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct MockTool;

    #[async_trait]
    impl ToolFunction for MockTool {
        fn name(&self) -> &str { "mock_tool" }
        fn description(&self) -> &str { "A mock tool" }
        fn parameters(&self) -> Option<serde_json::Value> { None }
        async fn call(&self, _args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
            Ok(json!({"result": "ok"}))
        }
    }

    #[tokio::test]
    async fn register_and_call_function_tool() {
        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register_function(Arc::new(MockTool));
        let result = dispatcher.call_function("mock_tool", json!({})).await.unwrap();
        assert_eq!(result["result"], "ok");
    }

    #[tokio::test]
    async fn call_unknown_tool_returns_error() {
        let dispatcher = ToolDispatcher::new();
        let result = dispatcher.call_function("nonexistent", json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn to_tool_declarations() {
        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register_function(Arc::new(MockTool));
        let decls = dispatcher.to_tool_declarations();
        assert_eq!(decls.len(), 1);
    }
}
```

**Step 2: Implement tool.rs**

```rust
//! Tool dispatch — regular, streaming, and input-streaming tools.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use parking_lot::Mutex as SyncMutex;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use gemini_live_wire::prelude::{FunctionCall, FunctionDeclaration, FunctionResponse, Tool};

use crate::error::ToolError;
use crate::agent_session::InputEvent;

/// A regular tool — called once, returns a result.
#[async_trait]
pub trait ToolFunction: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Option<serde_json::Value>;
    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError>;
}

/// A streaming tool — runs in background, yields multiple results.
#[async_trait]
pub trait StreamingTool: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Option<serde_json::Value>;
    async fn run(
        &self,
        args: serde_json::Value,
        yield_tx: mpsc::Sender<serde_json::Value>,
    ) -> Result<(), ToolError>;
}

/// An input-streaming tool — receives duplicated live input while running.
#[async_trait]
pub trait InputStreamingTool: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Option<serde_json::Value>;
    async fn run(
        &self,
        args: serde_json::Value,
        input_rx: broadcast::Receiver<InputEvent>,
        yield_tx: mpsc::Sender<serde_json::Value>,
    ) -> Result<(), ToolError>;
}

/// Classification of a registered tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolClass {
    Regular,
    Streaming,
    InputStream,
}

/// Unified tool storage.
pub enum ToolKind {
    Function(Arc<dyn ToolFunction>),
    Streaming(Arc<dyn StreamingTool>),
    InputStream(Arc<dyn InputStreamingTool>),
}

/// Handle to a running streaming tool.
pub struct ActiveStreamingTool {
    pub task: JoinHandle<()>,
    pub input_tx: Option<broadcast::Sender<InputEvent>>,
    pub cancel: CancellationToken,
}

/// Routes function calls to the right tool implementation.
pub struct ToolDispatcher {
    tools: HashMap<String, ToolKind>,
    active: Arc<tokio::sync::Mutex<HashMap<String, ActiveStreamingTool>>>,
}

impl ToolDispatcher {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            active: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Register a regular function tool.
    pub fn register_function(&mut self, tool: Arc<dyn ToolFunction>) {
        self.tools.insert(tool.name().to_string(), ToolKind::Function(tool));
    }

    /// Register a streaming tool.
    pub fn register_streaming(&mut self, tool: Arc<dyn StreamingTool>) {
        self.tools.insert(tool.name().to_string(), ToolKind::Streaming(tool));
    }

    /// Register an input-streaming tool.
    pub fn register_input_streaming(&mut self, tool: Arc<dyn InputStreamingTool>) {
        self.tools.insert(tool.name().to_string(), ToolKind::InputStream(tool));
    }

    /// Classify a tool by name.
    pub fn classify(&self, name: &str) -> Option<ToolClass> {
        self.tools.get(name).map(|t| match t {
            ToolKind::Function(_) => ToolClass::Regular,
            ToolKind::Streaming(_) => ToolClass::Streaming,
            ToolKind::InputStream(_) => ToolClass::InputStream,
        })
    }

    /// Call a regular function tool by name.
    pub async fn call_function(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        match self.tools.get(name) {
            Some(ToolKind::Function(f)) => f.call(args).await,
            Some(_) => Err(ToolError::Other(format!("{name} is not a regular function tool"))),
            None => Err(ToolError::NotFound(name.to_string())),
        }
    }

    /// Build a FunctionResponse from a FunctionCall result.
    pub fn build_response(
        call: &FunctionCall,
        result: Result<serde_json::Value, ToolError>,
    ) -> FunctionResponse {
        match result {
            Ok(value) => FunctionResponse {
                name: call.name.clone(),
                response: value,
                id: call.id.clone(),
            },
            Err(e) => FunctionResponse {
                name: call.name.clone(),
                response: serde_json::json!({"error": e.to_string()}),
                id: call.id.clone(),
            },
        }
    }

    /// Cancel a streaming tool by name.
    pub async fn cancel_streaming(&self, name: &str) {
        let mut active = self.active.lock().await;
        if let Some(tool) = active.remove(name) {
            tool.cancel.cancel();
            tool.task.abort();
        }
    }

    /// Cancel streaming tools by IDs.
    pub async fn cancel_by_ids(&self, ids: &[String]) {
        let mut active = self.active.lock().await;
        for id in ids {
            if let Some(tool) = active.remove(id.as_str()) {
                tool.cancel.cancel();
                tool.task.abort();
            }
        }
    }

    /// Generate Tool declarations for the setup message.
    pub fn to_tool_declarations(&self) -> Vec<Tool> {
        let declarations: Vec<FunctionDeclaration> = self.tools.values()
            .map(|t| {
                let (name, desc, params) = match t {
                    ToolKind::Function(f) => (f.name(), f.description(), f.parameters()),
                    ToolKind::Streaming(s) => (s.name(), s.description(), s.parameters()),
                    ToolKind::InputStream(i) => (i.name(), i.description(), i.parameters()),
                };
                FunctionDeclaration {
                    name: name.to_string(),
                    description: desc.to_string(),
                    parameters: params,
                }
            })
            .collect();

        if declarations.is_empty() {
            vec![]
        } else {
            vec![Tool::functions(declarations)]
        }
    }
}

// Tests at the top of this section
```

**Step 3: Update lib.rs and run tests**

```bash
cargo test -p gemini-live-runtime
```

Expected: ALL pass.

**Step 4: Commit**

```bash
git add -A
git commit -m "feat(runtime): add ToolDispatcher with regular, streaming, and input-streaming tools"
```

---

### Task 8: Implement Middleware trait and built-in middleware

**Files:**
- Create: `crates/gemini-live-runtime/src/middleware.rs`
- Modify: `crates/gemini-live-runtime/src/lib.rs`

**Step 1: Implement middleware.rs**

```rust
//! Middleware trait and chain — wraps agent execution at lifecycle points.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use gemini_live_wire::prelude::FunctionCall;

use crate::agent::AgentEvent;
use crate::context::InvocationContext;
use crate::error::{AgentError, ToolError};

/// Middleware hooks — all optional, implement only what you need.
#[async_trait]
pub trait Middleware: Send + Sync + 'static {
    fn name(&self) -> &str;

    async fn before_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> { Ok(()) }
    async fn after_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> { Ok(()) }

    async fn before_tool(&self, _call: &FunctionCall) -> Result<(), AgentError> { Ok(()) }
    async fn after_tool(&self, _call: &FunctionCall, _result: &serde_json::Value) -> Result<(), AgentError> { Ok(()) }
    async fn on_tool_error(&self, _call: &FunctionCall, _err: &ToolError) -> Result<(), AgentError> { Ok(()) }

    async fn on_event(&self, _event: &AgentEvent) -> Result<(), AgentError> { Ok(()) }

    async fn on_error(&self, _err: &AgentError) -> Result<(), AgentError> { Ok(()) }
}

/// Ordered chain of middleware.
#[derive(Clone, Default)]
pub struct MiddlewareChain {
    layers: Vec<Arc<dyn Middleware>>,
}

impl MiddlewareChain {
    pub fn new() -> Self { Self::default() }

    pub fn add(&mut self, middleware: Arc<dyn Middleware>) {
        self.layers.push(middleware);
    }

    pub async fn run_before_agent(&self, ctx: &InvocationContext) -> Result<(), AgentError> {
        for m in &self.layers {
            m.before_agent(ctx).await?;
        }
        Ok(())
    }

    pub async fn run_after_agent(&self, ctx: &InvocationContext) -> Result<(), AgentError> {
        for m in self.layers.iter().rev() {
            m.after_agent(ctx).await?;
        }
        Ok(())
    }

    pub async fn run_before_tool(&self, call: &FunctionCall) -> Result<(), AgentError> {
        for m in &self.layers {
            m.before_tool(call).await?;
        }
        Ok(())
    }

    pub async fn run_after_tool(&self, call: &FunctionCall, result: &serde_json::Value) -> Result<(), AgentError> {
        for m in self.layers.iter().rev() {
            m.after_tool(call, result).await?;
        }
        Ok(())
    }

    pub async fn run_on_event(&self, event: &AgentEvent) -> Result<(), AgentError> {
        for m in &self.layers {
            m.on_event(event).await?;
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool { self.layers.is_empty() }
    pub fn len(&self) -> usize { self.layers.len() }
}

// ── Built-in Middleware ──

/// Logs agent and tool lifecycle events.
pub struct LogMiddleware {
    pub name: String,
}

impl LogMiddleware {
    pub fn new() -> Self { Self { name: "log".to_string() } }
}

#[async_trait]
impl Middleware for LogMiddleware {
    fn name(&self) -> &str { &self.name }

    async fn before_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> {
        // In production, use tracing::info! here
        Ok(())
    }

    async fn before_tool(&self, call: &FunctionCall) -> Result<(), AgentError> {
        // tracing::info!(tool = %call.name, "tool call started");
        Ok(())
    }

    async fn after_tool(&self, call: &FunctionCall, _result: &serde_json::Value) -> Result<(), AgentError> {
        // tracing::info!(tool = %call.name, "tool call completed");
        Ok(())
    }
}

/// Tracks latency of tool calls.
pub struct LatencyMiddleware {
    pub name: String,
}

#[async_trait]
impl Middleware for LatencyMiddleware {
    fn name(&self) -> &str { "latency" }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CountingMiddleware {
        call_count: Arc<std::sync::atomic::AtomicU32>,
    }

    #[async_trait]
    impl Middleware for CountingMiddleware {
        fn name(&self) -> &str { "counter" }

        async fn before_agent(&self, _ctx: &InvocationContext) -> Result<(), AgentError> {
            self.call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn middleware_chain_ordering() {
        let chain = MiddlewareChain::new();
        assert!(chain.is_empty());
        assert_eq!(chain.len(), 0);
    }

    #[test]
    fn middleware_is_object_safe() {
        fn _assert(_: &dyn Middleware) {}
    }
}
```

**Step 2: Update lib.rs, compile, test, commit**

```bash
cargo test -p gemini-live-runtime
git add -A
git commit -m "feat(runtime): add Middleware trait, MiddlewareChain, and built-in middleware"
```

---

### Task 9: Implement AgentRegistry for agent transfer routing

**Files:**
- Create: `crates/gemini-live-runtime/src/router.rs`
- Modify: `crates/gemini-live-runtime/src/lib.rs`

**Step 1: Implement router.rs**

```rust
//! Agent registry and transfer routing.

use std::collections::HashMap;
use std::sync::Arc;

use crate::agent::Agent;

/// Registry of named agents for transfer routing.
#[derive(Default)]
pub struct AgentRegistry {
    agents: HashMap<String, Arc<dyn Agent>>,
}

impl AgentRegistry {
    pub fn new() -> Self { Self::default() }

    /// Register a named agent.
    pub fn register(&mut self, agent: Arc<dyn Agent>) {
        self.agents.insert(agent.name().to_string(), agent);
    }

    /// Look up an agent by name.
    pub fn resolve(&self, name: &str) -> Option<Arc<dyn Agent>> {
        self.agents.get(name).cloned()
    }

    /// List all registered agent names.
    pub fn names(&self) -> Vec<String> {
        self.agents.keys().cloned().collect()
    }

    /// Number of registered agents.
    pub fn len(&self) -> usize { self.agents.len() }
    pub fn is_empty(&self) -> bool { self.agents.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AgentError;
    use crate::context::InvocationContext;
    use async_trait::async_trait;

    struct DummyAgent { name: String }

    #[async_trait]
    impl Agent for DummyAgent {
        fn name(&self) -> &str { &self.name }
        async fn run_live(&self, _ctx: &mut InvocationContext) -> Result<(), AgentError> { Ok(()) }
    }

    #[test]
    fn register_and_resolve() {
        let mut registry = AgentRegistry::new();
        registry.register(Arc::new(DummyAgent { name: "billing".into() }));
        registry.register(Arc::new(DummyAgent { name: "tech".into() }));
        assert_eq!(registry.len(), 2);
        assert!(registry.resolve("billing").is_some());
        assert!(registry.resolve("nonexistent").is_none());
    }
}
```

**Step 2: Update lib.rs, compile, test, commit**

```bash
cargo test -p gemini-live-runtime
git add -A
git commit -m "feat(runtime): add AgentRegistry for agent transfer routing"
```

---

## Phase 3: Layer 2 — Fluent DX (`gemini-live`)

### Task 10: Scaffold `gemini-live` crate with AgentBuilder

**Files:**
- Create: `crates/gemini-live/Cargo.toml`
- Create: `crates/gemini-live/src/lib.rs`
- Create: `crates/gemini-live/src/builder.rs`
- Modify: workspace `Cargo.toml`

**Step 1: Add to workspace**

```toml
members = [
    "crates/gemini-live-wire",
    "crates/gemini-live-runtime",
    "crates/gemini-live",
]
```

**Step 2: Create Cargo.toml**

```toml
[package]
name = "gemini-live"
version = "0.1.0"
edition.workspace = true
license.workspace = true
description = "Fluent DX for Gemini Live — builder API, operator algebra, composition modules"

[dependencies]
gemini-live-wire = { path = "../gemini-live-wire" }
gemini-live-runtime = { path = "../gemini-live-runtime" }
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[dev-dependencies]
tokio-test = "0.4"
```

**Step 3: Create builder.rs with AgentBuilder**

Implement the copy-on-write fluent builder with all setter methods as specified in the design doc Section 5.2. Include tests for:
- Builder creates with name
- Fluent chaining works
- Copy-on-write: mutating a clone doesn't affect original
- `.text_only()` sets correct modalities
- `.url_context()` adds the right tool

**Step 4: Create lib.rs with prelude**

```rust
pub mod builder;

pub use gemini_live_wire;
pub use gemini_live_runtime;

pub mod prelude {
    pub use crate::builder::*;
    pub use gemini_live_wire::prelude::*;
    pub use gemini_live_runtime::agent::*;
    pub use gemini_live_runtime::state::State;
    pub use gemini_live_runtime::agent_session::*;
}
```

**Step 5: Compile, test, commit**

```bash
cargo test -p gemini-live
git add -A
git commit -m "feat(fluent): scaffold gemini-live crate with AgentBuilder"
```

---

### Task 11: Implement operator overloading (>>, |, *, //)

**Files:**
- Create: `crates/gemini-live/src/operators.rs`
- Create: `crates/gemini-live/src/ir.rs`
- Modify: `crates/gemini-live/src/lib.rs`

Implement the `Composable` trait and operator overloads as specified in design doc Section 5.3. Define `Pipeline`, `FanOut`, `Loop`, `Fallback` workflow types. The IR nodes serve as the intermediate representation.

Test that:
- `AgentBuilder >> AgentBuilder` produces a Pipeline
- `Pipeline >> AgentBuilder` flattens (no nesting)
- `AgentBuilder | AgentBuilder` produces a FanOut
- `AgentBuilder * 3` produces a Loop with max=3
- `AgentBuilder * until(pred)` produces a conditional Loop
- `AgentBuilder // AgentBuilder` produces a Fallback

**Commit:**
```bash
git commit -m "feat(fluent): add operator algebra (>>, |, *, //) for agent composition"
```

---

### Task 12: Implement composition modules (S, C, P, M, T)

**Files:**
- Create: `crates/gemini-live/src/compose/mod.rs`
- Create: `crates/gemini-live/src/compose/state.rs` (S module)
- Create: `crates/gemini-live/src/compose/context.rs` (C module)
- Create: `crates/gemini-live/src/compose/prompt.rs` (P module)
- Create: `crates/gemini-live/src/compose/middleware.rs` (M module)
- Create: `crates/gemini-live/src/compose/tools.rs` (T module)

Implement each module as specified in design doc Section 5.4. Each module has:
- A struct with static factory methods
- A composite type supporting operator composition
- Tests for composition and basic functionality

**Commit:**
```bash
git commit -m "feat(fluent): add S, C, P, M, T composition modules"
```

---

### Task 13: Implement pre-built patterns and testing utilities

**Files:**
- Create: `crates/gemini-live/src/patterns.rs`
- Create: `crates/gemini-live/src/testing.rs`

Implement the patterns from design doc Section 5.6 (`review_loop`, `cascade`, `fan_out_merge`, `supervised`, `map_over`) and testing utilities from Section 5.7 (`MockBackend`, `AgentHarness`, `check_contracts`).

**Commit:**
```bash
git commit -m "feat(fluent): add pre-built patterns and testing utilities"
```

---

## Phase 4: Python Bindings

### Task 14: Scaffold `gemini-live-python` crate with PyO3

**Files:**
- Create: `crates/gemini-live-python/Cargo.toml`
- Create: `crates/gemini-live-python/pyproject.toml`
- Create: `crates/gemini-live-python/src/lib.rs`
- Create: `crates/gemini-live-python/src/py_types.rs`
- Create: `crates/gemini-live-python/src/py_config.rs`
- Modify: workspace `Cargo.toml`

Set up the basic PyO3 module structure with type wrappers. Verify it builds with `maturin develop`.

**Commit:**
```bash
git commit -m "feat(python): scaffold PyO3 bindings crate"
```

---

### Task 15: Implement Python session and event bindings

**Files:**
- Create: `crates/gemini-live-python/src/py_session.rs`
- Create: `crates/gemini-live-python/src/py_events.rs`
- Create: `crates/gemini-live-python/src/py_agent.rs`
- Create: `crates/gemini-live-python/src/py_tool.rs`

Implement the three-tier Python API from design doc Section 6.2.

**Commit:**
```bash
git commit -m "feat(python): implement session, event, agent, and tool bindings"
```

---

## Phase 5: Integration and Examples

### Task 16: Wire-level example (Layer 0)

**Files:**
- Create: `examples/wire_raw_session.rs`

A minimal example using only `gemini-live-wire` to connect, send text, and print responses. Verifies the protocol fixes work end-to-end.

**Commit:**
```bash
git commit -m "feat: add wire-level raw session example"
```

---

### Task 17: Runtime agent example (Layer 1)

**Files:**
- Create: `examples/runtime_agent.rs`

An example using `gemini-live-runtime` with the Agent trait, ToolDispatcher, and AgentSession. Demonstrates function calling and streaming tools.

**Commit:**
```bash
git commit -m "feat: add runtime agent example with tool dispatch"
```

---

### Task 18: Fluent pipeline example (Layer 2)

**Files:**
- Create: `examples/fluent_pipeline.rs`

An example using `gemini-live` with the operator algebra, composition modules, and builder API. The full "deep research" pipeline from the design doc.

**Commit:**
```bash
git commit -m "feat: add fluent pipeline example with operator composition"
```

---

### Task 19: Clean up old monolithic src/ directory

**Files:**
- Remove: `src/` (the old monolithic source, now replaced by `crates/`)
- Update: Any remaining references

Only do this AFTER all three layers are working and examples compile.

**Commit:**
```bash
git commit -m "refactor: remove old monolithic src/ in favor of workspace crates"
```

---

---

## Phase 6: Audit-Driven Hardening

### Task 20: Wire protocol completeness (missing Gemini features)

**Files:**
- Modify: `crates/gemini-live-wire/src/protocol/types.rs`
- Modify: `crates/gemini-live-wire/src/protocol/messages.rs`

Add all missing wire features identified in JS SDK audit:
- `ToolSpec` enum replacing all-optional `Tool` struct (sum type, not product type)
- `Role` enum replacing `Option<String>` for `Content.role`
- `Blob.data` as `Vec<u8>` with `#[serde(with = "base64_serde")]`
- `RealtimeInputConfig`: `activity_handling`, `turn_coverage`
- `ContextWindowCompression`, `Proactivity` in setup config
- `RealtimeInput`: `video`, `text`, `audio_stream_end` fields
- `ServerContent`: `generation_complete`, `grounding_metadata`, `turn_complete_reason`, `waiting_for_input`
- `SessionResumptionUpdate`: `last_consumed_client_message_index`
- `VoiceActivity` server message

**Commit:**
```bash
git commit -m "feat(wire): complete wire protocol from JS SDK audit"
```

### Task 21: Audio hot path optimization

**Files:**
- Modify: `crates/gemini-live-wire/Cargo.toml` (add `bytemuck`)
- Modify: `crates/gemini-live-wire/src/transport/connection.rs`

Replace 6-allocation audio path with:
1. `bytemuck::cast_slice` for zero-copy i16→u8
2. Pre-allocated `SendBuffer` for base64 + JSON serialization
3. `Bytes` type for audio data through channels

**Commit:**
```bash
git commit -m "perf(wire): reduce audio hot path from 6 to 2 allocations"
```

### Task 22: Typed tool registration with schemars

**Files:**
- Modify: `crates/gemini-live-runtime/Cargo.toml` (add `schemars`)
- Modify: `crates/gemini-live-runtime/src/tool.rs`

Add `FnTool::typed<T: Deserialize + JsonSchema>()` alongside existing `FnTool::new()`.
Auto-generate JSON Schema from struct. Deserialize args before calling handler.

**Commit:**
```bash
git commit -m "feat(runtime): add FnTool::typed with auto-schema via schemars"
```

### Task 23: Tool timeout and cancellation

**Files:**
- Modify: `crates/gemini-live-runtime/src/tool.rs`

Add `CancellationToken` parameter to `ToolFunction::call()`. Implement timeout
wrapper in `ToolDispatcher`. On timeout: cancel token → grace period → abort → error response.

**Commit:**
```bash
git commit -m "feat(runtime): add tool timeout and CancellationToken support"
```

### Task 24: Barge-in race condition fix

**Files:**
- Modify: `crates/gemini-live-wire/src/flow/barge_in.rs`

Implement tentative barge-in: duck volume on VAD PendingSpeech, flush on confirmed
Speech, restore on false positive.

**Commit:**
```bash
git commit -m "fix(wire): tentative barge-in to prevent false-positive silence"
```

### Task 25: JoinHandle tracking and broadcast lag handling

**Files:**
- Modify: `crates/gemini-live-wire/src/transport/connection.rs`
- Modify: `crates/gemini-live-wire/src/session/mod.rs`
- Modify: `crates/gemini-live-runtime/src/agent_session.rs`

Store connection_loop JoinHandle in SessionHandle. Use JoinSet for tool tasks.
Handle RecvError::Lagged explicitly in event consumers.

**Commit:**
```bash
git commit -m "fix(wire): track JoinHandles and handle broadcast lag"
```

### Task 26: Integration test infrastructure

**Files:**
- Create: `tests/integration/mock_server.rs`
- Create: `tests/integration/connect_test.rs`
- Create: `tests/integration/tool_dispatch_test.rs`

Mock WebSocket server for end-to-end testing of connection, setup handshake,
text exchange, tool dispatch, and reconnection.

**Commit:**
```bash
git commit -m "test: add mock WebSocket server and integration tests"
```

### Task 27: Performance benchmarks

**Files:**
- Create: `benches/audio_pipeline.rs`
- Create: `benches/session_memory.rs`

Criterion benchmarks validating: audio frame latency, memory per session,
concurrent session throughput.

**Commit:**
```bash
git commit -m "bench: add audio pipeline and session memory benchmarks"
```

### Task 28: DX convenience APIs

**Files:**
- Create: `crates/gemini-live-wire/src/quick.rs` (quick_connect convenience fn)
- Create: `crates/gemini-live-runtime/src/llm_agent.rs` (LlmAgent golden path)

Add `quick_connect()` for 5-line hello world. Add `LlmAgent::builder()` that
hides InvocationContext/ToolDispatcher/event loop from basic users.

**Commit:**
```bash
git commit -m "feat: add quick_connect and LlmAgent convenience APIs"
```

---

## Summary

| Phase | Tasks | Crate | Key Deliverables |
|-------|-------|-------|-----------------|
| 1 | 1-4 | `gemini-live-wire` | Workspace scaffold, protocol fixes (ToolSpec enum, ThinkingConfig), send_client_content |
| 2 | 5-9 | `gemini-live-runtime` | AgentSession, State, Agent trait, ToolDispatcher, Middleware, AgentRegistry |
| 3 | 10-13 | `gemini-live` | AgentBuilder, operator algebra, compose modules, patterns, testing |
| 4 | 14-15 | `gemini-live-python` | PyO3 bindings with three-tier Python API |
| 5 | 16-19 | All | Examples, cleanup |
| 6 | 20-28 | All | Wire completeness, hot path optimization, typed tools, tool timeout, barge-in fix, JoinHandle tracking, integration tests, benchmarks, DX convenience APIs |
