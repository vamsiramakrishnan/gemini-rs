# Console UI Redesign: OTel-Native Trace Explorer

**Date**: 2026-03-07
**Status**: Approved
**Scope**: `apps/adk-web/` — frontend (HTML/CSS/JS) + server-side message types + session bridge

## Problem

The current Web UI devtools panel has three structural problems:

1. **Events tab is a firehose.** Hundreds of events scroll past with no way to find what matters. No search, no grouping, unbounded DOM nodes cause memory growth and scroll jank.

2. **No unified timeline.** State changes, phase transitions, tool calls, and telemetry are scattered across 5 tabs. Correlating "what happened at T+3.2s" requires clicking between tabs and eyeballing timestamps.

3. **Telemetry is shallow.** The runtime already tracks token counts (total/prompt/response/cached/thoughts), per-turn latencies, and session resume info via `SessionTelemetry` atomic counters. The browser ignores most of this data. Usage metadata and cost estimation are absent.

Additionally, the rendering architecture uses `innerHTML` replacement for most panels, causing full DOM rebuilds on every update. At scale (50+ state keys, hundreds of events), this creates visible jank that competes with audio playback for main-thread time.

## Design Principles

- **Performance is non-negotiable.** Audio playback and recording must never stutter. DOM work is budgeted per frame.
- **OTel-native.** The runtime already emits tracing spans and Prometheus metrics. The browser should consume this data, not maintain a parallel event model.
- **Dense enough for debugging, polished enough for demos.** The UI serves both SDK developers and demo audiences.
- **Vanilla JS, no frameworks, no bundlers.** The current stack (< 50KB total JS) is correct. Keep it.

## Architecture

### 1. Rendering Engine

All panels share three primitives:

**RingBuffer(capacity)** — Bounded event storage. O(1) append, oldest events dropped silently when full. Default capacity: 10,000 events. This is a developer console, not a logging backend.

```
Event stream (unbounded)
    |
    v
RingBuffer(10000)           <-- bounded memory, ~2MB ceiling
    |
    v
FilterPipeline              <-- type filters, search, phase scope
    |
    v
VirtualList                 <-- renders only visible rows (~40-60)
    |
    v
DOM Pool (recycled nodes)   <-- zero GC pressure, no createElement
```

**VirtualList** — Measures container height, computes visible index range, positions a pool of pre-created DOM nodes via `transform: translateY()`. Scroll events trigger repositioning, not DOM creation/destruction. ~120 lines of vanilla JS. Used by: Timeline, State table, Tool calls list, Phase entries.

**RenderScheduler** — Single `requestAnimationFrame` loop. Panels set dirty flags; the scheduler renders all dirty panels in one rAF callback per frame. No panel ever triggers its own rAF. Coalesces rapid updates (telemetry arrives every 100ms, state updates can burst).

**CSS containment** — Every panel gets `contain: strict` to prevent cross-panel layout thrashing. The minimap canvas is `contain: size layout paint`.

### 2. Unified Timeline (replaces Events tab)

The Timeline is the primary devtools view. Everything that happens in a session — span lifecycle, state mutations, phase transitions, tool calls, text, audio, interruptions — appears in one chronologically ordered, scrollable list.

**Row format** (monospaced, single-line, expandable on click):
```
[T+2.3s] [PHASE]  greeting -> main        guard: greeted=true    142ms
[T+2.4s] [STATE]  app:score = 0.85
[T+2.5s] [TOOL]   get_weather({city:"NYC"})                      180ms
[T+3.1s] [TEXT]   "The weather in New York is..."
[T+3.1s] [AUDIO]  2.4KB out
[T+4.0s] [TURN]   complete    347 prompt / 89 response tokens
[T+4.1s] [SPAN]   rs_genai.tool_call exited                      180ms
```

**Color coding** by event type (reuses existing badge palette). Clicking a row expands to show the full JSON payload or span attributes.

**Filters**: Toolbar at the top with toggle buttons per event type. Persistent across sessions via `localStorage`. Default hidden: `audio`, `voiceActivityStart/End`.

**Search**: Text input filters rows by substring match on rendered content. Debounced 150ms.

**Span nesting**: Span events (`SpanBegin`/`SpanEnd`) can optionally render with indentation showing parent-child relationships (e.g., `session > tool_call > http_request`). Toggle between flat and nested views.

### 3. Canvas Minimap

A 24px-tall horizontal bar rendered on a `<canvas>` element above the timeline.

- Time flows left to right, scaled to session duration.
- Each event is a 1-2px colored tick on its color lane (audio=purple, state=amber, phase=orange, tool=blue, text=blue, etc.).
- The visible viewport is a translucent overlay rectangle.
- Click anywhere on the minimap to scroll the timeline to that point.
- During live sessions, auto-advances to keep the latest events visible.
- Repaint budget: < 0.5ms per frame. Batch-painted in the `RenderScheduler` rAF loop.
- Uses `OffscreenCanvas` when available for compositing off main thread.

### 4. Metrics Tab (replaces NFR)

Layout: three-column hero strip + sections below.

```
+------------------------------------------+
|  LATENCY        TOKENS         SESSION    |
|  avg 245ms      1,847 total    2m 34s     |
|  last 180ms     1,204 prompt   14 turns   |
|  min/max bar    643 response   3 interr.  |
|                 est. ~$0.003   phase: main|
+------------------------------------------+
|  PER-TURN LATENCY [sparkline canvas]      |
+------------------------------------------+
|  AUDIO                                    |
|  48KB out  |  12KB/s  |  buffer health    |
+------------------------------------------+
|  RECENT TOOL CALLS (last 5, expandable)   |
|  get_weather(NYC) -> {temp:22}  [180ms]   |
+------------------------------------------+
```

**New data surfaced** (already available from `SessionTelemetry::snapshot()`):
- Token counts: `total_token_count`, `prompt_token_count`, `response_token_count`, `cached_content_token_count`, `thoughts_token_count`
- Cost estimation: multiply token counts by model pricing (configurable, defaults to Flash pricing)
- Per-turn latency sparkline: tiny `<canvas>` bar chart (80x24px), one bar per turn

**The latency hero metric stays** — well designed, just gains peer columns for tokens and session stats.

### 5. State Panel Upgrade

**Targeted cell updates**: Maintain a `Map<key, {keyEl, valueEl}>`. On `stateUpdate`, update only the changed cell's `textContent`. No `innerHTML` rebuild.

**Change diffing**: Flash animation on the changed row. Show previous value in muted text for 2 seconds: `0.85 (was: 0.7)`.

**Collapsed groups**: Prefix groups (`session:`, `derived:`, `turn:`, `app:`, `bg:`) start collapsed. Click header to expand. Reduces initial visible rows.

**Search filter**: Small input at the top filters keys by substring. Debounced.

### 6. Phases Panel Upgrade

**Current phase hero**: Prominent card at the top showing the active phase name, its instruction snippet (first 120 chars), and a list of tools available in this phase.

**Duration bars**: Each phase transition entry gets a proportional-width bar showing how long that phase lasted relative to total session time.

**Needs fulfillment**: If phases declare `.needs()`, show a checklist of fulfilled (checkmark) vs pending (empty) requirements with the key name and current value.

### 7. OTel Integration: WebSocketSpanLayer

A custom `tracing::Layer` implementation that bridges backend spans to the browser.

**Filtering**: Only captures spans with targets matching `rs_genai` or `gemini.agent` (the ~13 span types defined in `spans.rs` files). All other tracing output is ignored.

**Serialization**: On span close, emits:
```rust
ServerMessage::SpanEvent {
    name: String,           // "rs_genai.tool_call"
    span_id: String,        // hex-encoded span ID
    parent_id: Option<String>,
    duration_us: u64,
    attributes: Value,      // {"function_name": "get_weather", "session_id": "..."}
    status: String,         // "ok" | "error"
}
```

**Performance**: The layer is only active when a browser session is connected. It uses a bounded channel (capacity 256) to avoid backpressure on the tracing pipeline. If the channel is full, span events are dropped (the primary OTLP export path is unaffected).

**Lifecycle**: Created per-session, dropped when the WebSocket disconnects. No global state.

### 8. OTel Export

**"Copy Trace as OTLP JSON" button** in the metrics tab. Serializes the captured span tree (from the RingBuffer) into OTLP-compatible JSON format. User can:
- Copy to clipboard and `curl` it to any OTLP collector
- Save as a `.json` file and import into Jaeger/Grafana Tempo

**Trace ID display**: If the session has a trace ID (from the `rs_genai.session` span), show it in the status bar as a clickable badge. If `OTEL_EXPORTER_OTLP_ENDPOINT` is configured, the badge links to `{jaeger_url}/trace/{trace_id}` (configurable base URL via env var `TRACE_VIEWER_URL`).

### 9. Server-Side: SessionBridge

Every demo app currently has ~15 `tx.clone()` lines to wire up callbacks. Replace with a shared helper:

```rust
pub struct SessionBridge {
    tx: WsSender,
    // Optional: WebSocketSpanLayer sender for this session
}

impl SessionBridge {
    pub fn new(tx: WsSender) -> Self { ... }

    /// Wire all standard callbacks onto a Live::builder().
    /// Returns the builder with on_audio, on_text, on_turn_complete,
    /// on_interrupted, on_vad_*, on_error, on_input/output_transcript,
    /// and telemetry forwarding already attached.
    pub fn wire_live(&self, builder: LiveBuilder) -> LiveBuilder { ... }

    /// Send appMeta message.
    pub fn send_meta(&self, app: &dyn CookbookApp) { ... }
}
```

This reduces each demo app's `handle_session` to ~20 lines: create bridge, build config, wire callbacks, enter recv loop.

### 10. New ServerMessage Variants

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerMessage {
    // ... existing variants unchanged ...

    // New: OTel span events
    SpanEvent {
        name: String,
        span_id: String,
        parent_id: Option<String>,
        duration_us: u64,
        attributes: serde_json::Value,
        status: String,
    },

    // New: per-turn metrics for sparkline
    TurnMetrics {
        turn: u32,
        latency_ms: u32,
        prompt_tokens: u32,
        response_tokens: u32,
    },
}
```

The existing `Telemetry { stats }` variant already carries token counts from `SessionTelemetry::snapshot()`. No changes needed — the browser just needs to render the fields it currently ignores.

### 11. Tab Restructure

**Before**: State | Events | Playbook | Evaluator | NFR
**After**: Timeline | State | Phases | Metrics

- **Timeline** (new, default): unified event/span stream with filters, search, minimap
- **State**: upgraded with targeted updates, diff flash, search, collapsed groups
- **Phases**: upgraded playbook with duration bars, needs checklist, current phase hero
- **Metrics**: upgraded NFR with tokens, cost, sparkline, OTel export
- **Evaluator**: merged into Timeline as filterable event type (violations show as colored rows)

### 12. File Structure Changes

```
static/
  js/
    app.js              # Conversation UX (minor changes)
    devtools.js          # Complete rewrite — new panel architecture
    audio.js            # Unchanged
    render/
      ring-buffer.js    # RingBuffer(capacity) — bounded event storage
      virtual-list.js   # VirtualList — DOM recycling + scroll virtualization
      render-scheduler.js  # Single rAF loop, dirty flags
      minimap.js        # Canvas minimap renderer
      sparkline.js      # Tiny canvas sparkline for per-turn latency
  css/
    main.css            # Minor additions
    devtools.css        # Significant rewrite for new panels
    landing.css         # Unchanged
  worklets/
    capture-processor.js   # Unchanged
    playback-processor.js  # Unchanged
```

## Performance Targets

| Operation | Budget |
|---|---|
| Timeline row append (DOM recycle) | < 0.1ms |
| Minimap full repaint | < 0.5ms |
| State cell update | < 0.05ms |
| Sparkline repaint | < 0.2ms |
| Memory ceiling (10K events) | < 2MB |
| Scroll framerate | 60fps locked |
| Initial page load (all JS) | < 50KB total |
| Audio callback → DOM | Zero interference (separate rAF) |

## What Does NOT Change

- **Landing page** (`index.html`, `landing.css`): untouched
- **Audio pipeline** (`audio.js`, worklets): untouched
- **App HTML structure** (`app.html`): minor — tab names change, minimap canvas added
- **Demo app trait** (`CookbookApp`): untouched
- **WebSocket handler** (`ws_handler.rs`): untouched
- **Audio worklets**: untouched

## Implementation Order

1. **Rendering primitives** — `RingBuffer`, `VirtualList`, `RenderScheduler` (pure JS, testable in isolation)
2. **Timeline panel** — replace Events tab, wire to existing `addEvent()` flow
3. **Canvas minimap** — paint loop, click-to-scroll, viewport overlay
4. **Metrics panel** — surface token counts from existing telemetry, add sparkline
5. **State panel** — targeted updates, diff flash, search
6. **Phases panel** — current phase hero, duration bars, needs checklist
7. **Server: SessionBridge** — reduce app boilerplate
8. **Server: new message variants** — `SpanEvent`, `TurnMetrics`
9. **Server: WebSocketSpanLayer** — bridge tracing spans to browser
10. **OTel export** — "Copy as OTLP JSON" button, trace ID badge

Steps 1-6 are frontend-only (no Rust changes). Steps 7-10 are backend changes. Frontend and backend steps are independently deployable — the new frontend gracefully handles missing new message types.
