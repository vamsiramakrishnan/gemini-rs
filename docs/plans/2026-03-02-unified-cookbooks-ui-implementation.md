# Unified Web UI — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Single-binary router-based demo hub consolidating existing demos and adding advanced apps showcasing out-of-band control driven by Live API events.

**Architecture:** Axum server with `CookbookApp` trait, `AppRegistry`, route-based app selection. Each app is a module implementing `handle_session()`. Frontend: landing page with app cards + per-app screen with conversation pane and collapsible devtools panel.

**Tech Stack:** Rust (axum, tokio, serde_json), gemini-genai-rs, gemini-adk-rs, gemini-adk-fluent-rs, HTML/CSS/JS (vanilla)

**Working directory:** `/home/user/gemini-genai-rs`

---

## Phase 1: Backend Infrastructure (Tasks 1-4)

### Task 1: CookbookApp trait + AppRegistry

**Files:**
- Create: `apps/gemini-adk-web-rs/src/app.rs`
- Modify: `apps/gemini-adk-web-rs/src/main.rs`
- Modify: `apps/gemini-adk-web-rs/Cargo.toml`

**Step 1: Create `app.rs`**

```rust
use std::sync::Arc;
use serde::Serialize;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AppCategory {
    Basic,
    Advanced,
    Showcase,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppMeta {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub category: AppCategory,
    pub features: &'static [&'static str],
}

/// Configuration passed to each app's session handler.
#[derive(Clone)]
pub struct AppConfig {
    pub auth: super::AuthConfig,
    pub model: Option<String>,
    pub voice: Option<String>,
    pub system_instruction: Option<String>,
}

/// Sender half for pushing JSON messages to the browser WebSocket.
pub type WsSender = mpsc::Sender<serde_json::Value>;

/// Each demo app implements this trait.
#[async_trait::async_trait]
pub trait CookbookApp: Send + Sync {
    fn meta(&self) -> AppMeta;
    async fn handle_session(
        &self,
        config: AppConfig,
        ws_tx: WsSender,
        mut ws_rx: mpsc::Receiver<serde_json::Value>,
    );
}

/// Registry of all available apps.
pub struct AppRegistry {
    apps: Vec<Arc<dyn CookbookApp>>,
}

impl AppRegistry {
    pub fn new() -> Self {
        Self { apps: Vec::new() }
    }

    pub fn register(&mut self, app: impl CookbookApp + 'static) {
        self.apps.push(Arc::new(app));
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn CookbookApp>> {
        self.apps.iter().find(|a| a.meta().name == name).cloned()
    }

    pub fn list(&self) -> Vec<AppMeta> {
        self.apps.iter().map(|a| a.meta()).collect()
    }
}
```

**Step 2: Add `async-trait` dependency to Cargo.toml**

Add `async-trait = "0.1"` to `[dependencies]`.

**Step 3: Add `mod app;` to main.rs** (just the module declaration, don't restructure yet).

**Step 4: Verify compilation**

Run: `cargo check -p gemini-genai-ui`

---

### Task 2: Refactor main.rs — Router + shared WS handler

**Files:**
- Modify: `apps/gemini-adk-web-rs/src/main.rs`

Replace the single `/ws` route with `/ws/{name}` and add `/api/apps` endpoint. Keep existing `AuthConfig` and move message types into a shared module.

**Step 1: Add route for app listing and per-app WebSocket**

Replace the router setup in `main()`:

```rust
let registry = {
    let mut r = app::AppRegistry::new();
    // Apps registered in later tasks
    r
};
let registry = Arc::new(registry);

let app = Router::new()
    .route("/api/apps", get({
        let reg = registry.clone();
        move || async move {
            axum::Json(reg.list())
        }
    }))
    .route("/ws/{name}", get({
        let reg = registry.clone();
        move |path: axum::extract::Path<String>, ws: WebSocketUpgrade, State(state): State<AppState>| {
            let reg = reg.clone();
            async move {
                let name = path.0;
                if let Some(app_impl) = reg.get(&name) {
                    ws.on_upgrade(move |socket| handle_app_socket(socket, state, app_impl))
                } else {
                    // Return 404 for unknown app
                    (axum::http::StatusCode::NOT_FOUND, "App not found").into_response()
                }
            }
        }
    }))
    .fallback_service(ServeDir::new(static_dir).fallback(ServeFile::new(format!("{}/index.html", static_dir))))
    .layer(CorsLayer::permissive())
    .with_state(state);
```

**Step 2: Add `handle_app_socket` function**

Generic handler that bridges browser WebSocket ↔ app's `handle_session()`:

```rust
async fn handle_app_socket(
    socket: WebSocket,
    state: AppState,
    app: Arc<dyn app::CookbookApp>,
) {
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Channel: app → browser
    let (tx_to_browser, mut rx_to_browser) = mpsc::channel::<serde_json::Value>(100);
    // Channel: browser → app
    let (tx_to_app, rx_to_app) = mpsc::channel::<serde_json::Value>(100);

    // Forward app messages to browser WebSocket
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx_to_browser.recv().await {
            if let Ok(json) = serde_json::to_string(&msg) {
                if ws_sender.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
        }
    });

    // Forward browser messages to app channel
    let tx_to_app_clone = tx_to_app.clone();
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(Message::Text(text))) = ws_receiver.next().await {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                if tx_to_app_clone.send(value).await.is_err() {
                    break;
                }
            }
        }
    });

    // Wait for "start" message to get config, then delegate to app
    // The app receives the rx_to_app channel and handles all messages
    let config = app::AppConfig {
        auth: state.auth,
        model: None,
        voice: None,
        system_instruction: None,
    };

    app.handle_session(config, tx_to_browser, rx_to_app).await;

    send_task.abort();
    recv_task.abort();
}
```

**Step 3: Remove old `ws_handler` and `handle_socket` functions** — they are replaced by the new routing.

**Step 4: Verify compilation**

Run: `cargo check -p gemini-genai-ui`

---

### Task 3: Voice Chat app (port existing main.rs logic)

**Files:**
- Create: `apps/gemini-adk-web-rs/src/apps/mod.rs`
- Create: `apps/gemini-adk-web-rs/src/apps/voice_chat.rs`

This ports the existing `handle_socket` logic into a `CookbookApp` implementation.

**Step 1: Create `apps/mod.rs`**

```rust
pub mod voice_chat;

pub use voice_chat::VoiceChatApp;
```

**Step 2: Create `apps/voice_chat.rs`**

Port the existing `handle_socket` logic, using `Live::builder()` from the fluent API:

```rust
use crate::app::{AppCategory, AppConfig, AppMeta, CookbookApp, WsSender};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use gemini_genai_rs::prelude::*;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{error, info};

pub struct VoiceChatApp;

#[async_trait::async_trait]
impl CookbookApp for VoiceChatApp {
    fn meta(&self) -> AppMeta {
        AppMeta {
            name: "voice-chat",
            title: "Voice Chat",
            description: "Native audio conversation with transcription",
            category: AppCategory::Basic,
            features: &["voice", "transcription"],
        }
    }

    async fn handle_session(
        &self,
        config: AppConfig,
        ws_tx: WsSender,
        mut ws_rx: mpsc::Receiver<serde_json::Value>,
    ) {
        // Wait for "start" message
        let start_msg = match ws_rx.recv().await {
            Some(msg) if msg.get("type").and_then(|t| t.as_str()) == Some("start") => msg,
            _ => return,
        };

        // Extract config from start message
        let model_str = start_msg.get("model").and_then(|v| v.as_str()).unwrap_or("gemini-genai-2.5-flash-native-audio");
        let voice_str = start_msg.get("voice").and_then(|v| v.as_str()).unwrap_or("Aoede");
        let sys_instr = start_msg.get("system_instruction").and_then(|v| v.as_str());

        let voice = match voice_str {
            "Puck" => Voice::Puck,
            "Charon" => Voice::Charon,
            "Kore" => Voice::Kore,
            "Fenrir" => Voice::Fenrir,
            _ => Voice::Aoede,
        };

        let model = match model_str {
            "gemini-2.0-flash-live-001" => GeminiModel::Gemini2_0FlashLive,
            "gemini-genai-2.5-flash-native-audio" => GeminiModel::GeminiLive2_5FlashNativeAudio,
            other => GeminiModel::Custom(other.to_string()),
        };

        // Build Live session with fluent API
        let tx = ws_tx.clone();
        let tx2 = ws_tx.clone();
        let tx3 = ws_tx.clone();
        let tx4 = ws_tx.clone();
        let tx5 = ws_tx.clone();
        let tx6 = ws_tx.clone();
        let tx7 = ws_tx.clone();

        let mut builder = gemini_adk_fluent_rs::Live::builder()
            .model(model)
            .voice(voice)
            .transcription(true, true)
            .on_audio(move |data| {
                let b64 = BASE64.encode(data);
                let _ = tx.try_send(json!({"type": "audio", "data": b64}));
            })
            .on_text(move |t| {
                let _ = tx2.try_send(json!({"type": "textDelta", "text": t}));
            })
            .on_text_complete(move |t| {
                let _ = tx3.try_send(json!({"type": "textComplete", "text": t}));
            })
            .on_input_transcript(move |t, _is_final| {
                let _ = tx4.try_send(json!({"type": "inputTranscription", "text": t}));
            })
            .on_output_transcript(move |t, _is_final| {
                let _ = tx5.try_send(json!({"type": "outputTranscription", "text": t}));
            })
            .on_vad_start(move || {
                let _ = tx6.try_send(json!({"type": "voiceActivityStart"}));
            })
            .on_vad_end(move || {
                let _ = tx7.try_send(json!({"type": "voiceActivityEnd"}));
            })
            .on_interrupted({
                let tx = ws_tx.clone();
                move || { let tx = tx.clone(); async move { let _ = tx.send(json!({"type": "interrupted"})).await; } }
            })
            .on_turn_complete({
                let tx = ws_tx.clone();
                move || { let tx = tx.clone(); async move { let _ = tx.send(json!({"type": "turnComplete"})).await; } }
            })
            .on_error({
                let tx = ws_tx.clone();
                move |e| { let tx = tx.clone(); async move { let _ = tx.send(json!({"type": "error", "message": e})).await; } }
            });

        if let Some(sys) = sys_instr {
            builder = builder.instruction(sys);
        }

        // Connect based on auth config
        let handle = match &config.auth {
            crate::AuthConfig::GoogleAI { api_key } => {
                builder.connect_google_ai(api_key).await
            }
            crate::AuthConfig::VertexAI { project, location } => {
                match fetch_gcloud_token() {
                    Ok(token) => builder.connect_vertex(project, location, token).await,
                    Err(e) => {
                        let _ = ws_tx.send(json!({"type": "error", "message": e})).await;
                        return;
                    }
                }
            }
        };

        let handle = match handle {
            Ok(h) => h,
            Err(e) => {
                let _ = ws_tx.send(json!({"type": "error", "message": format!("{e}")})).await;
                return;
            }
        };

        let _ = ws_tx.send(json!({"type": "connected"})).await;

        // Process incoming messages from browser
        while let Some(msg) = ws_rx.recv().await {
            match msg.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(text) = msg.get("text").and_then(|t| t.as_str()) {
                        let _ = handle.send_text(text).await;
                    }
                }
                Some("audio") => {
                    if let Some(data) = msg.get("data").and_then(|d| d.as_str()) {
                        if let Ok(bytes) = BASE64.decode(data) {
                            let _ = handle.send_audio(bytes).await;
                        }
                    }
                }
                Some("stop") => {
                    let _ = handle.disconnect().await;
                    break;
                }
                _ => {}
            }
        }

        let _ = handle.disconnect().await;
    }
}

fn fetch_gcloud_token() -> Result<String, String> {
    match std::process::Command::new("gcloud")
        .args(["auth", "print-access-token"])
        .output()
    {
        Ok(output) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        _ => Err("Failed to fetch gcloud access token".to_string()),
    }
}
```

**Step 3: Register in main.rs**

```rust
mod apps;

// In main():
let registry = {
    let mut r = app::AppRegistry::new();
    r.register(apps::VoiceChatApp);
    r
};
```

**Step 4: Add `gemini-adk-fluent-rs` dependency to Cargo.toml**

```toml
gemini-adk-fluent-rs = { path = "../../crates/gemini-adk-fluent-rs" }
```

**Step 5: Verify compilation**

Run: `cargo check -p gemini-genai-ui`

---

### Task 4: Text Chat + Tool Calling apps

**Files:**
- Create: `apps/gemini-adk-web-rs/src/apps/text_chat.rs`
- Create: `apps/gemini-adk-web-rs/src/apps/tool_calling.rs`
- Modify: `apps/gemini-adk-web-rs/src/apps/mod.rs`
- Modify: `apps/gemini-adk-web-rs/src/main.rs` (register apps)

**Step 1: Create `text_chat.rs`**

Minimal text-only session — same as voice_chat but without audio callbacks, uses `.text_only()` response modality:

```rust
// Same pattern as VoiceChatApp but:
// - model defaults to Gemini2_0FlashLive (text model)
// - uses text_only() config (no audio output)
// - No on_audio callback
// - No on_vad_start/end
```

Key differences from voice_chat:
- `AppMeta { name: "text-chat", title: "Text Chat", features: &["text"] }`
- Default model: `Gemini2_0FlashLive` (not native audio)
- No audio-related callbacks

**Step 2: Create `tool_calling.rs`**

Uses `TypedTool` + `ToolDispatcher` from gemini-adk-rs:

```rust
// Define tool argument types with JsonSchema
// Create ToolDispatcher with weather + calculator tools
// Pass dispatcher via .tools(dispatcher) on Live::builder()
// Features: &["voice", "tools"]
```

Key: Register tools with `ToolDispatcher`, pass to `Live::builder().tools(dispatcher)`.

**Step 3: Update `apps/mod.rs`**

```rust
pub mod text_chat;
pub mod tool_calling;
pub mod voice_chat;

pub use text_chat::TextChatApp;
pub use tool_calling::ToolCallingApp;
pub use voice_chat::VoiceChatApp;
```

**Step 4: Register all three in main.rs**

```rust
r.register(apps::TextChatApp);
r.register(apps::VoiceChatApp);
r.register(apps::ToolCallingApp);
```

**Step 5: Add gemini-adk-rs + schemars dependencies to Cargo.toml**

```toml
gemini-adk-rs = { path = "../../crates/gemini-adk-rs" }
schemars = "0.8"
```

**Step 6: Verify compilation**

Run: `cargo check -p gemini-genai-ui`

---

## Phase 2: Frontend (Tasks 5-7)

### Task 5: Landing page

**Files:**
- Rewrite: `apps/gemini-adk-web-rs/static/index.html`
- Create: `apps/gemini-adk-web-rs/static/css/main.css`
- Create: `apps/gemini-adk-web-rs/static/css/landing.css`

**Step 1: Rewrite `index.html` as landing page**

Grid of app cards grouped by category. Fetches `/api/apps` on load.

```html
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Gemini Live RS — Examples</title>
    <link rel="stylesheet" href="/css/main.css">
    <link rel="stylesheet" href="/css/landing.css">
</head>
<body>
    <div class="landing">
        <header class="landing-header">
            <h1>Gemini Live RS</h1>
            <p>Interactive demos for speech-to-speech AI</p>
        </header>
        <div id="app-grid" class="app-grid"></div>
    </div>
    <script>
        fetch('/api/apps')
            .then(r => r.json())
            .then(apps => {
                const grid = document.getElementById('app-grid');
                const groups = { basic: [], advanced: [], showcase: [] };
                apps.forEach(a => groups[a.category]?.push(a));
                for (const [cat, list] of Object.entries(groups)) {
                    if (list.length === 0) continue;
                    const section = document.createElement('section');
                    section.innerHTML = `<h2>${cat.charAt(0).toUpperCase() + cat.slice(1)}</h2>`;
                    const cards = document.createElement('div');
                    cards.className = 'cards';
                    list.forEach(app => {
                        const card = document.createElement('a');
                        card.href = `/app.html?name=${app.name}`;
                        card.className = 'card';
                        card.innerHTML = `
                            <h3>${app.title}</h3>
                            <p>${app.description}</p>
                            <div class="badges">${app.features.map(f => `<span class="badge">${f}</span>`).join('')}</div>
                        `;
                        cards.appendChild(card);
                    });
                    section.appendChild(cards);
                    grid.appendChild(section);
                }
            });
    </script>
</body>
</html>
```

**Step 2: Create `css/main.css`**

Shared variables and base reset. Port `:root` vars and reset from existing `style.css`.

**Step 3: Create `css/landing.css`**

Grid layout, card styles, badges, responsive.

**Step 4: Remove old `style.css`** (replaced by css/ directory)

**Step 5: Verify** — open browser to `http://localhost:3000`, see landing page with cards.

---

### Task 6: App screen — conversation pane

**Files:**
- Create: `apps/gemini-adk-web-rs/static/app.html`
- Create: `apps/gemini-adk-web-rs/static/js/app.js`
- Create: `apps/gemini-adk-web-rs/static/js/audio.js`

**Step 1: Create `app.html`**

Two-pane layout. Left = conversation, right = devtools (collapsible). Reads `?name=` from URL.

```html
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>App — Gemini Live RS</title>
    <link rel="stylesheet" href="/css/main.css">
    <link rel="stylesheet" href="/css/app.css">
    <link rel="stylesheet" href="/css/devtools.css">
</head>
<body>
    <div class="app-layout">
        <!-- Left: Conversation pane -->
        <div class="conversation-pane">
            <header class="conv-header">
                <a href="/" class="back-link">← Back</a>
                <h2 id="app-title">App</h2>
                <div class="header-controls">
                    <!-- model/voice selects, connect/disconnect -->
                </div>
            </header>
            <div class="messages" id="messages"></div>
            <div id="speaking-indicator" class="speaking-indicator">Listening...</div>
            <div class="input-area">
                <button id="mic-btn" class="mic-btn" disabled>🎤</button>
                <input type="text" id="text-input" placeholder="Type a message..." disabled>
                <button id="send-btn" disabled>Send</button>
            </div>
        </div>
        <!-- Right: Devtools panel (collapsible) -->
        <div class="devtools-panel" id="devtools-panel">
            <!-- Populated by devtools.js -->
        </div>
    </div>
    <script src="/js/audio.js"></script>
    <script src="/js/app.js"></script>
    <script src="/js/devtools.js"></script>
</body>
</html>
```

**Step 2: Create `js/app.js`**

Port existing `app.js` logic. Key change: reads app name from URL `?name=`, connects to `/ws/{name}`. Dispatches devtools messages to `window.devtools` if present.

**Step 3: Create `js/audio.js`**

Extract audio recording + playback into separate file (from existing app.js).

**Step 4: Create `css/app.css`**

Port existing style.css conversation styles into app-specific stylesheet.

**Step 5: Remove old `app.js`** (replaced by js/ directory)

**Step 6: Verify** — click a card on landing page, arrive at app screen, connect + chat works.

---

### Task 7: Devtools panel

**Files:**
- Create: `apps/gemini-adk-web-rs/static/js/devtools.js`
- Create: `apps/gemini-adk-web-rs/static/css/devtools.css`

**Step 1: Create `js/devtools.js`**

Four tabs: State, Events, Playbook, Evaluator. Listens for extended WebSocket messages.

```javascript
class Devtools {
    constructor(container) {
        this.container = container;
        this.tabs = {};
        this.activeTab = 'events';
        this.render();
    }

    render() {
        this.container.innerHTML = `
            <div class="devtools-tabs">
                <button class="tab active" data-tab="events">Events</button>
                <button class="tab" data-tab="state">State</button>
                <button class="tab" data-tab="playbook">Playbook</button>
                <button class="tab" data-tab="evaluator">Evaluator</button>
            </div>
            <div class="tab-content" id="tab-content"></div>
        `;
        // Tab click handlers...
    }

    handleMessage(msg) {
        switch (msg.type) {
            case 'stateUpdate': this.updateState(msg.key, msg.value); break;
            case 'phaseChange': this.updatePlaybook(msg.from, msg.to, msg.reason); break;
            case 'evaluation': this.addEvaluation(msg); break;
            case 'violation': this.addViolation(msg); break;
        }
        this.addEvent(msg); // All messages go to events log
    }
}
```

**Step 2: Create `css/devtools.css`**

Tab bar, state key-value table, events log with timestamps/badges, playbook phase diagram, evaluator output.

**Step 3: Wire into `js/app.js`**

```javascript
// In app.js, after DOM ready:
const devtools = new Devtools(document.getElementById('devtools-panel'));

// In ws.onmessage handler, after existing cases:
devtools.handleMessage(msg);
```

**Step 4: Verify** — connect to voice-chat app, see Events tab filling with session events.

---

## Phase 3: Advanced Apps (Tasks 8-10)

### Task 8: Playbook Agent app

**Files:**
- Create: `apps/gemini-adk-web-rs/src/apps/playbook.rs`
- Modify: `apps/gemini-adk-web-rs/src/apps/mod.rs`
- Modify: `apps/gemini-adk-web-rs/src/main.rs`

This is the core advanced app. Uses `Live::builder()` with `extract_turns()`, `instruction_template()`, and `on_extracted()` to implement a state machine that controls a voice agent following a customer support playbook.

**Step 1: Define extraction schema**

```rust
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
struct PlaybookState {
    customer_name: Option<String>,
    issue_type: Option<String>,
    order_number: Option<String>,
    sentiment: Option<String>,
    resolution: Option<String>,
    phase_signals: Option<String>, // LLM's assessment of conversation progress
}
```

**Step 2: Define state machine phases**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
enum Phase {
    Greet,
    Identify,
    Investigate,
    Explain,
    Resolve,
    Close,
}

impl Phase {
    fn instruction(&self) -> &'static str {
        match self {
            Phase::Greet => "You are a friendly customer support agent. Greet the customer warmly and ask how you can help today. Be natural and conversational.",
            Phase::Identify => "Ask for the customer's name and order number. Be patient and helpful. Once you have both, acknowledge them.",
            Phase::Investigate => "Investigate the customer's issue by asking clarifying questions. Understand the problem thoroughly before suggesting solutions.",
            Phase::Explain => "Explain what happened and why. Be transparent and empathetic. Make sure the customer understands the situation.",
            Phase::Resolve => "Propose a resolution. Offer concrete next steps. Confirm the customer is satisfied with the solution.",
            Phase::Close => "Summarize what was discussed and resolved. Thank the customer. Ask if there's anything else you can help with.",
        }
    }

    fn can_transition_to(&self, next: Phase, state: &PlaybookState) -> bool {
        match (self, next) {
            (Phase::Greet, Phase::Identify) => true, // always allowed
            (Phase::Identify, Phase::Investigate) => state.customer_name.is_some(),
            (Phase::Investigate, Phase::Explain) => state.issue_type.is_some(),
            (Phase::Explain, Phase::Resolve) => true,
            (Phase::Resolve, Phase::Close) => state.resolution.is_some(),
            _ => false,
        }
    }

    fn next(&self) -> Option<Phase> {
        match self {
            Phase::Greet => Some(Phase::Identify),
            Phase::Identify => Some(Phase::Investigate),
            Phase::Investigate => Some(Phase::Explain),
            Phase::Explain => Some(Phase::Resolve),
            Phase::Resolve => Some(Phase::Close),
            Phase::Close => None,
        }
    }
}
```

**Step 3: Implement PlaybookApp**

Core pattern — `on_extracted` callback checks state machine transitions and sends devtools updates:

```rust
#[async_trait::async_trait]
impl CookbookApp for PlaybookApp {
    fn meta(&self) -> AppMeta {
        AppMeta {
            name: "playbook",
            title: "Playbook Agent",
            description: "State machine driven support agent with phase tracking",
            category: AppCategory::Advanced,
            features: &["voice", "state-machine", "extraction", "dynamic-instructions"],
        }
    }

    async fn handle_session(&self, config: AppConfig, ws_tx: WsSender, mut ws_rx: mpsc::Receiver<serde_json::Value>) {
        // Wait for start message...

        let current_phase = Arc::new(std::sync::Mutex::new(Phase::Greet));

        // Build Live session with extraction pipeline
        let handle = Live::builder()
            .model(model)
            .voice(voice)
            .instruction(Phase::Greet.instruction())
            .extract_turns::<PlaybookState>(
                extraction_llm, // text model for OOB extraction
                "Extract from this customer support conversation: customer_name, issue_type, order_number, sentiment, resolution status, and phase_signals (your assessment of where the conversation is)"
            )
            .on_extracted({
                let ws_tx = ws_tx.clone();
                let phase = current_phase.clone();
                move |name, value| {
                    let ws_tx = ws_tx.clone();
                    let phase = phase.clone();
                    async move {
                        // Send state update to devtools
                        let _ = ws_tx.send(json!({"type": "stateUpdate", "key": name, "value": value})).await;

                        // Check phase transitions
                        if let Ok(extracted) = serde_json::from_value::<PlaybookState>(value) {
                            let mut current = phase.lock().unwrap();
                            if let Some(next) = current.next() {
                                if current.can_transition_to(next, &extracted) {
                                    let from = format!("{:?}", *current);
                                    *current = next;
                                    let to = format!("{:?}", next);
                                    let _ = ws_tx.send(json!({
                                        "type": "phaseChange",
                                        "from": from,
                                        "to": to,
                                        "reason": "State conditions met"
                                    })).await;
                                }
                            }
                        }
                    }
                }
            })
            .instruction_template({
                let phase = current_phase.clone();
                move |_state| {
                    let p = phase.lock().unwrap();
                    Some(p.instruction().to_string())
                }
            })
            .connect_vertex(project, location, token)
            .await?;

        // ... message loop (same as voice_chat)
    }
}
```

**Step 4: Register app**

```rust
r.register(apps::PlaybookApp);
```

**Step 5: Verify compilation**

Run: `cargo check -p gemini-genai-ui`

---

### Task 9: Guardrails Agent app

**Files:**
- Create: `apps/gemini-adk-web-rs/src/apps/guardrails.rs`
- Modify: `apps/gemini-adk-web-rs/src/apps/mod.rs`
- Modify: `apps/gemini-adk-web-rs/src/main.rs`

Same architecture as playbook but monitors for policy violations instead of phase transitions.

**Step 1: Define extraction schema**

```rust
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
struct GuardrailState {
    contains_pii: Option<bool>,
    off_topic: Option<bool>,
    sentiment: Option<String>,
    safety_concern: Option<String>,
    topic_category: Option<String>,
}
```

**Step 2: Define policy rules**

```rust
struct PolicyRule {
    name: &'static str,
    check: fn(&GuardrailState) -> Option<Violation>,
    corrective_instruction: &'static str,
}
```

**Step 3: Implement GuardrailsApp**

- Uses `on_extracted` to check policy rules against extracted state
- Sends `violation` messages to devtools when rules are triggered
- Uses `instruction_template` to inject corrective instructions when violations detected

**Step 4: Register and verify**

---

### Task 10: Support Assistant app (multi-agent)

**Files:**
- Create: `apps/gemini-adk-web-rs/src/apps/support.rs`
- Modify: `apps/gemini-adk-web-rs/src/apps/mod.rs`
- Modify: `apps/gemini-adk-web-rs/src/main.rs`

**Step 1: Define multi-agent flow**

Chains playbook state machine with handoff logic. When state machine reaches escalation conditions, transitions to a different instruction set (simulating agent transfer).

**Step 2: Implement SupportApp**

Two instruction sets (billing flow, technical flow). Handoff detected via extraction. `instruction_template` switches entire persona.

**Step 3: Register and verify**

---

### Task 11: All Config app

**Files:**
- Create: `apps/gemini-adk-web-rs/src/apps/all_config.rs`
- Modify: `apps/gemini-adk-web-rs/src/apps/mod.rs`
- Modify: `apps/gemini-adk-web-rs/src/main.rs`

Showcase app exposing every Gemini Live config option in the UI. Extended start message includes all config fields. Frontend shows additional controls.

---

## Phase 4: Polish + README (Tasks 12-13)

### Task 12: Wire everything together + test all apps

**Files:**
- Modify: `apps/gemini-adk-web-rs/src/main.rs` (final registration)
- Modify: `apps/gemini-adk-web-rs/static/js/devtools.js` (tab visibility per category)

**Step 1: Final app registration in main.rs**

```rust
let registry = {
    let mut r = app::AppRegistry::new();
    r.register(apps::TextChatApp);
    r.register(apps::VoiceChatApp);
    r.register(apps::ToolCallingApp);
    r.register(apps::PlaybookApp);
    r.register(apps::GuardrailsApp);
    r.register(apps::SupportApp);
    r.register(apps::AllConfigApp);
    r
};
```

**Step 2: Devtools tab visibility**

Send `appMeta` message on connect. JS hides tabs not relevant to app category:
- Basic: Events only
- Advanced: Events + State + Playbook
- Showcase: All four tabs

**Step 3: Manual testing**

- Landing page shows all 7 apps grouped by category
- Each app card links to `/app.html?name=<name>`
- Voice chat: connect, speak, hear response, see transcriptions
- Playbook: see state extraction, phase transitions in devtools

**Step 4: Verify full build**

Run: `cargo build -p gemini-genai-ui`

---

### Task 13: Update README.md

**Files:**
- Modify: `README.md`

Update to reflect:
- New unified Web UI (`cargo run -p gemini-genai-ui`)
- App descriptions and categories
- Screenshot placeholder for landing page
- Out-of-band control pattern explanation
- Updated project structure

---

## Verification

1. `cargo build -p gemini-genai-ui` — compiles
2. `cargo run -p gemini-genai-ui` — server starts on port 3000
3. Landing page shows 7 app cards
4. Voice chat app works end-to-end
5. Playbook app shows state extraction + phase transitions in devtools

## Critical Files

| File | Action | What |
|------|--------|------|
| `apps/gemini-adk-web-rs/src/app.rs` | CREATE | CookbookApp trait, AppRegistry, AppConfig |
| `apps/gemini-adk-web-rs/src/apps/mod.rs` | CREATE | App module exports |
| `apps/gemini-adk-web-rs/src/apps/voice_chat.rs` | CREATE | Voice chat using Live::builder() |
| `apps/gemini-adk-web-rs/src/apps/text_chat.rs` | CREATE | Text-only chat |
| `apps/gemini-adk-web-rs/src/apps/tool_calling.rs` | CREATE | TypedTool demo |
| `apps/gemini-adk-web-rs/src/apps/playbook.rs` | CREATE | State machine + extraction |
| `apps/gemini-adk-web-rs/src/apps/guardrails.rs` | CREATE | Policy monitoring |
| `apps/gemini-adk-web-rs/src/apps/support.rs` | CREATE | Multi-agent handoff |
| `apps/gemini-adk-web-rs/src/apps/all_config.rs` | CREATE | Every config option |
| `apps/gemini-adk-web-rs/src/main.rs` | MODIFY | Router, registry, handle_app_socket |
| `apps/gemini-adk-web-rs/Cargo.toml` | MODIFY | Add gemini-adk-fluent-rs, gemini-adk-rs, async-trait, schemars |
| `apps/gemini-adk-web-rs/static/index.html` | REWRITE | Landing page with app cards |
| `apps/gemini-adk-web-rs/static/app.html` | CREATE | App screen with devtools |
| `apps/gemini-adk-web-rs/static/css/main.css` | CREATE | Shared styles |
| `apps/gemini-adk-web-rs/static/css/landing.css` | CREATE | Landing page styles |
| `apps/gemini-adk-web-rs/static/css/app.css` | CREATE | App screen styles |
| `apps/gemini-adk-web-rs/static/css/devtools.css` | CREATE | Devtools panel styles |
| `apps/gemini-adk-web-rs/static/js/app.js` | CREATE | Conversation logic |
| `apps/gemini-adk-web-rs/static/js/audio.js` | CREATE | Audio recording/playback |
| `apps/gemini-adk-web-rs/static/js/devtools.js` | CREATE | Devtools panel logic |
| `README.md` | MODIFY | Updated project docs |

## Estimated Total: ~2,500 LoC across 13 tasks
