# ADK Devtools Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Ship two new crates — `adk-devtools` (library) and `adk-cli` (binary) — providing `adk web` and `adk run` commands with a binary-multiplexed WebSocket protocol and embedded dev UI.

**Architecture:** `adk-devtools` is a library crate with protocol types, Axum DevServer, terminal REPL, and embedded vanilla JS/CSS dev UI. `adk-cli` is a thin binary wrapping `cargo run` with env vars. The protocol uses binary WebSocket frames for audio and JSON text frames for control/devtools channels.

**Tech Stack:** Axum 0.8, clap 4, rust-embed 8, rustyline 15, serde_json, tokio, vanilla JS/CSS.

**Design doc:** `docs/plans/2026-03-03-adk-devtools-design.md`

---

## Task 1: Scaffold `adk-devtools` crate with protocol types

**Files:**
- Create: `crates/adk-devtools/Cargo.toml`
- Create: `crates/adk-devtools/src/lib.rs`
- Create: `crates/adk-devtools/src/protocol.rs`
- Modify: `Cargo.toml` (workspace members)

**Step 1: Create `crates/adk-devtools/Cargo.toml`**

```toml
[package]
name = "adk-devtools"
version = "0.1.0"
edition.workspace = true
license.workspace = true
repository.workspace = true
description = "Dev tools, CLI, and dev UI for gemini-rs agents"

[dependencies]
adk-rs-fluent = { path = "../adk-rs-fluent" }
rs-adk = { path = "../rs-adk" }
rs-genai = { path = "../rs-genai" }
axum = { version = "0.8", features = ["ws"] }
tokio = { version = "1", features = ["full"] }
rust-embed = "8"
mime_guess = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rustyline = "15"
tracing = "0.1"
dotenvy = "0.15"
thiserror = "2"
bytes = "1"
futures = "0.3"
base64 = "0.22"
async-trait = "0.1"
```

**Step 2: Create `crates/adk-devtools/src/protocol.rs`**

This module defines all WebSocket message types. Two categories: client→server and server→client. Each message has a `ch` (channel) field for routing.

```rust
//! Binary-multiplexed WebSocket protocol types.
//!
//! Text frames carry JSON with a `ch` field for channel routing.
//! Binary frames carry raw audio/video with a 1-byte channel tag.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// -- Binary channel tags --

/// Mic audio from client (PCM16 @ 16kHz).
pub const CHANNEL_MIC: u8 = 0x01;
/// Model audio to client (PCM16 @ 24kHz).
pub const CHANNEL_AUDIO_OUT: u8 = 0x02;
/// Video frame from client (JPEG).
pub const CHANNEL_VIDEO: u8 = 0x03;

// -- Client → Server messages --

/// Messages sent from the browser to the server (JSON text frames).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "ch", rename_all = "lowercase")]
pub enum ClientMsg {
    /// Control channel: session lifecycle.
    Control(ControlClientMsg),
}

/// Control messages from client.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlClientMsg {
    /// Start a session with the named agent.
    Start {
        agent: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        voice: Option<String>,
    },
    /// Send a text message to the agent.
    Text { text: String },
    /// End the session.
    Stop,
}

// -- Server → Client messages --

/// Messages sent from the server to the browser (JSON text frames).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "ch", rename_all = "lowercase")]
pub enum ServerMsg {
    /// Control channel: session lifecycle events.
    Control(ControlServerMsg),
    /// Model text output.
    Text(TextMsg),
    /// Transcript (user or model speech-to-text).
    Transcript(TranscriptMsg),
    /// State key mutation.
    State(StateMsg),
    /// Phase transition.
    Phase(PhaseMsg),
    /// Tool call lifecycle event.
    Tool(ToolMsg),
    /// Aggregated telemetry snapshot.
    Telemetry(TelemetryMsg),
}

/// Control messages from server.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlServerMsg {
    /// Session successfully connected.
    Connected { session_id: String, agent: String },
    /// Model finished its response.
    TurnComplete,
    /// User interrupted the model.
    Interrupted,
    /// Voice activity detected.
    VadStart,
    /// Voice activity ended.
    VadEnd,
    /// Error occurred.
    Error { message: String },
}

/// Model text output.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TextMsg {
    /// Streaming text delta.
    Delta { delta: String },
    /// Final complete text.
    Complete { text: String },
}

/// Transcription of speech.
#[derive(Debug, Clone, Serialize)]
pub struct TranscriptMsg {
    pub role: String,
    pub text: String,
    #[serde(rename = "final")]
    pub is_final: bool,
}

/// State key mutation event.
#[derive(Debug, Clone, Serialize)]
pub struct StateMsg {
    pub key: String,
    pub value: Value,
}

/// Phase transition event.
#[derive(Debug, Clone, Serialize)]
pub struct PhaseMsg {
    pub from: String,
    pub to: String,
    pub reason: String,
}

/// Tool call lifecycle event.
#[derive(Debug, Clone, Serialize)]
pub struct ToolMsg {
    pub name: String,
    pub args: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Aggregated telemetry snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct TelemetryMsg {
    pub stats: Value,
}

// -- Agent metadata --

/// Metadata about a registered agent (sent to UI for sidebar).
#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub name: String,
    pub description: String,
    pub category: String,
    pub features: Vec<String>,
}
```

**Step 3: Create `crates/adk-devtools/src/lib.rs`**

```rust
//! Dev tools for gemini-rs: DevServer, REPL, and binary WebSocket protocol.

#![warn(missing_docs)]

pub mod protocol;
```

**Step 4: Add to workspace**

Add `"crates/adk-devtools"` to the `members` list in the root `Cargo.toml`.

**Step 5: Verify it compiles**

Run: `cargo check -p adk-devtools`
Expected: Compiles with no errors.

**Step 6: Commit**

```bash
git add crates/adk-devtools/ Cargo.toml
git commit -m "feat(adk-devtools): scaffold crate with binary WebSocket protocol types"
```

---

## Task 2: Agent registry and descriptor types

**Files:**
- Create: `crates/adk-devtools/src/registry.rs`
- Modify: `crates/adk-devtools/src/lib.rs`

**Step 1: Create `crates/adk-devtools/src/registry.rs`**

The agent registry holds `AgentDescriptor` instances. Each descriptor has metadata + a factory closure that returns a `Live` builder when a WebSocket connection starts.

```rust
//! Agent registry for devtools — stores agent descriptors and metadata.

use std::collections::HashMap;
use std::sync::Arc;

use adk_rs_fluent::live::Live;

use crate::protocol::AgentInfo;

/// Category for grouping agents in the dev UI sidebar.
#[derive(Debug, Clone, Copy)]
pub enum AgentCategory {
    /// Simple examples.
    Basic,
    /// Multi-feature agents.
    Advanced,
    /// Full production patterns.
    Showcase,
}

impl std::fmt::Display for AgentCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Basic => write!(f, "basic"),
            Self::Advanced => write!(f, "advanced"),
            Self::Showcase => write!(f, "showcase"),
        }
    }
}

/// Feature flags controlling which devtools panels appear.
#[derive(Debug, Clone, Copy)]
pub enum Feature {
    /// Voice I/O.
    Voice,
    /// Text I/O.
    Text,
    /// Tool calling.
    Tools,
    /// Phase state machine.
    Phases,
    /// LLM extraction.
    Extraction,
    /// Evaluation / guardrails.
    Evaluation,
}

impl std::fmt::Display for Feature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Voice => write!(f, "voice"),
            Self::Text => write!(f, "text"),
            Self::Tools => write!(f, "tools"),
            Self::Phases => write!(f, "phases"),
            Self::Extraction => write!(f, "extraction"),
            Self::Evaluation => write!(f, "evaluation"),
        }
    }
}

/// Factory type that creates a new `Live` builder per WebSocket connection.
pub type LiveFactory = Arc<dyn Fn() -> Live + Send + Sync>;

/// Describes an agent registered with the devtools server.
pub struct AgentDescriptor {
    /// Human-readable name (used in URL paths and CLI).
    pub name: String,
    /// Short description shown in dev UI sidebar.
    pub description: String,
    /// Category for grouping.
    pub category: AgentCategory,
    /// Feature flags.
    pub features: Vec<Feature>,
    /// Factory called per connection.
    pub factory: LiveFactory,
}

/// Registry of agents available in the devtools server.
pub struct AgentRegistry {
    agents: HashMap<String, AgentDescriptor>,
    order: Vec<String>,
}

impl AgentRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
            order: Vec::new(),
        }
    }

    /// Register an agent.
    pub fn register(&mut self, descriptor: AgentDescriptor) {
        self.order.push(descriptor.name.clone());
        self.agents.insert(descriptor.name.clone(), descriptor);
    }

    /// Look up an agent by name.
    pub fn get(&self, name: &str) -> Option<&AgentDescriptor> {
        self.agents.get(name)
    }

    /// List all agents as metadata for the UI.
    pub fn list(&self) -> Vec<AgentInfo> {
        self.order
            .iter()
            .filter_map(|name| self.agents.get(name))
            .map(|d| AgentInfo {
                name: d.name.clone(),
                description: d.description.clone(),
                category: d.category.to_string(),
                features: d.features.iter().map(|f| f.to_string()).collect(),
            })
            .collect()
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}
```

**Step 2: Wire up in lib.rs**

Add `pub mod registry;` to `lib.rs`.

**Step 3: Verify**

Run: `cargo check -p adk-devtools`

**Step 4: Commit**

```bash
git add crates/adk-devtools/src/registry.rs crates/adk-devtools/src/lib.rs
git commit -m "feat(adk-devtools): add agent registry and descriptor types"
```

---

## Task 3: DevServer — Axum server with WebSocket handler

**Files:**
- Create: `crates/adk-devtools/src/server.rs`
- Modify: `crates/adk-devtools/src/lib.rs`

This is the core server. It serves the embedded UI, lists agents via JSON, and handles WebSocket connections with binary-multiplexed protocol.

**Step 1: Create `crates/adk-devtools/src/server.rs`**

Reference the existing cookbook patterns from `cookbooks/ui/src/main.rs` and `cookbooks/ui/src/ws_handler.rs`, but with the binary protocol.

```rust
//! Axum-based dev server with embedded UI and binary WebSocket protocol.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, State as AxumState, WebSocketUpgrade};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use futures::{SinkExt, StreamExt};
use rust_embed::Embed;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::protocol::{
    AgentInfo, ClientMsg, ControlClientMsg, ControlServerMsg, ServerMsg, CHANNEL_AUDIO_OUT,
    CHANNEL_MIC,
};
use crate::registry::AgentRegistry;

type SharedRegistry = Arc<AgentRegistry>;

/// Embedded static assets for the dev UI.
#[derive(Embed)]
#[folder = "ui/"]
struct UiAssets;

/// Start the devtools server.
pub async fn serve(registry: AgentRegistry, host: &str, port: u16) -> Result<(), std::io::Error> {
    let registry = Arc::new(registry);

    let app = Router::new()
        .route("/", get(index_page))
        .route("/api/agents", get(list_agents))
        .route("/ws/{agent}", get(ws_upgrade))
        .route("/static/{*path}", get(static_asset))
        .with_state(registry);

    let addr = format!("{host}:{port}");
    info!("adk devtools at http://{addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await
}

async fn index_page() -> impl IntoResponse {
    match UiAssets::get("index.html") {
        Some(file) => Html(String::from_utf8_lossy(file.data.as_ref()).to_string()).into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, "UI not built").into_response(),
    }
}

async fn list_agents(AxumState(registry): AxumState<SharedRegistry>) -> Json<Vec<AgentInfo>> {
    Json(registry.list())
}

async fn static_asset(Path(path): Path<String>) -> impl IntoResponse {
    match UiAssets::get(&path) {
        Some(file) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            Response::builder()
                .header("content-type", mime.as_ref())
                .body(axum::body::Body::from(file.data.to_vec()))
                .unwrap()
                .into_response()
        }
        None => (axum::http::StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

async fn ws_upgrade(
    Path(agent): Path<String>,
    AxumState(registry): AxumState<SharedRegistry>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if registry.get(&agent).is_some() {
        ws.on_upgrade(move |socket| handle_ws(socket, agent, registry))
    } else {
        ws.on_upgrade(|socket| async { let _ = socket.close().await; })
    }
}

async fn handle_ws(socket: WebSocket, agent_name: String, registry: Arc<AgentRegistry>) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Channel for server→client messages
    let (srv_tx, mut srv_rx) = mpsc::unbounded_channel::<ServerMsg>();
    // Channel for binary audio out
    let (audio_tx, mut audio_rx) = mpsc::unbounded_channel::<bytes::Bytes>();

    // Send task: forward ServerMsg (JSON text) and audio (binary) to WebSocket
    let send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(msg) = srv_rx.recv() => {
                    if let Ok(json) = serde_json::to_string(&msg) {
                        if ws_tx.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                }
                Some(audio) = audio_rx.recv() => {
                    let mut frame = Vec::with_capacity(1 + audio.len());
                    frame.push(CHANNEL_AUDIO_OUT);
                    frame.extend_from_slice(&audio);
                    if ws_tx.send(Message::Binary(frame.into())).await.is_err() {
                        break;
                    }
                }
                else => break,
            }
        }
    });

    // Process incoming messages
    while let Some(Ok(msg)) = ws_rx.next().await {
        match msg {
            Message::Text(text) => {
                match serde_json::from_str::<ClientMsg>(&text) {
                    Ok(ClientMsg::Control(ControlClientMsg::Start { agent, model, voice })) => {
                        // TODO: Task 4 wires up the Live session here
                        info!("Start session for agent: {agent}");
                        let _ = srv_tx.send(ServerMsg::Control(ControlServerMsg::Connected {
                            session_id: uuid_v4(),
                            agent: agent_name.clone(),
                        }));
                    }
                    Ok(ClientMsg::Control(ControlClientMsg::Text { text })) => {
                        // TODO: forward to Live handle
                        info!("Text: {text}");
                    }
                    Ok(ClientMsg::Control(ControlClientMsg::Stop)) => {
                        info!("Stop");
                        break;
                    }
                    Err(e) => {
                        warn!("Invalid message: {e}");
                    }
                }
            }
            Message::Binary(data) => {
                if data.is_empty() { continue; }
                match data[0] {
                    CHANNEL_MIC => {
                        // TODO: forward to Live handle
                        let _audio = &data[1..];
                    }
                    tag => {
                        warn!("Unknown binary channel: 0x{tag:02x}");
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    send_task.abort();
}

fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    format!("{t:032x}")
}
```

**Step 2: Create placeholder `ui/index.html`**

Create `crates/adk-devtools/ui/index.html`:

```html
<!DOCTYPE html>
<html>
<head><title>gemini-rs devtools</title></head>
<body>
<h1>gemini-rs devtools</h1>
<p>Dev UI coming in Task 7.</p>
</body>
</html>
```

**Step 3: Wire up in lib.rs**

Add `pub mod server;` to `lib.rs`.

**Step 4: Verify**

Run: `cargo check -p adk-devtools`

**Step 5: Commit**

```bash
git add crates/adk-devtools/src/server.rs crates/adk-devtools/ui/
git commit -m "feat(adk-devtools): add Axum DevServer with binary WebSocket handler"
```

---

## Task 4: Wire WebSocket handler to Live sessions

**Files:**
- Modify: `crates/adk-devtools/src/server.rs`

This is the critical integration — when a `start` message arrives, the server calls the agent's factory to get a `Live` builder, hooks up all callbacks to emit on the WebSocket channels, connects the Gemini session, and starts forwarding audio.

**Step 1: Extract `handle_ws` into a session module**

Create `crates/adk-devtools/src/session.rs`:

```rust
//! Per-connection session: bridges a Live session to the binary WebSocket protocol.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{info, warn};

use rs_adk::live::LiveHandle;
use rs_genai::prelude::*;

use crate::protocol::*;
use crate::registry::AgentRegistry;

/// Run a live agent session, bridging callbacks to protocol messages.
///
/// `srv_tx` sends JSON text messages; `audio_tx` sends raw PCM16 binary.
pub async fn run_session(
    agent_name: &str,
    registry: &AgentRegistry,
    model_override: Option<&str>,
    voice_override: Option<&str>,
    srv_tx: mpsc::UnboundedSender<ServerMsg>,
    audio_tx: mpsc::UnboundedSender<Bytes>,
) -> Option<LiveHandle> {
    let descriptor = registry.get(agent_name)?;

    // Create a fresh Live builder from the factory
    let mut builder = (descriptor.factory)();

    // Apply model/voice overrides from the start message
    if let Some(model_str) = model_override {
        if let Some(model) = parse_model(model_str) {
            builder = builder.model(model);
        }
    }
    if let Some(voice_str) = voice_override {
        if let Some(voice) = parse_voice(voice_str) {
            builder = builder.voice(voice);
        }
    }

    // --- Wire fast-lane callbacks (audio, text, VAD) ---

    let tx = audio_tx.clone();
    builder = builder.on_audio(move |data: &Bytes| {
        let _ = tx.send(data.clone());
    });

    let tx = srv_tx.clone();
    builder = builder.on_text(move |text: &str| {
        let _ = tx.send(ServerMsg::Text(TextMsg::Delta {
            delta: text.to_string(),
        }));
    });

    let tx = srv_tx.clone();
    builder = builder.on_text_complete(move |text: &str| {
        let _ = tx.send(ServerMsg::Text(TextMsg::Complete {
            text: text.to_string(),
        }));
    });

    let tx = srv_tx.clone();
    builder = builder.on_input_transcript(move |text: &str| {
        let _ = tx.send(ServerMsg::Transcript(TranscriptMsg {
            role: "user".into(),
            text: text.to_string(),
            is_final: true,
        }));
    });

    let tx = srv_tx.clone();
    builder = builder.on_output_transcript(move |text: &str| {
        let _ = tx.send(ServerMsg::Transcript(TranscriptMsg {
            role: "model".into(),
            text: text.to_string(),
            is_final: true,
        }));
    });

    let tx = srv_tx.clone();
    builder = builder.on_vad_start(move || {
        let _ = tx.send(ServerMsg::Control(ControlServerMsg::VadStart));
    });

    let tx = srv_tx.clone();
    builder = builder.on_vad_end(move || {
        let _ = tx.send(ServerMsg::Control(ControlServerMsg::VadEnd));
    });

    // --- Wire control-lane callbacks ---

    let tx = srv_tx.clone();
    builder = builder.on_turn_complete(move || {
        let tx = tx.clone();
        async move {
            let _ = tx.send(ServerMsg::Control(ControlServerMsg::TurnComplete));
        }
    });

    let tx = srv_tx.clone();
    builder = builder.on_interrupted(move || {
        let tx = tx.clone();
        async move {
            let _ = tx.send(ServerMsg::Control(ControlServerMsg::Interrupted));
        }
    });

    let tx = srv_tx.clone();
    builder = builder.on_error(move |msg: String| {
        let tx = tx.clone();
        async move {
            let _ = tx.send(ServerMsg::Control(ControlServerMsg::Error { message: msg }));
        }
    });

    // --- Wire devtools callbacks (state, phase, tool) ---

    let tx = srv_tx.clone();
    builder = builder.on_phase(move |from: &str, to: &str| {
        let _ = tx.send(ServerMsg::Phase(PhaseMsg {
            from: from.to_string(),
            to: to.to_string(),
            reason: "guard".into(),
        }));
    });

    // --- Connect ---

    let project = std::env::var("GOOGLE_CLOUD_PROJECT").unwrap_or_default();
    let location = std::env::var("GOOGLE_CLOUD_LOCATION").unwrap_or_else(|_| "us-central1".into());

    let handle = match get_access_token().await {
        Ok(token) => {
            match builder.connect_vertex(&project, &location, &token).await {
                Ok(h) => h,
                Err(e) => {
                    let _ = srv_tx.send(ServerMsg::Control(ControlServerMsg::Error {
                        message: format!("Connect failed: {e}"),
                    }));
                    return None;
                }
            }
        }
        Err(e) => {
            let _ = srv_tx.send(ServerMsg::Control(ControlServerMsg::Error {
                message: format!("Auth failed: {e}"),
            }));
            return None;
        }
    };

    // Send connected message
    let _ = srv_tx.send(ServerMsg::Control(ControlServerMsg::Connected {
        session_id: format!("{:016x}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()),
        agent: agent_name.to_string(),
    }));

    // Start periodic telemetry flush
    let telemetry = handle.telemetry().clone();
    let tx = srv_tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
        loop {
            interval.tick().await;
            let snapshot = telemetry.snapshot();
            let _ = tx.send(ServerMsg::Telemetry(TelemetryMsg { stats: snapshot }));
        }
    });

    Some(handle)
}

async fn get_access_token() -> Result<String, String> {
    // Try env var first, then gcloud
    if let Ok(token) = std::env::var("GOOGLE_ACCESS_TOKEN") {
        return Ok(token);
    }
    let output = tokio::process::Command::new("gcloud")
        .args(["auth", "print-access-token"])
        .output()
        .await
        .map_err(|e| format!("gcloud not found: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err("gcloud auth failed".into())
    }
}

fn parse_model(s: &str) -> Option<GeminiModel> {
    // Map common model strings to GeminiModel variants
    match s {
        s if s.contains("flash-lite") => Some(GeminiModel::Gemini2_0FlashLite),
        s if s.contains("native-audio") => Some(GeminiModel::Gemini2_5FlashNativeAudioLive),
        s if s.contains("flash") => Some(GeminiModel::Gemini2_0FlashLive),
        _ => None,
    }
}

fn parse_voice(s: &str) -> Option<Voice> {
    match s.to_lowercase().as_str() {
        "puck" => Some(Voice::Puck),
        "charon" => Some(Voice::Charon),
        "kore" => Some(Voice::Kore),
        "fenrir" => Some(Voice::Fenrir),
        "aoede" => Some(Voice::Aoede),
        _ => None,
    }
}
```

**Step 2: Update `server.rs` to use the session module**

Replace the TODO stubs in `handle_ws` with calls to `session::run_session()`. Wire binary mic frames to `handle.send_audio()` and text messages to `handle.send_text()`.

The key change in `handle_ws`:
- On `Start` message → call `session::run_session()` → get `LiveHandle`
- On binary `CHANNEL_MIC` → `handle.send_audio(&data[1..])`
- On text `Text { text }` → `handle.send_text(&text)`
- On `Stop` or disconnect → `handle.disconnect()`

**Step 3: Wire up in lib.rs**

Add `pub mod session;` to `lib.rs`.

**Step 4: Verify**

Run: `cargo check -p adk-devtools`

**Step 5: Commit**

```bash
git add crates/adk-devtools/src/session.rs crates/adk-devtools/src/server.rs
git commit -m "feat(adk-devtools): wire WebSocket handler to Live sessions with binary audio"
```

---

## Task 5: `run()` entry point and `prelude` module

**Files:**
- Create: `crates/adk-devtools/src/prelude.rs`
- Modify: `crates/adk-devtools/src/lib.rs`

The `run()` function is the single entry point users call from their `main()`. It reads `ADK_MODE` and dispatches to the right mode.

**Step 1: Create `crates/adk-devtools/src/prelude.rs`**

```rust
//! Prelude — re-exports for ergonomic agent registration.

pub use crate::registry::{AgentCategory, AgentDescriptor, AgentRegistry, Feature, LiveFactory};
pub use crate::protocol::AgentInfo;

// Re-export L2 fluent API
pub use adk_rs_fluent::prelude::*;
```

**Step 2: Add `run()` to `lib.rs`**

```rust
//! Dev tools for gemini-rs: DevServer, REPL, and binary WebSocket protocol.

#![warn(missing_docs)]

pub mod prelude;
pub mod protocol;
pub mod registry;
pub mod server;
pub mod session;

use registry::{AgentDescriptor, AgentRegistry};

/// Run the devtools server or REPL based on `ADK_MODE` env var.
///
/// - `ADK_MODE=web` (default): Start Axum server with dev UI
/// - `ADK_MODE=run`: Start interactive REPL for agent named in `ADK_AGENT`
///
/// # Example
/// ```ignore
/// use adk_devtools::prelude::*;
///
/// fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let agent = AgentDescriptor { ... };
///     adk_devtools::run(vec![agent])
/// }
/// ```
#[tokio::main]
pub async fn run(agents: Vec<AgentDescriptor>) -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    dotenvy::dotenv().ok();

    let mut registry = AgentRegistry::new();
    for agent in agents {
        registry.register(agent);
    }

    let mode = std::env::var("ADK_MODE").unwrap_or_else(|_| "web".into());

    match mode.as_str() {
        "web" => {
            let host = std::env::var("ADK_HOST").unwrap_or_else(|_| "127.0.0.1".into());
            let port: u16 = std::env::var("ADK_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(8000);
            server::serve(registry, &host, port).await?;
        }
        "run" => {
            let agent = std::env::var("ADK_AGENT")
                .map_err(|_| "ADK_AGENT env var required for run mode")?;
            // TODO: Task 6 implements the REPL
            eprintln!("REPL mode for '{agent}' — coming in Task 6");
        }
        other => {
            eprintln!("Unknown ADK_MODE: {other}. Use 'web' or 'run'.");
            std::process::exit(1);
        }
    }

    Ok(())
}
```

**Step 3: Add `tracing-subscriber` dependency**

Add to `crates/adk-devtools/Cargo.toml`:
```toml
tracing-subscriber = "0.3"
```

**Step 4: Verify**

Run: `cargo check -p adk-devtools`

**Step 5: Commit**

```bash
git add crates/adk-devtools/
git commit -m "feat(adk-devtools): add run() entry point and prelude module"
```

---

## Task 6: `adk run` — Interactive text REPL

**Files:**
- Create: `crates/adk-devtools/src/repl.rs`
- Modify: `crates/adk-devtools/src/lib.rs` (wire into `run()`)

**Step 1: Create `crates/adk-devtools/src/repl.rs`**

```rust
//! Interactive terminal REPL for testing agents via text input.

use std::sync::Arc;

use bytes::Bytes;
use rustyline::DefaultEditor;
use tokio::sync::mpsc;
use tracing::info;

use crate::protocol::*;
use crate::registry::AgentRegistry;
use crate::session;

/// Run the interactive REPL for the named agent.
pub async fn run_repl(agent_name: &str, registry: &AgentRegistry) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!();
    eprintln!("  gemini-rs devtools");
    eprintln!("  Agent: {agent_name}");
    eprintln!("  Type 'exit' to quit, '/state' to dump state");
    eprintln!();

    let (srv_tx, mut srv_rx) = mpsc::unbounded_channel::<ServerMsg>();
    let (audio_tx, _audio_rx) = mpsc::unbounded_channel::<Bytes>();

    // Connect the agent
    let handle = session::run_session(agent_name, registry, None, None, srv_tx.clone(), audio_tx)
        .await
        .ok_or_else(|| format!("Failed to start agent '{agent_name}'"))?;

    // Spawn receiver that prints server messages
    let print_handle = tokio::spawn(async move {
        while let Some(msg) = srv_rx.recv().await {
            match msg {
                ServerMsg::Text(TextMsg::Delta { delta }) => {
                    eprint!("{delta}");
                }
                ServerMsg::Text(TextMsg::Complete { text }) => {
                    eprintln!();
                }
                ServerMsg::Control(ControlServerMsg::TurnComplete) => {
                    eprintln!();
                }
                ServerMsg::Control(ControlServerMsg::Error { message }) => {
                    eprintln!("  ERROR: {message}");
                }
                ServerMsg::State(StateMsg { key, value }) => {
                    eprintln!("  state: {key} = {value}");
                }
                ServerMsg::Phase(PhaseMsg { from, to, .. }) => {
                    eprintln!("  phase: {from} → {to}");
                }
                ServerMsg::Control(ControlServerMsg::Connected { agent, .. }) => {
                    eprintln!("[{agent}]: (connected)");
                }
                _ => {}
            }
        }
    });

    // REPL loop
    let mut rl = DefaultEditor::new()?;
    loop {
        match rl.readline("[you]: ") {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() { continue; }
                if trimmed == "exit" || trimmed == "/quit" { break; }
                if trimmed == "/state" {
                    let state = handle.state();
                    for key in state.keys() {
                        if let Some(val) = state.get_raw(&key) {
                            eprintln!("  {key} = {val}");
                        }
                    }
                    continue;
                }
                if trimmed == "/phase" {
                    eprintln!("  current phase: {}", handle.phase().unwrap_or_default());
                    continue;
                }
                rl.add_history_entry(trimmed)?;
                eprint!("[{}]: ", "agent");
                handle.send_text(trimmed).await;
            }
            Err(_) => break,
        }
    }

    handle.disconnect().await;
    print_handle.abort();
    eprintln!("\nDisconnected.");
    Ok(())
}
```

**Step 2: Wire into `run()` in lib.rs**

Replace the TODO in the `"run"` match arm:
```rust
"run" => {
    let agent = std::env::var("ADK_AGENT")
        .map_err(|_| "ADK_AGENT env var required for run mode")?;
    repl::run_repl(&agent, &registry).await?;
}
```

Add `pub mod repl;` to `lib.rs`.

**Step 3: Verify**

Run: `cargo check -p adk-devtools`

**Step 4: Commit**

```bash
git add crates/adk-devtools/src/repl.rs crates/adk-devtools/src/lib.rs
git commit -m "feat(adk-devtools): add interactive text REPL (adk run)"
```

---

## Task 7: Dev UI — vanilla JS/CSS embedded assets

**Files:**
- Create: `crates/adk-devtools/ui/index.html`
- Create: `crates/adk-devtools/ui/app.js`
- Create: `crates/adk-devtools/ui/audio.js`
- Create: `crates/adk-devtools/ui/devtools.js`
- Create: `crates/adk-devtools/ui/styles.css`
- Create: `crates/adk-devtools/ui/audio-worklet.js`

This is the biggest task. The UI has three panels: agent sidebar, conversation, and devtools.

**Reference files:**
- `cookbooks/ui/static/js/app.js` — reuse WebSocket lifecycle, upgrade to binary protocol
- `cookbooks/ui/static/js/audio.js` — reuse recording/playback, upgrade to AudioWorklet + binary frames
- `cookbooks/ui/static/js/devtools.js` — reuse state grouping, event logging, telemetry

**Key differences from cookbook UI:**

1. **Agent sidebar** — new panel listing agents from `/api/agents`, click to connect
2. **Binary audio** — `ws.send(arrayBuffer)` with channel prefix instead of base64 JSON
3. **AudioWorklet** — replaces deprecated ScriptProcessorNode
4. **Channel-tagged JSON** — messages have `ch` field instead of `type` at top level
5. **Three-column layout** — sidebar + conversation + devtools

**Step 1: Create `ui/index.html`**

Three-column layout with sidebar, conversation panel, and collapsible devtools panel. Link to `app.js`, `devtools.js`, `audio.js`, and `styles.css` via `/static/` paths.

**Step 2: Create `ui/styles.css`**

Three-column grid layout. Dark theme matching adk-js dev UI. Responsive.

**Step 3: Create `ui/audio-worklet.js`**

AudioWorklet processor for mic capture at 16kHz. Converts Float32 to PCM16 and posts to main thread.

**Step 4: Create `ui/audio.js`**

Audio manager class. Recording via AudioWorklet → binary frames. Playback via AudioWorklet at 24kHz. `clearQueue()` for interruptions.

**Step 5: Create `ui/app.js`**

Main app class:
- Fetch `/api/agents` → render sidebar
- Click agent → open WebSocket to `/ws/{name}`
- Binary frame routing: `ws.onmessage` checks `typeof data` — `ArrayBuffer` for binary, `string` for JSON
- JSON routing: `switch (msg.ch)` → control, text, transcript, state, phase, tool, telemetry
- Binary routing: `data[0]` — `0x02` = audio out → play
- Send text: `ws.send(JSON.stringify({ch: "control", type: "text", text}))`
- Send audio: `ws.send(new Uint8Array([0x01, ...pcm16]))` (binary frame)

**Step 6: Create `ui/devtools.js`**

Devtools panel manager — reuse and refactor from `cookbooks/ui/static/js/devtools.js`:
- State tab: grouped by prefix, highlights on change
- Events tab: chronological log
- Phases tab: timeline visualization
- Tools tab: tool call records
- Telemetry tab: live metrics

**Step 7: Verify embedded assets load**

Run: `cargo check -p adk-devtools`

The dev UI is not testable via `cargo test` — it requires manual verification with a browser. Document: "Start with `cargo run -p adk-devtools` (or a test binary) and open http://localhost:8000 in a browser."

**Step 8: Commit**

```bash
git add crates/adk-devtools/ui/
git commit -m "feat(adk-devtools): add embedded dev UI with binary WebSocket protocol"
```

---

## Task 8: Scaffold `adk-cli` binary crate

**Files:**
- Create: `tools/adk-cli/Cargo.toml`
- Create: `tools/adk-cli/src/main.rs`
- Modify: `Cargo.toml` (workspace members)

**Step 1: Create `tools/adk-cli/Cargo.toml`**

```toml
[package]
name = "adk-cli"
version = "0.1.0"
edition.workspace = true
license.workspace = true
repository.workspace = true
description = "CLI for gemini-rs agent development"

[[bin]]
name = "adk"
path = "src/main.rs"

[dependencies]
clap = { version = "4", features = ["derive"] }
```

**Step 2: Create `tools/adk-cli/src/main.rs`**

```rust
//! `adk` CLI — thin wrapper that sets env vars and runs `cargo run`.

use std::process::Command;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "adk", about = "gemini-rs agent development kit")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the devtools server with embedded UI.
    Web {
        /// Directory containing the agent project (default: current dir).
        #[arg(default_value = ".")]
        dir: String,
        /// Host to bind to.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port to bind to.
        #[arg(long, default_value = "8000")]
        port: u16,
    },
    /// Run an agent interactively in the terminal.
    Run {
        /// Agent name to run.
        agent: String,
        /// Directory containing the agent project (default: current dir).
        #[arg(long, default_value = ".")]
        dir: String,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Web { dir, host, port } => {
            let status = Command::new("cargo")
                .arg("run")
                .current_dir(&dir)
                .env("ADK_MODE", "web")
                .env("ADK_HOST", &host)
                .env("ADK_PORT", port.to_string())
                .status()
                .expect("Failed to run cargo");
            std::process::exit(status.code().unwrap_or(1));
        }
        Commands::Run { agent, dir } => {
            let status = Command::new("cargo")
                .arg("run")
                .current_dir(&dir)
                .env("ADK_MODE", "run")
                .env("ADK_AGENT", &agent)
                .status()
                .expect("Failed to run cargo");
            std::process::exit(status.code().unwrap_or(1));
        }
    }
}
```

**Step 3: Add to workspace**

Add `"tools/adk-cli"` to the workspace members.

**Step 4: Verify**

Run: `cargo check -p adk-cli`
Run: `cargo run -p adk-cli -- --help`
Expected: Shows help with `web` and `run` subcommands.

**Step 5: Commit**

```bash
git add tools/adk-cli/ Cargo.toml
git commit -m "feat(adk-cli): scaffold CLI binary with web and run subcommands"
```

---

## Task 9: Integration test — register agent and start server

**Files:**
- Create: `crates/adk-devtools/tests/smoke.rs`
- Create: `crates/adk-devtools/examples/basic.rs`

**Step 1: Create a basic example**

Create `crates/adk-devtools/examples/basic.rs` that registers a minimal agent and starts the server. This serves as both documentation and smoke test:

```rust
//! Basic example: register an agent and start the devtools server.
//!
//! Run: `cargo run -p adk-devtools --example basic`

use std::sync::Arc;
use adk_devtools::prelude::*;
use adk_devtools::registry::{AgentCategory, AgentDescriptor, Feature};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent = AgentDescriptor {
        name: "echo-agent".into(),
        description: "Echoes your input back".into(),
        category: AgentCategory::Basic,
        features: vec![Feature::Voice, Feature::Text],
        factory: Arc::new(|| {
            Live::builder()
                .model(GeminiModel::Gemini2_0FlashLive)
                .instruction("Echo back whatever the user says.")
        }),
    };

    adk_devtools::run(vec![agent])
}
```

**Step 2: Create a smoke test for the registry and protocol**

Create `crates/adk-devtools/tests/smoke.rs`:

```rust
use adk_devtools::protocol::*;
use adk_devtools::registry::*;

#[test]
fn registry_register_and_list() {
    let mut reg = AgentRegistry::new();
    reg.register(AgentDescriptor {
        name: "test".into(),
        description: "A test agent".into(),
        category: AgentCategory::Basic,
        features: vec![Feature::Voice],
        factory: std::sync::Arc::new(|| {
            adk_rs_fluent::live::Live::builder()
        }),
    });

    let list = reg.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "test");
    assert_eq!(list[0].category, "basic");
}

#[test]
fn protocol_serialize_server_msg() {
    let msg = ServerMsg::Control(ControlServerMsg::Connected {
        session_id: "abc".into(),
        agent: "test".into(),
    });
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("\"ch\":\"control\""));
    assert!(json.contains("\"type\":\"connected\""));
}

#[test]
fn protocol_deserialize_client_msg() {
    let json = r#"{"ch":"control","type":"start","agent":"billing"}"#;
    let msg: ClientMsg = serde_json::from_str(json).unwrap();
    match msg {
        ClientMsg::Control(ControlClientMsg::Start { agent, .. }) => {
            assert_eq!(agent, "billing");
        }
        _ => panic!("Expected Start"),
    }
}

#[test]
fn protocol_binary_channel_constants() {
    assert_eq!(CHANNEL_MIC, 0x01);
    assert_eq!(CHANNEL_AUDIO_OUT, 0x02);
    assert_eq!(CHANNEL_VIDEO, 0x03);
}
```

**Step 3: Run tests**

Run: `cargo test -p adk-devtools --lib --tests`
Expected: All tests pass.

**Step 4: Commit**

```bash
git add crates/adk-devtools/tests/ crates/adk-devtools/examples/
git commit -m "test(adk-devtools): add smoke tests and basic example"
```

---

## Task 10: Final wiring, docs, and CI check

**Files:**
- Modify: `crates/adk-devtools/src/lib.rs` (doc comments)
- Modify: `.github/workflows/docs.yml` (add adk-devtools to doc check)
- Verify: Full workspace builds, tests pass, docs build

**Step 1: Ensure all public items have doc comments**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc -p adk-devtools --no-deps 2>&1 | head -20`

Fix any missing doc warnings.

**Step 2: Run full workspace checks**

```bash
cargo fmt --check
cargo clippy --workspace -- -D warnings
cargo test --workspace --lib
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

All must pass.

**Step 3: Verify the devtools server starts**

Run (no credentials needed to START, only to CONNECT):
```bash
ADK_MODE=web cargo run -p adk-devtools --example basic
```

Expected: Server starts on `http://127.0.0.1:8000`, serves the placeholder UI.

**Step 4: Commit any fixes**

```bash
git add -A
git commit -m "docs(adk-devtools): add doc comments, verify CI compliance"
```

---

## Summary

| Task | What it builds | Key files |
|------|---------------|-----------|
| 1 | Crate scaffold + protocol types | `Cargo.toml`, `protocol.rs` |
| 2 | Agent registry + descriptor | `registry.rs` |
| 3 | Axum DevServer skeleton | `server.rs`, placeholder `ui/` |
| 4 | Wire WebSocket to Live sessions | `session.rs` |
| 5 | `run()` entry point + prelude | `lib.rs`, `prelude.rs` |
| 6 | Interactive text REPL | `repl.rs` |
| 7 | Dev UI (vanilla JS/CSS) | `ui/index.html`, `app.js`, `audio.js`, `devtools.js` |
| 8 | `adk` CLI binary | `tools/adk-cli/` |
| 9 | Smoke tests + example | `tests/smoke.rs`, `examples/basic.rs` |
| 10 | Docs, CI, final verification | Doc comments, CI check |

Tasks 1–6 and 8 are backend Rust. Task 7 is frontend JS/CSS. Task 9 is tests. Task 10 is polish.

Tasks 1–5 are sequential (each depends on the previous). Task 6 depends on 5. Task 7 depends on 3. Task 8 depends on 5. Task 9 depends on all. Task 10 is last.
