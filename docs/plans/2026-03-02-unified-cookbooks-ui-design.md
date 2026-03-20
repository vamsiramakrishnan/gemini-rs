# Unified Web UI — Design Document

**Date:** 2026-03-02
**Status:** Approved

## Goal

Single-binary, router-based demo hub that consolidates existing demos and adds new higher-order apps showcasing out-of-band control driven by Live API events in real-world speech-to-speech scenarios.

## Architecture

### Single Axum Binary (`apps/gemini-adk-web-rs/`)

```
GET /                → Landing page with app cards
GET /app/<name>      → Full-screen app (conversation + devtools)
WS  /ws/<name>       → WebSocket per app
GET /static/*        → Shared CSS, JS, assets
```

Each app is a Rust module implementing:

```rust
#[async_trait]
trait CookbookApp: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn category(&self) -> AppCategory; // Basic | Advanced | Showcase
    fn features(&self) -> &[&str];     // ["voice", "tools", "state-machine", ...]
    async fn handle_session(&self, config: AppConfig, ws_tx: WsSender) -> Result<(), AppError>;
}
```

Apps registered in a central `AppRegistry` at startup, routes generated automatically.

### App Lineup (7 apps)

| Category | App | Key Pattern |
|----------|-----|-------------|
| Basic | Text Chat | Minimal text-only session |
| Basic | Voice Chat | Native audio + transcription |
| Basic | Tool Calling | TypedTool dispatch |
| Advanced | Playbook Agent | State machine + text agent evaluation |
| Advanced | Guardrails Agent | Policy monitoring + corrective injection |
| Showcase | Support Assistant | Multi-agent handoff + dynamic instructions |
| Showcase | All Config | Every Gemini Live configuration option |

## UI Screens

### Landing Page (`/`)

Grid of app cards grouped by category (Basic / Advanced / Showcase). Each card shows name, one-line description, and feature badges (voice, tools, state-machine, guardrails).

### App Screen (`/app/<name>`)

Two-pane layout:

**Left — Conversation pane (polished UX):**
- Chat message bubbles (user right-aligned blue, model left-aligned gray)
- Mic button with recording indicator (pulsing red)
- Text input with send button
- Transcription display (italic, semi-transparent)
- Speaking indicator ("Listening...")
- Connection status bar

**Right — Devtools panel (collapsible):**

Four tabs, shown/hidden per app category:

1. **State** — live key-value view of the State container, highlights changes
2. **Events** — scrolling log of SessionEvents with type badges + timestamps
3. **Playbook** — state machine visualization with current phase highlighted, phase checklist
4. **Evaluator** — text agent adherence output (score, violations, suggestions)

Basic apps show only State + Events. Advanced adds Playbook. Showcase adds all four.

### WebSocket Protocol

Base protocol (same as current):

```
Client → Server: start, text, audio, stop
Server → Client: connected, textDelta, textComplete, audio,
                  turnComplete, interrupted, inputTranscription,
                  outputTranscription, voiceActivityStart/End, error
```

Extended for devtools:

```
Server → Client:
  { type: "stateUpdate", key, value }
  { type: "phaseChange", from, to, reason }
  { type: "evaluation", phase, score, notes }
  { type: "violation", rule, severity, detail }
  { type: "appMeta", name, description, category, features }
```

## Out-of-Band Control Architecture

Core pattern for all Advanced/Showcase apps:

```
Browser ←WS→ Axum Handler
                 │
                 ├── Live::builder()         ← Gemini Live voice session
                 │     .on_text/audio(...)
                 │     .extract_turns(...)    ← structured state extraction
                 │
                 ├── Processor                ← reacts to extracted state
                 │     .check_transitions()   ← state machine / policy logic
                 │     .update_instruction()  ← rewrites prompt mid-session
                 │     → sends devtools msgs to browser
                 │
                 └── TextAgent evaluator      ← runs on phase transitions
                       .run(state)            ← adherence check
                       → sends evaluation to browser
```

### Playbook Agent (package-late example)

State machine phases:

```
Greet → Identify → Investigate → Explain → Resolve → Close
```

Each phase defines:
- **Entry conditions:** required state keys (e.g., `customer_name` must exist to leave Greet)
- **System instruction override:** phase-specific prompt injected via `live_handle.update_instruction()`
- **Extraction targets:** structured data to extract (order number, sentiment, resolution type)

`PlaybookProcessor` subscribes to extracted state from `Live::builder().extract_turns()`, checks the state machine, triggers phase transitions, and rewrites the system instruction for the new phase.

`TextAgent` evaluator runs on each phase transition. Takes conversation state + playbook definition, returns adherence score + notes. Uses `FnTextAgent` or `LlmTextAgent` from gemini-adk-rs.

### Guardrails Agent

Same architecture. Processor monitors for policy violations (PII, off-topic, sentiment drift) instead of phase transitions. Injects corrective instructions when violations detected.

### Support Assistant

Chains two processors:
1. Playbook processor for state machine (billing flow → technical flow)
2. Handoff processor that triggers agent transfer when state machine reaches escalation phase, with context preservation across the handoff.

## File Structure

```
apps/gemini-adk-web-rs/
  Cargo.toml
  src/
    main.rs              ← Axum server, route setup, app registry
    app.rs               ← CookbookApp trait, AppRegistry, AppConfig
    ws_handler.rs        ← Shared WebSocket handler logic
    apps/
      mod.rs
      text_chat.rs       ← Basic: text-only
      voice_chat.rs      ← Basic: native audio
      tool_calling.rs    ← Basic: TypedTool dispatch
      playbook.rs        ← Advanced: state machine + evaluator
      guardrails.rs      ← Advanced: policy monitoring
      support.rs         ← Showcase: multi-agent handoff
      all_config.rs      ← Showcase: every config option
  static/
    index.html           ← Landing page
    app.html             ← App screen template
    css/
      main.css           ← Shared styles
      landing.css        ← Landing page specific
      devtools.css       ← Devtools panel
    js/
      app.js             ← Conversation UX logic
      devtools.js        ← Devtools panel logic
      audio.js           ← Mic recording + playback
      state-machine.js   ← Phase diagram renderer
```

## Dependencies

Uses existing crates only:
- `gemini-genai-rs` — wire-level Gemini Live connection
- `gemini-adk-rs` — TextAgent, State, ToolDispatcher, Plugin system
- `gemini-adk-fluent-rs` — Live::builder(), AgentBuilder, composition
- `axum`, `tokio`, `serde_json` — server infrastructure

## Non-Goals

- No database persistence (all in-memory for demos)
- No authentication (local dev tool)
- No deployment packaging (just `cargo run`)
