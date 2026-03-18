# ADK Web UI

`adk-web` is the interactive development environment for building and debugging Gemini Live agents.
It runs a single Axum server at `http://localhost:3000` that hosts all demo apps and a shared DevTools panel.

```bash
cargo run -p adk-web
# → http://127.0.0.1:3000
```

---

## Design System

The web UI is built on a CSS design system defined in `apps/adk-web/static/css/design-system.css`.

### Tokens

80+ CSS custom properties cover the full visual language:

| Category | Examples |
|----------|---------|
| **Brand** | `--brand`, `--brand-light`, `--brand-dark`, `--brand-glow` |
| **Surfaces** | `--bg-root`, `--bg-card`, `--bg-elevated`, `--bg-inset`, `--bg-code` |
| **Text** | `--text-1` (primary), `--text-2` (secondary), `--text-3` (muted), `--text-inverse` |
| **Borders** | `--border-1` (subtle), `--border-2` (default), `--border-3` (strong) |
| **Spacing** | `--space-1` through `--space-16` (4px base scale) |
| **Radii** | `--radius-sm`, `--radius-md`, `--radius-lg`, `--radius-xl`, `--radius-full` |
| **Shadows** | `--shadow-sm`, `--shadow-md`, `--shadow-lg`, `--shadow-xl`, `--shadow-glow` |
| **Motion** | `--ease-out`, `--ease-spring`, `--duration-fast`, `--duration-base`, `--duration-slow` |
| **Semantic** | `--success`, `--warning`, `--error`, `--info` and their `-light` / `-dark` variants |

### Typography

| Role | Font | Weights |
|------|------|---------|
| UI text | `Inter` | 300, 400, 500, 600, 700, 800, 900 |
| Code / monospace | `JetBrains Mono` | 400, 500, 600, 700 |

### Dark / Light Mode

Theme is toggled via a button in the navigation bar and persisted to `localStorage` under the key `theme`.
The active theme is set as a `data-theme` attribute on `<html>`:

```html
<html data-theme="dark">   <!-- or "light" -->
```

All design tokens redefine their values under `[data-theme="dark"]`, so every component inherits the
correct colors without any extra CSS.

---

## Landing Page

The `index.html` landing page showcases the SDK before a user starts a session.

| Section | Description |
|---------|-------------|
| **Hero** | Animated gradient orbs, headline, CTA buttons, live stats counters (crates, examples, namespaces) |
| **Architecture diagram** | Three-layer crate stack (L0/L1/L2) with the three-lane processor (Fast/Control/Telemetry) |
| **Operator algebra** | Interactive showcase of S·C·T·P·M·A operators with syntax-highlighted composition examples |
| **Pipeline visualization** | Animated flow diagram of the `>>`, `\|`, `*`, `/` agent combinators |
| **Cookbook browser** | Filterable example gallery with Crawl/Walk/Run difficulty tiers (see below) |
| **Feature highlights** | Cards covering key SDK capabilities: phases, extraction, watchers, async tools |
| **Glassmorphism nav** | Frosted-glass navigation bar with scroll-aware opacity and backdrop blur |

---

## Cookbook Browser

The landing page includes a browsable gallery of all 30 cookbook examples.

- **Filter by tier**: Crawl / Walk / Run buttons filter by difficulty
- **Each card shows**: example number, title, brief description, and a difficulty badge
- **Click to view**: links directly to the source file on GitHub

The same data powers the **Cookbook panel** in DevTools (see below).

---

## DevTools Panel

When a session is active (`app.html`), a DevTools sidebar gives real-time visibility into every layer of the SDK.
Open DevTools by clicking the `</>` button in the top-right corner of any app.

### Panels

| Panel | What it shows |
|-------|---------------|
| **State** | Live key-value state with prefix colouring (`session:`, `turn:`, `app:`, `derived:`) — updates on every turn |
| **Timeline** | Chronological event log with VirtualList rendering — `AudioDelta`, `TextDelta`, `ToolCall`, `TurnComplete`, etc. |
| **Phases** | Current phase, phase history with entry/exit timestamps, duration bars, active guards |
| **Metrics** | Token counts, cost estimation, latency sparkline, turns per minute, audio throughput |
| **Transcript** | Full turn-by-turn conversation transcript with role labels |
| **Artifacts** | Structured data extracted by the extraction pipeline, grouped by schema name |
| **Eval** | Evaluation results when the session runs an `EvalSuite` — scores per criterion |
| **Event Inspector** | Raw `SessionEvent` stream with JSON expansion for any event |
| **Trace** | OpenTelemetry-style span timeline, copy-as-OTLP button, trace ID badge |
| **Cookbook** | Filterable cookbook browser (same data as the landing page gallery) |

### Telemetry Integration

The DevTools panel receives structured data from the server via the same WebSocket connection used for the session.
The server sends additional message types alongside audio/text frames:

- `SpanEvent` — individual OTel span start/end
- `TurnMetrics` — per-turn latency, token counts, tool call count
- `StateUpdate` — delta state snapshot after each control plane cycle

---

## Architecture

```
Browser                              Server (Axum)
───────                              ─────────────
index.html ──── static files ──────► apps/adk-web/static/
app.html   ──── WebSocket ─────────► ws_handler.rs
                                          │
                                     SessionBridge
                                          │
                                     LiveHandle (rs-adk)
                                          │
                                     Gemini Live API (WebSocket)
```

`SessionBridge` (in `apps/adk-web/src/bridge.rs`) wires the `LiveHandle` event stream to the browser
WebSocket connection. It translates `LiveEvent` values into JSON messages the DevTools panels consume.

---

## CSS Files

| File | Purpose |
|------|---------|
| `design-system.css` | Design tokens, typography, theme variables |
| `main.css` | App shell layout: nav bar, sidebar, content areas, DevTools panel |
| `landing.css` | Landing page sections: hero, architecture, algebra, cookbook browser |
| `devtools.css` | DevTools panel chrome: tabs, panel containers, scrollable lists |
