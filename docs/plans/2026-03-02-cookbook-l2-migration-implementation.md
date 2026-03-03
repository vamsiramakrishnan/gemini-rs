# Cookbook L2 Migration Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Migrate all 7 cookbook/ui apps from raw L0 `ConnectBuilder` to L2 `Live::builder()` fluent API.

**Architecture:** Each app's `handle_session` keeps the same signature (`tx: WsSender, rx: Receiver<ClientMessage>`) but replaces the manual `tokio::select!` event loop with L2 callbacks for Gemini->browser and a simple recv loop for browser->Gemini. Advanced apps replace hand-coded state machines with `PhaseMachine`, regex-based state extraction moves into a `TurnExtractor` implementation, and violation detection moves to watchers.

**Tech Stack:** `adk-rs-fluent` (L2 fluent builder), `rs-adk` (L1 modules: PhaseMachine, WatcherRegistry, TurnExtractor, ComputedRegistry), `rs-genai` (L0 wire protocol)

**Design doc:** `docs/plans/2026-03-02-cookbook-l2-migration-design.md`

---

### Task 1: Add adk-rs-fluent dependency and shared helpers

**Files:**
- Modify: `cookbooks/ui/Cargo.toml`
- Modify: `cookbooks/ui/src/apps/mod.rs`

**Step 1: Add adk-rs-fluent to Cargo.toml**

In `cookbooks/ui/Cargo.toml`, add after the `rs-adk` line:

```toml
adk-rs-fluent = { path = "../../crates/adk-rs-fluent" }
```

**Step 2: Add resolve_voice to shared mod.rs**

In `cookbooks/ui/src/apps/mod.rs`, add a shared `resolve_voice` function (currently duplicated in voice_chat, playbook, guardrails, support, all_config). Add this after the `ConversationBuffer` impl:

```rust
use rs_genai::prelude::Voice;

/// Resolve a voice name string to the Voice enum.
pub fn resolve_voice(name: Option<&str>) -> Voice {
    match name {
        Some("Aoede") => Voice::Aoede,
        Some("Charon") => Voice::Charon,
        Some("Fenrir") => Voice::Fenrir,
        Some("Kore") => Voice::Kore,
        Some("Puck") | None => Voice::Puck,
        Some(other) => Voice::Custom(other.to_string()),
    }
}
```

**Step 3: Verify it compiles**

Run: `cargo check -p rs-genai-ui`
Expected: Compiles with unused import warnings (OK for now)

**Step 4: Commit**

```
feat(cookbooks): add adk-rs-fluent dependency and shared resolve_voice
```

---

### Task 2: Migrate text_chat.rs (simplest app, validates pattern)

**Files:**
- Modify: `cookbooks/ui/src/apps/text_chat.rs`

**Step 1: Rewrite text_chat.rs**

Replace the entire `handle_session` body. The CookbookApp trait impl (name, description, etc.) stays unchanged. Key change: replace `ConnectBuilder + tokio::select!` with `Live::builder() + callbacks + simple recv loop`.

```rust
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::{info, warn};

use adk_rs_fluent::prelude::*;

use crate::app::{AppCategory, AppError, ClientMessage, CookbookApp, ServerMessage, WsSender};

use super::{build_session_config, send_app_meta, wait_for_start};

/// Minimal text-only Gemini Live session.
pub struct TextChat;

#[async_trait]
impl CookbookApp for TextChat {
    fn name(&self) -> &str { "text-chat" }
    fn description(&self) -> &str { "Minimal text-only Gemini Live session" }
    fn category(&self) -> AppCategory { AppCategory::Basic }
    fn features(&self) -> Vec<String> { vec!["text".into()] }

    fn tips(&self) -> Vec<String> {
        vec![
            "Text-only mode — no microphone needed".into(),
            "Watch the streaming text deltas arrive in real time".into(),
        ]
    }

    fn try_saying(&self) -> Vec<String> {
        vec![
            "What are three interesting facts about octopuses?".into(),
            "Explain quantum computing in simple terms".into(),
            "Write a short poem about Rust programming".into(),
        ]
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        let start = wait_for_start(&mut rx).await?;

        let config = build_session_config(start.model.as_deref())
            .map_err(|e| AppError::Connection(e.to_string()))?
            .text_only()
            .system_instruction(
                start.system_instruction.as_deref()
                    .unwrap_or("You are a helpful assistant."),
            );

        // Set up L2 callbacks for Gemini -> Browser
        let tx_text = tx.clone();
        let tx_complete = tx.clone();
        let tx_turn = tx.clone();
        let tx_interrupt = tx.clone();
        let tx_err = tx.clone();
        let tx_dc = tx.clone();

        let handle = Live::builder()
            .on_text(move |t| { let _ = tx_text.send(ServerMessage::TextDelta { text: t.to_string() }); })
            .on_text_complete(move |t| { let _ = tx_complete.send(ServerMessage::TextComplete { text: t.to_string() }); })
            .on_turn_complete(move || { let tx = tx_turn.clone(); async move { let _ = tx.send(ServerMessage::TurnComplete); } })
            .on_interrupted(move || { let tx = tx_interrupt.clone(); async move { let _ = tx.send(ServerMessage::Interrupted); } })
            .on_error(move |msg| { let tx = tx_err.clone(); async move { let _ = tx.send(ServerMessage::Error { message: msg }); } })
            .on_disconnected(move |_| { let tx = tx_dc.clone(); async move { let _ = tx.send(ServerMessage::Error { message: "Disconnected".into() }); } })
            .connect(config)
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        let _ = tx.send(ServerMessage::Connected);
        send_app_meta(&tx, self);
        info!("TextChat session connected");

        // Browser -> Gemini: simple recv loop
        while let Some(msg) = rx.recv().await {
            match msg {
                ClientMessage::Text { text } => {
                    if let Err(e) = handle.send_text(&text).await {
                        warn!("Failed to send text: {e}");
                        let _ = tx.send(ServerMessage::Error { message: e.to_string() });
                    }
                }
                ClientMessage::Stop => {
                    info!("TextChat session stopping");
                    let _ = handle.disconnect().await;
                    break;
                }
                _ => {} // Ignore audio messages in text mode
            }
        }

        Ok(())
    }
}
```

**Step 2: Verify it compiles**

Run: `cargo check -p rs-genai-ui`
Expected: Compiles successfully

**Step 3: Run existing tests**

Run: `cargo test -p rs-genai-ui`
Expected: All tests pass (text_chat has no tests but other app tests should not break)

**Step 4: Commit**

```
refactor(cookbooks): migrate text_chat to L2 Live::builder()
```

---

### Task 3: Migrate voice_chat.rs

**Files:**
- Modify: `cookbooks/ui/src/apps/voice_chat.rs`

**Step 1: Rewrite voice_chat.rs**

Same pattern as text_chat but with audio modality. Replace the manual `tokio::select!` event loop with L2 callbacks. Remove the inline voice resolution (use shared `resolve_voice` from mod.rs).

```rust
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use tokio::sync::mpsc;
use tracing::{info, warn};

use adk_rs_fluent::prelude::*;

use crate::app::{AppCategory, AppError, ClientMessage, CookbookApp, ServerMessage, WsSender};

use super::{build_session_config, resolve_voice, send_app_meta, wait_for_start};

/// Native audio voice chat with Gemini Live.
pub struct VoiceChat;

#[async_trait]
impl CookbookApp for VoiceChat {
    fn name(&self) -> &str { "voice-chat" }
    fn description(&self) -> &str { "Native audio voice chat with Gemini Live" }
    fn category(&self) -> AppCategory { AppCategory::Basic }
    fn features(&self) -> Vec<String> { vec!["voice".into(), "transcription".into()] }

    fn tips(&self) -> Vec<String> {
        vec![
            "Click the microphone button to start speaking".into(),
            "Transcriptions appear below each message showing what was said".into(),
            "You can also type text — the model will respond with voice".into(),
        ]
    }

    fn try_saying(&self) -> Vec<String> {
        vec![
            "Hello! Tell me a joke.".into(),
            "What's the weather like on Mars?".into(),
            "Can you sing a short song?".into(),
        ]
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        let start = wait_for_start(&mut rx).await?;
        let selected_voice = resolve_voice(start.voice.as_deref());

        let config = build_session_config(start.model.as_deref())
            .map_err(|e| AppError::Connection(e.to_string()))?
            .response_modalities(vec![Modality::Audio])
            .voice(selected_voice)
            .enable_input_transcription()
            .enable_output_transcription()
            .system_instruction(
                start.system_instruction.as_deref()
                    .unwrap_or("You are a helpful voice assistant. Keep your responses concise and conversational."),
            );

        let b64 = base64::engine::general_purpose::STANDARD;

        // Gemini -> Browser callbacks
        let tx_audio = tx.clone();
        let tx_in = tx.clone();
        let tx_out = tx.clone();
        let tx_text = tx.clone();
        let tx_complete = tx.clone();
        let tx_turn = tx.clone();
        let tx_interrupt = tx.clone();
        let tx_vad_s = tx.clone();
        let tx_vad_e = tx.clone();
        let tx_err = tx.clone();
        let tx_dc = tx.clone();

        let handle = Live::builder()
            .on_audio(move |data| {
                let encoded = b64.encode(data);
                let _ = tx_audio.send(ServerMessage::Audio { data: encoded });
            })
            .on_input_transcript(move |text, _| { let _ = tx_in.send(ServerMessage::InputTranscription { text: text.to_string() }); })
            .on_output_transcript(move |text, _| { let _ = tx_out.send(ServerMessage::OutputTranscription { text: text.to_string() }); })
            .on_text(move |t| { let _ = tx_text.send(ServerMessage::TextDelta { text: t.to_string() }); })
            .on_text_complete(move |t| { let _ = tx_complete.send(ServerMessage::TextComplete { text: t.to_string() }); })
            .on_turn_complete(move || { let tx = tx_turn.clone(); async move { let _ = tx.send(ServerMessage::TurnComplete); } })
            .on_interrupted(move || { let tx = tx_interrupt.clone(); async move { let _ = tx.send(ServerMessage::Interrupted); } })
            .on_vad_start(move || { let _ = tx_vad_s.send(ServerMessage::VoiceActivityStart); })
            .on_vad_end(move || { let _ = tx_vad_e.send(ServerMessage::VoiceActivityEnd); })
            .on_error(move |msg| { let tx = tx_err.clone(); async move { let _ = tx.send(ServerMessage::Error { message: msg }); } })
            .on_disconnected(move |_| { let tx = tx_dc.clone(); async move { let _ = tx.send(ServerMessage::Error { message: "Disconnected".into() }); } })
            .connect(config)
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        let _ = tx.send(ServerMessage::Connected);
        send_app_meta(&tx, self);
        info!("VoiceChat session connected");

        let b64 = base64::engine::general_purpose::STANDARD;

        // Browser -> Gemini
        while let Some(msg) = rx.recv().await {
            match msg {
                ClientMessage::Audio { data } => {
                    match b64.decode(&data) {
                        Ok(pcm_bytes) => {
                            if let Err(e) = handle.send_audio(pcm_bytes).await {
                                warn!("Failed to send audio: {e}");
                                let _ = tx.send(ServerMessage::Error { message: e.to_string() });
                            }
                        }
                        Err(e) => warn!("Failed to decode base64 audio: {e}"),
                    }
                }
                ClientMessage::Text { text } => {
                    if let Err(e) = handle.send_text(&text).await {
                        warn!("Failed to send text: {e}");
                        let _ = tx.send(ServerMessage::Error { message: e.to_string() });
                    }
                }
                ClientMessage::Stop => {
                    info!("VoiceChat session stopping");
                    let _ = handle.disconnect().await;
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }
}
```

**Step 2: Verify**

Run: `cargo check -p rs-genai-ui && cargo test -p rs-genai-ui`
Expected: Compiles and all tests pass

**Step 3: Commit**

```
refactor(cookbooks): migrate voice_chat to L2 Live::builder()
```

---

### Task 4: Migrate tool_calling.rs

**Files:**
- Modify: `cookbooks/ui/src/apps/tool_calling.rs`

**Step 1: Rewrite tool_calling.rs**

Keep `demo_tools()`, `execute_tool()`, `evaluate_simple_expr()` and all tests unchanged. Only replace `handle_session` body. The key difference: use `on_tool_call` callback for manual tool dispatch (since this app uses raw FunctionDeclarations, not a `ToolDispatcher`).

```rust
    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        let start = wait_for_start(&mut rx).await?;

        let config = build_session_config(start.model.as_deref())
            .map_err(|e| AppError::Connection(e.to_string()))?
            .text_only()
            .add_tool(demo_tools())
            .system_instruction(
                start.system_instruction.as_deref().unwrap_or(
                    "You are a helpful assistant with access to tools. \
                     You can check the weather, get the current time, and calculate arithmetic expressions. \
                     Use the available tools when appropriate.",
                ),
            );

        let tx_text = tx.clone();
        let tx_complete = tx.clone();
        let tx_turn = tx.clone();
        let tx_interrupt = tx.clone();
        let tx_err = tx.clone();
        let tx_dc = tx.clone();
        let tx_tool = tx.clone();

        let handle = Live::builder()
            .on_text(move |t| { let _ = tx_text.send(ServerMessage::TextDelta { text: t.to_string() }); })
            .on_text_complete(move |t| { let _ = tx_complete.send(ServerMessage::TextComplete { text: t.to_string() }); })
            .on_turn_complete(move || { let tx = tx_turn.clone(); async move { let _ = tx.send(ServerMessage::TurnComplete); } })
            .on_interrupted(move || { let tx = tx_interrupt.clone(); async move { let _ = tx.send(ServerMessage::Interrupted); } })
            .on_error(move |msg| { let tx = tx_err.clone(); async move { let _ = tx.send(ServerMessage::Error { message: msg }); } })
            .on_disconnected(move |_| { let tx = tx_dc.clone(); async move { let _ = tx.send(ServerMessage::Error { message: "Disconnected".into() }); } })
            .on_tool_call(move |calls| {
                let tx = tx_tool.clone();
                async move {
                    let responses: Vec<FunctionResponse> = calls
                        .iter()
                        .map(|call| {
                            let result = execute_tool(&call.name, &call.args);
                            let _ = tx.send(ServerMessage::StateUpdate {
                                key: format!("tool:{}", call.name),
                                value: json!({
                                    "name": call.name,
                                    "args": call.args,
                                    "result": result,
                                }),
                            });
                            FunctionResponse {
                                name: call.name.clone(),
                                response: result,
                                id: call.id.clone(),
                            }
                        })
                        .collect();
                    Some(responses)
                }
            })
            .connect(config)
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        let _ = tx.send(ServerMessage::Connected);
        send_app_meta(&tx, self);
        info!("ToolCalling session connected");

        while let Some(msg) = rx.recv().await {
            match msg {
                ClientMessage::Text { text } => {
                    if let Err(e) = handle.send_text(&text).await {
                        warn!("Failed to send text: {e}");
                        let _ = tx.send(ServerMessage::Error { message: e.to_string() });
                    }
                }
                ClientMessage::Stop => {
                    info!("ToolCalling session stopping");
                    let _ = handle.disconnect().await;
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }
```

**Step 2: Verify**

Run: `cargo check -p rs-genai-ui && cargo test -p rs-genai-ui`
Expected: Compiles; all 4 `evaluate_simple_expr` tests pass

**Step 3: Commit**

```
refactor(cookbooks): migrate tool_calling to L2 Live::builder()
```

---

### Task 5: Migrate all_config.rs

**Files:**
- Modify: `cookbooks/ui/src/apps/all_config.rs`

**Step 1: Rewrite handle_session**

Keep `AllConfigOptions`, `ToolDef`, `parse_config`, `resolve_voice` (local — this app has its own because it parses JSON config), `config_summary`, `execute_mock_tool`, and all tests unchanged. Only rewrite `handle_session`.

The all_config app is special: it builds `SessionConfig` dynamically based on JSON input. It still uses `Live::connect(config)` but adds appropriate callbacks based on which modalities are active. Tool calls use `on_tool_call` callback for the mock tools.

```rust
    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        let start = wait_for_start(&mut rx).await?;

        let opts = parse_config(
            start.system_instruction.as_deref(),
            start.voice.as_deref(),
        );

        // Build the SessionConfig with all specified options (same as before).
        let mut config = build_session_config(start.model.as_deref())
            .map_err(|e| AppError::Connection(e.to_string()))?;

        let instruction = opts.system_instruction.as_deref()
            .unwrap_or("You are a helpful assistant. This session was started via the all-config playground.");
        config = config.system_instruction(instruction);
        if let Some(temp) = opts.temperature { config = config.temperature(temp); }

        let modality_str = opts.modality.as_deref().unwrap_or("audio");
        let is_audio = modality_str == "audio" || modality_str == "both";

        match modality_str {
            "text" => { config = config.text_only(); }
            "both" => {
                config = config.response_modalities(vec![Modality::Audio, Modality::Text]);
                config = config.voice(resolve_voice(opts.voice.as_deref()));
            }
            _ => {
                config = config.response_modalities(vec![Modality::Audio]);
                config = config.voice(resolve_voice(opts.voice.as_deref()));
            }
        }

        if opts.enable_transcription.unwrap_or(is_audio) {
            config = config.enable_input_transcription().enable_output_transcription();
        }
        if opts.enable_google_search.unwrap_or(false) { config = config.with_google_search(); }
        if opts.enable_code_execution.unwrap_or(false) { config = config.with_code_execution(); }
        if let Some(ref tool_defs) = opts.tools {
            if !tool_defs.is_empty() {
                let declarations: Vec<FunctionDeclaration> = tool_defs.iter().map(|td| FunctionDeclaration {
                    name: td.name.clone(),
                    description: td.description.clone(),
                    parameters: Some(json!({"type":"object","properties":{"input":{"type":"string","description":"Input for the tool"}}})),
                }).collect();
                config = config.add_tool(Tool::functions(declarations));
            }
        }
        if let Some(target_tokens) = opts.context_window_compression {
            config = config.context_window_compression(target_tokens);
        }
        if opts.enable_session_resumption.unwrap_or(false) {
            config = config.session_resumption(None);
        }

        let b64 = base64::engine::general_purpose::STANDARD;

        // Gemini -> Browser callbacks
        let tx_audio = tx.clone();
        let tx_in = tx.clone();
        let tx_out = tx.clone();
        let tx_text = tx.clone();
        let tx_complete = tx.clone();
        let tx_turn = tx.clone();
        let tx_interrupt = tx.clone();
        let tx_vad_s = tx.clone();
        let tx_vad_e = tx.clone();
        let tx_err = tx.clone();
        let tx_dc = tx.clone();
        let tx_tool = tx.clone();

        let handle = Live::builder()
            .on_audio(move |data| {
                let encoded = b64.encode(data);
                let _ = tx_audio.send(ServerMessage::Audio { data: encoded });
            })
            .on_input_transcript(move |text, _| { let _ = tx_in.send(ServerMessage::InputTranscription { text: text.to_string() }); })
            .on_output_transcript(move |text, _| { let _ = tx_out.send(ServerMessage::OutputTranscription { text: text.to_string() }); })
            .on_text(move |t| { let _ = tx_text.send(ServerMessage::TextDelta { text: t.to_string() }); })
            .on_text_complete(move |t| { let _ = tx_complete.send(ServerMessage::TextComplete { text: t.to_string() }); })
            .on_turn_complete(move || { let tx = tx_turn.clone(); async move { let _ = tx.send(ServerMessage::TurnComplete); } })
            .on_interrupted(move || { let tx = tx_interrupt.clone(); async move { let _ = tx.send(ServerMessage::Interrupted); } })
            .on_vad_start(move || { let _ = tx_vad_s.send(ServerMessage::VoiceActivityStart); })
            .on_vad_end(move || { let _ = tx_vad_e.send(ServerMessage::VoiceActivityEnd); })
            .on_error(move |msg| { let tx = tx_err.clone(); async move { let _ = tx.send(ServerMessage::Error { message: msg }); } })
            .on_disconnected(move |_| { let tx = tx_dc.clone(); async move { let _ = tx.send(ServerMessage::Error { message: "Disconnected".into() }); } })
            .on_tool_call(move |calls| {
                let tx = tx_tool.clone();
                async move {
                    let responses: Vec<FunctionResponse> = calls.iter().map(|call| {
                        let result = execute_mock_tool(&call.name, &call.args);
                        let _ = tx.send(ServerMessage::StateUpdate {
                            key: format!("tool:{}", call.name),
                            value: json!({"name": call.name, "args": call.args, "result": result}),
                        });
                        FunctionResponse { name: call.name.clone(), response: result, id: call.id.clone() }
                    }).collect();
                    Some(responses)
                }
            })
            .connect(config)
            .await
            .map_err(|e| AppError::Connection(e.to_string()))?;

        let _ = tx.send(ServerMessage::Connected);
        send_app_meta(&tx, self);
        info!("AllConfig session connected");

        let _ = tx.send(ServerMessage::StateUpdate {
            key: "config".into(),
            value: config_summary(&opts),
        });
        let _ = tx.send(ServerMessage::StateUpdate {
            key: "modality".into(),
            value: json!(modality_str),
        });

        let b64 = base64::engine::general_purpose::STANDARD;

        // Browser -> Gemini
        while let Some(msg) = rx.recv().await {
            match msg {
                ClientMessage::Audio { data } if is_audio => {
                    match b64.decode(&data) {
                        Ok(pcm_bytes) => {
                            if let Err(e) = handle.send_audio(pcm_bytes).await {
                                warn!("Failed to send audio: {e}");
                                let _ = tx.send(ServerMessage::Error { message: e.to_string() });
                            }
                        }
                        Err(e) => warn!("Failed to decode base64 audio: {e}"),
                    }
                }
                ClientMessage::Text { text } => {
                    if let Err(e) = handle.send_text(&text).await {
                        warn!("Failed to send text: {e}");
                        let _ = tx.send(ServerMessage::Error { message: e.to_string() });
                    }
                }
                ClientMessage::Stop => {
                    info!("AllConfig session stopping");
                    let _ = handle.disconnect().await;
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }
```

**Step 2: Verify**

Run: `cargo check -p rs-genai-ui && cargo test -p rs-genai-ui`
Expected: Compiles; all 8 all_config tests pass

**Step 3: Commit**

```
refactor(cookbooks): migrate all_config to L2 Live::builder()
```

---

### Task 6: Create RegexExtractor for custom TurnExtractor

**Files:**
- Create: `cookbooks/ui/src/apps/extractors.rs`
- Modify: `cookbooks/ui/src/apps/mod.rs` (add `pub mod extractors;`)

**Step 1: Create extractors.rs**

This module provides a `RegexExtractor` that wraps a regex-based extraction function into the `TurnExtractor` trait. The extraction function receives the formatted transcript text and returns key-value pairs to merge into State.

```rust
//! Custom TurnExtractor implementations for cookbook apps.
//!
//! Wraps regex-based extraction functions into the TurnExtractor trait
//! so they integrate with the L1 extraction pipeline.

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use rs_adk::live::{TranscriptTurn, TurnExtractor};
use rs_adk::llm::LlmError;

/// A TurnExtractor backed by a synchronous regex/keyword extraction function.
///
/// The extract function receives the formatted transcript text and a snapshot
/// of previously extracted values (to avoid re-extracting known keys).
/// It returns new key-value pairs to merge into State.
pub struct RegexExtractor {
    name: String,
    window_size: usize,
    extract_fn: Arc<dyn Fn(&str, &HashMap<String, Value>) -> HashMap<String, Value> + Send + Sync>,
    /// Accumulated extracted state (carried across turns so extract_fn can
    /// skip already-known keys).
    state: parking_lot::Mutex<HashMap<String, Value>>,
}

impl RegexExtractor {
    /// Create a new regex-based extractor.
    ///
    /// - `name`: key prefix for storing results in State
    /// - `window_size`: how many recent transcript turns to include
    /// - `extract_fn`: `(transcript_text, existing_state) -> new_key_values`
    pub fn new(
        name: impl Into<String>,
        window_size: usize,
        extract_fn: impl Fn(&str, &HashMap<String, Value>) -> HashMap<String, Value> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            window_size,
            extract_fn: Arc::new(extract_fn),
            state: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// Format transcript turns into a single text block for regex matching.
    fn format_window(window: &[TranscriptTurn]) -> String {
        let mut out = String::new();
        for turn in window {
            if !turn.user.is_empty() {
                let _ = write!(out, "[User] {} ", turn.user.trim());
            }
            if !turn.model.is_empty() {
                let _ = write!(out, "[Agent] {} ", turn.model.trim());
            }
        }
        out
    }
}

#[async_trait]
impl TurnExtractor for RegexExtractor {
    fn name(&self) -> &str {
        &self.name
    }

    fn window_size(&self) -> usize {
        self.window_size
    }

    async fn extract(&self, window: &[TranscriptTurn]) -> Result<Value, LlmError> {
        let text = Self::format_window(window);
        let existing = self.state.lock().clone();
        let new_values = (self.extract_fn)(&text, &existing);

        // Merge new values into accumulated state
        {
            let mut state = self.state.lock();
            state.extend(new_values.clone());
        }

        // Return the full accumulated state as JSON
        let full_state = self.state.lock().clone();
        Ok(serde_json::to_value(full_state).unwrap_or(Value::Null))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn make_turns(pairs: &[(&str, &str)]) -> Vec<TranscriptTurn> {
        pairs.iter().enumerate().map(|(i, (user, model))| TranscriptTurn {
            turn_number: i as u32,
            user: user.to_string(),
            model: model.to_string(),
            timestamp: Instant::now(),
        }).collect()
    }

    #[tokio::test]
    async fn regex_extractor_extracts_from_transcript() {
        let extractor = RegexExtractor::new("test", 5, |text, _existing| {
            let mut result = HashMap::new();
            if text.to_lowercase().contains("hello") {
                result.insert("greeted".into(), serde_json::json!(true));
            }
            result
        });

        let turns = make_turns(&[("hello there", "hi!")]);
        let result = extractor.extract(&turns).await.unwrap();
        assert_eq!(result["greeted"], true);
    }

    #[tokio::test]
    async fn regex_extractor_accumulates_state() {
        let extractor = RegexExtractor::new("test", 5, |text, existing| {
            let mut result = HashMap::new();
            let lower = text.to_lowercase();
            if !existing.contains_key("name") && lower.contains("alice") {
                result.insert("name".into(), serde_json::json!("Alice"));
            }
            if !existing.contains_key("issue") && lower.contains("broken") {
                result.insert("issue".into(), serde_json::json!("broken"));
            }
            result
        });

        // Turn 1: extract name
        let turns1 = make_turns(&[("I'm Alice", "Hello Alice!")]);
        let r1 = extractor.extract(&turns1).await.unwrap();
        assert_eq!(r1["name"], "Alice");

        // Turn 2: extract issue (name should still be there)
        let turns2 = make_turns(&[("My product is broken", "I'm sorry to hear that")]);
        let r2 = extractor.extract(&turns2).await.unwrap();
        assert_eq!(r2["name"], "Alice");
        assert_eq!(r2["issue"], "broken");
    }

    #[tokio::test]
    async fn regex_extractor_skips_existing_keys() {
        let call_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let cc = call_count.clone();

        let extractor = RegexExtractor::new("test", 5, move |_text, existing| {
            cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut result = HashMap::new();
            if !existing.contains_key("key") {
                result.insert("key".into(), serde_json::json!("value"));
            }
            result
        });

        let turns = make_turns(&[("hi", "hello")]);
        extractor.extract(&turns).await.unwrap();
        let r = extractor.extract(&turns).await.unwrap();

        // Key should only be extracted once
        assert_eq!(r["key"], "value");
        // But extract_fn was called twice
        assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn format_window_produces_readable_text() {
        let turns = make_turns(&[
            ("Hello there", "Hi! How can I help?"),
            ("My order is broken", "I'm sorry to hear that"),
        ]);
        let text = RegexExtractor::format_window(&turns);
        assert!(text.contains("[User] Hello there"));
        assert!(text.contains("[Agent] Hi! How can I help?"));
        assert!(text.contains("[User] My order is broken"));
    }
}
```

**Step 2: Add module declaration**

In `cookbooks/ui/src/apps/mod.rs`, add after the existing module declarations:

```rust
pub mod extractors;
```

**Step 3: Verify**

Run: `cargo check -p rs-genai-ui && cargo test -p rs-genai-ui`
Expected: Compiles; all 4 new extractor tests + all existing tests pass

**Step 4: Commit**

```
feat(cookbooks): add RegexExtractor for TurnExtractor-based state extraction
```

---

### Task 7: Migrate playbook.rs

**Files:**
- Modify: `cookbooks/ui/src/apps/playbook.rs`

**Step 1: Rewrite playbook.rs**

This is the biggest transformation. Replace:
- Manual `Phase` struct + `PHASES` array -> L2 `.phase()` builder
- Manual `extract_state()` -> `RegexExtractor`
- Manual `evaluate_phase()` -> Phase `on_exit` callback
- Manual `current_phase_idx` + `update_instruction()` -> `PhaseMachine` auto-management
- Manual `ConversationBuffer` -> Built-in `TranscriptBuffer`

Keep: `extract_state()` as a free function (unit tests depend on it), `NAME_PATTERNS`/`ORDER_HASH_RE`/`ORDER_WORD_RE` LazyLock regexes, `evaluate_phase()` as a free function (unit tests depend on it), all existing unit tests.

The key pattern: `extract_state` stays as a free function but is also wrapped in a `RegexExtractor` for the pipeline. Phase instructions become const strings. The `on_enter` callbacks notify the browser of phase transitions. The `instruction_template` dynamically builds context-aware instructions.

Implementation approach:
- Define phase instruction constants at module level
- Use `RegexExtractor::new("playbook_state", 10, |text, existing| extract_state(text, existing))` to wrap existing function
- Use `.phase("greet").instruction(GREET_INSTRUCTION).transition("identify", guard).on_enter(notify).done()` for each phase
- Use `.instruction_template(|state| ...)` for dynamic context-aware instructions
- Use `.on_extracted(|name, value| ...)` to forward StateUpdate to browser

**The implementer should**:
1. Keep the existing `Phase` struct, `PHASES` const, `extract_state()`, `evaluate_phase()`, all regex LazyLocks, and all `#[cfg(test)] mod tests` exactly as they are — these are used by unit tests
2. Rewrite ONLY the `handle_session` method body
3. Remove the `use super::ConversationBuffer;` import (no longer needed)
4. Add necessary imports: `use std::sync::Arc;`, `use adk_rs_fluent::prelude::*;`, `use super::extractors::RegexExtractor;`
5. In `handle_session`, build the L2 session using the pattern from Task 3 (voice callbacks) plus:
   - `.extractor(Arc::new(RegexExtractor::new("playbook_state", 10, |text, existing| extract_state(text, existing))))`
   - Phase definitions using `.phase("greet").instruction(PHASES[0].instruction).transition("identify", |s| { ... }).on_enter(|state, _| { ... }).done()`
   - For each phase transition guard, check the same `required_keys` logic as the current code
   - `.initial_phase("greet")`
   - `.on_extracted(|name, value| { ... })` to send StateUpdate messages to browser
   - `.instruction_template(|state| { ... })` for dynamic instruction with customer context
6. The `on_enter` callback for each phase should send `ServerMessage::PhaseChange` and `ServerMessage::StateUpdate { key: "phase", ... }`
7. The `on_exit` callback can optionally send `ServerMessage::Evaluation` using `evaluate_phase()`

**Step 2: Verify**

Run: `cargo check -p rs-genai-ui && cargo test -p rs-genai-ui`
Expected: Compiles; all 14 existing playbook tests pass (they test free functions, not handle_session)

**Step 3: Commit**

```
refactor(cookbooks): migrate playbook to L2 PhaseMachine + RegexExtractor
```

---

### Task 8: Migrate guardrails.rs

**Files:**
- Modify: `cookbooks/ui/src/apps/guardrails.rs`

**Step 1: Rewrite guardrails.rs**

Replace:
- Manual `ViolationTracker` + cooldown logic -> Watchers on violation state keys
- Manual `check_violations()` + `update_instruction()` -> `RegexExtractor` + `instruction_template`
- Manual `ConversationBuffer` -> Built-in `TranscriptBuffer`

Keep: `PolicyRule` struct, `POLICIES` const, `DetectedViolation` struct, `SSN_RE`/`CC_RE` LazyLocks, `check_violations()`, `ViolationTracker`, `BASE_INSTRUCTION`, and all unit tests.

Implementation approach:
- Create a violations `RegexExtractor` that wraps `check_violations()` and returns violation flags as JSON
- Use watchers on violation state keys (`violation:pii_ssn`, etc.) to send `ServerMessage::Violation` to browser
- Use `instruction_template` for corrective instruction injection
- Standard voice callbacks for audio/transcription/etc.

**The implementer should**:
1. Keep ALL existing structs, consts, functions, and tests unchanged
2. Rewrite ONLY `handle_session` body
3. Remove `use super::ConversationBuffer;` import
4. Add: `use std::sync::Arc;`, `use adk_rs_fluent::prelude::*;`, `use super::extractors::RegexExtractor;`
5. Create a `RegexExtractor` that calls `check_violations()` on the transcript and stores violation flags as booleans in the returned HashMap
6. Use `.watch("violation:pii_ssn").became_true().blocking().then(|_, _, _| async { send Violation })` for each violation type
7. Use `.instruction_template(|state| { build corrective instruction from active violations })` to replace the manual instruction update
8. Standard voice callback pattern from Task 3

**Step 2: Verify**

Run: `cargo check -p rs-genai-ui && cargo test -p rs-genai-ui`
Expected: Compiles; all 11 existing guardrails tests pass

**Step 3: Commit**

```
refactor(cookbooks): migrate guardrails to L2 watchers + RegexExtractor
```

---

### Task 9: Migrate support.rs

**Files:**
- Modify: `cookbooks/ui/src/apps/support.rs`

**Step 1: Rewrite support.rs**

This is the most complex migration — combines phases, computed state, watchers, and multi-agent handoff.

Replace:
- `AgentKind` enum + dual phase arrays -> Single PhaseMachine with `billing:` and `tech:` prefixed phase names
- Manual `extract_state()` -> `RegexExtractor`
- Manual `should_handoff_to_technical()` -> Phase transition guard from `billing:identify` to `tech:greet`
- Manual handoff logic -> Phase `on_enter` callback on `tech:greet`
- Manual `evaluate_phase()` -> Phase `on_exit` callbacks
- `ConversationBuffer` -> Built-in `TranscriptBuffer`

Keep: ALL existing structs, consts, functions, tests.

**The implementer should**:
1. Keep ALL existing code (AgentKind, AgentPhase, both PHASES arrays, extract_state, evaluate_phase, should_handoff_to_technical, build_instruction) and all tests
2. Rewrite ONLY `handle_session` body
3. Remove `use super::ConversationBuffer;`
4. Add: `use std::sync::Arc;`, `use adk_rs_fluent::prelude::*;`, `use super::extractors::RegexExtractor;`
5. Use `RegexExtractor::new("support_state", 10, |text, existing| extract_state(text, existing))`
6. Use `.computed("active_agent", &["issue_type"], |state| { ... })` to derive the active agent
7. Define phases with `billing:` and `tech:` prefixes:
   - `billing:greet` -> transitions to `billing:identify` when `customer_name` exists
   - `billing:identify` -> transitions to `tech:greet` when `issue_type == "technical"`, OR to `billing:investigate` when any `issue_type` exists
   - `billing:investigate` -> `billing:resolve` when `billing_detail` exists
   - `billing:resolve` -> `billing:close` when `resolution_confirmed` exists
   - `billing:close` -> terminal
   - `tech:greet` -> `tech:identify` when `tech_issue_desc` exists; `on_enter` sends handoff notifications
   - `tech:identify` -> `tech:troubleshoot` when `tech_category` exists
   - `tech:troubleshoot` -> `tech:escalate-or-resolve` when `troubleshoot_result` exists
   - `tech:escalate-or-resolve` -> `tech:close` when `final_outcome` exists
   - `tech:close` -> terminal
8. `.initial_phase("billing:greet")`
9. Use `.watch("final_outcome").changed_to(json!("escalated")).then(...)` for escalation notification
10. `.on_extracted(...)` to forward state updates to browser
11. `.instruction_template(...)` for context-aware instructions

**Step 2: Verify**

Run: `cargo check -p rs-genai-ui && cargo test -p rs-genai-ui`
Expected: Compiles; all 20 existing support tests pass

**Step 3: Commit**

```
refactor(cookbooks): migrate support to L2 PhaseMachine + computed + watchers
```

---

### Task 10: Cleanup and final verification

**Files:**
- Modify: `cookbooks/ui/src/apps/mod.rs` (potentially remove ConversationBuffer if unused)

**Step 1: Check if ConversationBuffer is still used**

Search for `ConversationBuffer` in `cookbooks/ui/src/apps/`. If no app imports it anymore, remove the struct from `mod.rs`. Note: the playbook test `conversation_buffer_limits_turns` and guardrails test `conversation_buffer_limits` test this struct directly, so check if those tests still exist.

If the existing tests still reference `ConversationBuffer`, keep it. If all advanced app test suites were preserved (which they should be — we didn't modify tests), `ConversationBuffer` tests may still reference it. In that case, keep the struct.

**Step 2: Remove unused imports**

Check each migrated file for unused imports (e.g., `regex::Regex` imports that are no longer needed in the main code but still needed for tests).

**Step 3: Full workspace build and test**

Run: `cargo build -p rs-genai-ui && cargo test -p rs-genai-ui`
Expected: Clean build, all tests pass

Run: `cargo test --workspace`
Expected: All 1,100+ workspace tests pass

**Step 4: Commit**

```
chore(cookbooks): cleanup unused imports after L2 migration
```
