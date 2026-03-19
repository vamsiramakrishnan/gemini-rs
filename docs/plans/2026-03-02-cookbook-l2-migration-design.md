# Web UI Migration to L2 Fluent API

**Date**: 2026-03-02
**Status**: Design
**Scope**: Migrate all 7 demo/ui apps from raw L0 `ConnectBuilder` to L2 `Live::builder()` fluent API

## Context

The apps/gemini-adk-web-rs currently implements 7 apps using low-level L0 `ConnectBuilder::new(config).build()`. Each app has a hand-coded `tokio::select!` event loop, and the three advanced apps (playbook, guardrails, support) manually implement state machines, regex extraction, phase tracking, violation detection, and instruction updates.

We now have L1 modules (PhaseMachine, WatcherRegistry, ComputedRegistry, TemporalRegistry, SessionSignals, BackgroundToolTracker) and the L2 `Live::builder()` fluent API that automates all of this.

## Architecture Decision

**Use L2 `gemini-adk-fluent-rs::Live::builder()`** for all demo apps.

Rationale: Examples exist to showcase best DX. The L2 API is the intended public API. Using it in demos validates the API design and serves as documentation.

### Connection Pattern

Keep the existing `build_session_config()` helper for credential handling (Vertex AI vs Google AI detection, gcloud token retrieval). Use `Live::connect(config)` which accepts a pre-built `SessionConfig`:

```rust
let config = build_session_config(start.model.as_deref())?
    .response_modalities(vec![Modality::Audio])
    .voice(selected_voice)
    .enable_input_transcription()
    .enable_output_transcription()
    .system_instruction(INITIAL_INSTRUCTION);

let handle = Live::builder()
    .phase("greet").instruction(INITIAL_INSTRUCTION).transition(...).done()
    .on_audio(move |data| { tx.send(ServerMessage::Audio { ... }); })
    .connect(config)
    .await
    .map_err(|e| AppError::Connection(e.to_string()))?;
```

Note: `Live::connect(config)` replaces `self.config`, so model/voice/instruction must be set on the `SessionConfig`, not via `.model()`/`.voice()` builder methods. This is fine since `build_session_config()` already handles all of that.

### CookbookApp Trait

**No changes.** The trait stays as-is:

```rust
async fn handle_session(&self, tx: WsSender, rx: mpsc::UnboundedReceiver<ClientMessage>) -> Result<(), AppError>;
```

The callbacks capture `tx` clones for Gemini-to-browser messages. A simple loop handles browser-to-Gemini messages via `LiveHandle`.

### State Extraction Strategy

**Keep regex-based extraction** wrapped in a custom `TurnExtractor`. Reasons:
- Free (no extra API calls)
- Works offline
- Zero latency
- Existing unit tests continue to pass
- Can be swapped for LLM-based `extract_turns::<T>()` later

The `RegexExtractor` implements `TurnExtractor` by running the existing regex functions on the formatted transcript window, storing results in `State`.

### Browser Notifications

How each notification type maps to L2:

| Message Type | Current | With L2 |
|---|---|---|
| Audio, TextDelta, TextComplete | Manual in select! | `on_audio`, `on_text`, `on_text_complete` callbacks |
| InputTranscription, OutputTranscription | Manual in select! | `on_input_transcript`, `on_output_transcript` callbacks |
| TurnComplete, Interrupted | Manual in select! | `on_turn_complete`, `on_interrupted` callbacks |
| VoiceActivityStart/End | Manual in select! | `on_vad_start`, `on_vad_end` callbacks |
| Error, Disconnected | Manual in select! | `on_error`, `on_disconnected` callbacks |
| PhaseChange | Manual phase_idx tracking | Phase `on_enter` callback |
| StateUpdate | Manual state HashMap | `on_extracted` callback + phase `on_enter` |
| Evaluation | Manual evaluate_phase() | Phase `on_exit` callback |
| Violation | Manual check_violations() | Watchers on violation state keys |

## Per-App Migration Plan

### 1. voice_chat.rs (Basic, ~177 lines -> ~60 lines)

Replace manual `tokio::select!` event loop with callbacks. The entire Gemini->browser direction becomes callbacks; browser->Gemini is a simple recv loop.

```rust
// Gemini -> Browser: callbacks
let handle = Live::builder()
    .on_audio(move |data| { tx.send(Audio { data: b64.encode(data) }); })
    .on_input_transcript(move |text, _| { tx.send(InputTranscription { text }); })
    .on_output_transcript(move |text, _| { tx.send(OutputTranscription { text }); })
    .on_text(move |text| { tx.send(TextDelta { text }); })
    .on_text_complete(move |text| { tx.send(TextComplete { text }); })
    .on_turn_complete(move || async move { tx.send(TurnComplete); })
    .on_interrupted(move || async move { tx.send(Interrupted); })
    .on_vad_start(move || { tx.send(VoiceActivityStart); })
    .on_vad_end(move || { tx.send(VoiceActivityEnd); })
    .on_error(move |msg| async move { tx.send(Error { message: msg }); })
    .on_disconnected(move |_| async move { /* break loop */ })
    .connect(config).await?;

// Browser -> Gemini: simple loop
while let Some(msg) = rx.recv().await {
    match msg {
        ClientMessage::Audio { data } => handle.send_audio(b64.decode(&data)?).await?,
        ClientMessage::Text { text } => handle.send_text(&text).await?,
        ClientMessage::Stop | None => { handle.disconnect().await; break; }
        _ => {}
    }
}
```

### 2. text_chat.rs (Basic, ~165 lines -> ~55 lines)

Same pattern as voice_chat but with TEXT modality instead of AUDIO.

### 3. tool_calling.rs (Basic, ~200 lines -> ~65 lines)

Same callback pattern. The tool dispatcher is passed via `Live::builder().tools(dispatcher)` which auto-dispatches tool calls. Tool results show up automatically.

### 4. all_config.rs (Basic)

Same callback pattern. Showcases all config options.

### 5. playbook.rs (Advanced, ~630 lines -> ~180 lines)

**Eliminates**: Manual Phase struct, PHASES array, extract_state(), evaluate_phase(), current_phase_idx tracking, manual update_instruction() calls, ConversationBuffer.

**Uses**: PhaseMachine (via `.phase()` builder), RegexExtractor (via `.extractor()`), phase `on_enter`/`on_exit` for browser notifications, `instruction_template` for dynamic instruction context.

Key structure:
```rust
Live::builder()
    .extractor(Arc::new(RegexExtractor::new("playbook_state", extract_state)))
    .on_extracted(move |name, value| async move {
        // Send StateUpdate for each extracted key
    })
    .phase("greet")
        .instruction(GREET_INSTRUCTION)
        .on_enter(move |state, _| async move {
            tx.send(PhaseChange { from: "none", to: "greet", ... });
        })
        .on_exit(move |state, _| async move {
            // Send Evaluation for outgoing phase
        })
        .transition("identify", |s| s.get::<String>("customer_name").is_some())
        .done()
    // ... remaining phases ...
    .initial_phase("greet")
    .instruction_template(|state| {
        let phase: String = state.get("session:phase").unwrap_or_default();
        let name: String = state.get("customer_name").unwrap_or_default();
        let state_json = /* serialize relevant state */;
        Some(format!("{phase_instruction}\n\nCustomer: {name}. State: {state_json}"))
    })
    .connect(config).await?;
```

### 6. guardrails.rs (Advanced, ~560 lines -> ~140 lines)

**Eliminates**: PolicyRule struct, POLICIES array, DetectedViolation struct, SSN_RE/CC_RE regexes, check_violations(), ViolationTracker struct (with cooldown logic), manual instruction reconstruction.

**Uses**: RegexExtractor (wrapping check_violations), watchers for violation state keys, `instruction_template` for corrective injection, `when_rate` for rapid-violation escalation.

Key structure:
```rust
Live::builder()
    .instruction(BASE_INSTRUCTION)
    .extractor(Arc::new(ViolationExtractor::new()))
    .watch("violation:pii_ssn")
        .became_true()
        .blocking()
        .then(move |_, _, state| async move {
            tx.send(Violation { rule: "pii_ssn", severity: "critical", ... });
        })
    .watch("violation:pii_credit_card")
        .became_true()
        .blocking()
        .then(move |_, _, _| async move { /* send Violation */ })
    .watch("violation:off_topic")
        .became_true()
        .then(move |_, _, _| async move { /* send Violation */ })
    .watch("violation:negative_sentiment")
        .became_true()
        .then(move |_, _, _| async move { /* send Violation */ })
    .instruction_template(|state| {
        let mut corrections = vec![BASE_INSTRUCTION.to_string()];
        if state.get::<bool>("violation:pii_ssn").unwrap_or(false) {
            corrections.push(PII_SSN_CORRECTION.into());
        }
        // ... other corrections ...
        Some(corrections.join("\n\n"))
    })
    .when_rate("rapid_violations",
        |evt| matches!(evt, SessionEvent::TurnComplete),
        3, Duration::from_secs(60),
        |state, writer| async move { /* escalate */ },
    )
    .connect(config).await?;
```

### 7. support.rs (Showcase, ~857 lines -> ~220 lines)

**Eliminates**: AgentKind enum, AgentPhase struct, BILLING_PHASES/TECHNICAL_PHASES arrays, duplicate extract_state(), evaluate_phase(), should_handoff_to_technical(), build_instruction(), manual handoff logic.

**Uses**: PhaseMachine with `billing:` and `tech:` prefixed phases, `computed` for derived `active_agent`, phase `on_enter` for handoff notifications, watchers for escalation.

Key structure:
```rust
Live::builder()
    .extractor(Arc::new(RegexExtractor::new("support_state", extract_support_state)))
    .computed("active_agent", &["issue_type"], |state| {
        let issue: String = state.get("issue_type")?;
        match issue.as_str() {
            "technical" => Some(json!("technical-support")),
            _ => Some(json!("billing-support")),
        }
    })
    // Billing phases
    .phase("billing:greet")
        .instruction(BILLING_GREET)
        .on_enter(move |state, _| async move {
            tx.send(PhaseChange { from: "none", to: "billing:greet", ... });
            tx.send(StateUpdate { key: "active_agent", value: json!("billing-support") });
        })
        .transition("billing:identify", |s| s.get::<String>("customer_name").is_some())
        .done()
    .phase("billing:identify")
        .instruction(BILLING_IDENTIFY)
        .transition("tech:greet", |s| {
            s.get::<String>("issue_type").map_or(false, |t| t == "technical")
        })
        .transition("billing:investigate", |s| s.get::<String>("issue_type").is_some())
        .done()
    // ... remaining billing phases ...
    // Technical phases (entered via handoff transition)
    .phase("tech:greet")
        .instruction(TECH_GREET)
        .on_enter(move |state, _| async move {
            tx.send(PhaseChange { from: "billing:identify", to: "tech:greet", reason: "Technical issue detected" });
            tx.send(StateUpdate { key: "active_agent", value: json!("technical-support") });
        })
        .transition("tech:identify", |s| s.get::<String>("tech_issue_desc").is_some())
        .done()
    // ... remaining tech phases ...
    .initial_phase("billing:greet")
    .watch("final_outcome")
        .changed_to(json!("escalated"))
        .then(move |_, _, state| async move {
            let priority = if state.get::<String>("sentiment").map_or(false, |s| s == "negative") {
                "high"
            } else {
                "normal"
            };
            tx.send(StateUpdate { key: "escalation", value: json!({"priority": priority}) });
        })
    .connect(config).await?;
```

## New Files

### `apps/gemini-adk-web-rs/src/apps/extractors.rs`

Custom `TurnExtractor` implementations wrapping the existing regex extraction functions:

```rust
pub struct RegexExtractor {
    name: String,
    extract_fn: Box<dyn Fn(&str) -> HashMap<String, Value> + Send + Sync>,
}

impl TurnExtractor for RegexExtractor {
    fn name(&self) -> &str { &self.name }
    fn window_size(&self) -> usize { 5 }  // last 5 turns
    async fn extract(&self, window: &[TranscriptTurn]) -> Result<Value, LlmError> {
        let text = format_window(window);
        let extracted = (self.extract_fn)(&text);
        Ok(json!(extracted))
    }
}
```

The existing `extract_state()` functions from playbook.rs, guardrails.rs, and support.rs move here (or stay in their respective files as free functions).

## Dependency Changes

### `apps/gemini-adk-web-rs/Cargo.toml`

Add:
```toml
gemini-adk-fluent-rs = { path = "../../crates/gemini-adk-fluent-rs" }
```

The `regex` dependency stays (used by RegexExtractor). The `gemini-adk-rs` dependency stays (for types like State, TurnExtractor).

## What Stays Unchanged

- `CookbookApp` trait and `AppRegistry`
- `ServerMessage` / `ClientMessage` enums
- `build_session_config()` helper (credential handling)
- `wait_for_start()` helper
- `send_app_meta()` helper
- Frontend HTML/JS/CSS
- WebSocket handler (`main.rs` / `app.rs`)

## What Gets Removed

- `ConversationBuffer` struct (replaced by built-in TranscriptBuffer)
- Manual `tokio::select!` event loops in all apps (replaced by callbacks + simple recv loop)
- Manual phase tracking variables (`current_phase_idx`, `phase_turn_count`, etc.)
- Manual `update_instruction()` calls (handled by PhaseMachine + instruction_template)
- Manual ViolationTracker (replaced by watchers + temporal patterns)
- ~2,400 lines of hand-coded infrastructure across the 7 apps

## Testing

- Existing regex extraction unit tests stay (they test pure functions)
- Existing evaluate_phase tests stay
- Existing violation detection tests stay
- No new integration tests needed (the L1/L2 modules have their own 1,100+ tests)

## Implementation Order

1. Add `gemini-adk-fluent-rs` dependency + create `extractors.rs`
2. Migrate basic apps (voice_chat, text_chat, tool_calling) — validates the callback pattern
3. Migrate all_config
4. Migrate playbook — validates PhaseMachine integration
5. Migrate guardrails — validates WatcherRegistry + TemporalRegistry
6. Migrate support — validates full pipeline (phases + computed + watchers)
7. Remove ConversationBuffer if unused
8. Verify all tests pass
