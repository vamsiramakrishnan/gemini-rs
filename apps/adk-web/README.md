# adk-web

Interactive web UI for ADK agent development and debugging. Mirrors upstream `adk web` — provides a browser interface with real-time audio streaming, text chat, tool call visualization, phase transitions, state inspection, trace waterfall, eval results, and artifact browsing.

## Prerequisites

- **Rust toolchain** (stable, 1.75+)
- **Google Cloud project** with the Gemini API enabled
- **Authentication** — one of:
  - `gcloud` CLI authenticated (`gcloud auth application-default login`)
  - `GOOGLE_API_KEY` environment variable (Google AI)
  - Valid access token via `GOOGLE_ACCESS_TOKEN`

## Environment Setup

Create a `.env` file in the **repository root**:

### Google AI (quickest start)

```env
GOOGLE_API_KEY=your-api-key-here
```

### Vertex AI (production)

```env
GOOGLE_CLOUD_PROJECT=your-project-id
GOOGLE_CLOUD_LOCATION=us-central1
GOOGLE_GENAI_USE_VERTEXAI=TRUE
```

## Running

```bash
# From workspace root
cargo run -p adk-web

# Or via just
just run-web
```

Opens at `http://localhost:25125`. The landing page lists all registered agent apps (debt collection, call screening, clinic, restaurant, etc.).

## Embedded Agent Apps

The web UI ships with showcase agents in `src/apps/`:

| App | Description |
|-----|-------------|
| `debt_collection` | Multi-phase debt collection voice agent |
| `call_screening` | Call screening with guardrails |
| `clinic` | Medical clinic scheduling assistant |
| `restaurant` | Restaurant order-taking agent |
| `playbook` | Playbook-driven conversation agent |
| `support` | Customer support with escalation |
| `extractors` | Extraction pipeline demo |
| `guardrails` | Content safety guardrails demo |
| `text_chat` | Basic text chat |
| `voice_chat` | Basic voice chat |
| `tool_calling` | Tool calling demo |
| `all_config` | Full configuration showcase |

## Devtools Panels

The right-side devtools panel includes:

| Panel | Purpose |
|-------|---------|
| **Timeline** | Chronological event stream with virtual scrolling, filter toolbar, minimap |
| **Events** | Per-event JSON inspector with search, type filtering, copy-to-clipboard |
| **State** | Live key-value state viewer with diff flash, prefix group collapsing |
| **Phases** | Phase machine visualization with transition history |
| **Metrics** | Latency sparklines, token counters, health indicators |
| **Traces** | Span waterfall / flame chart with zoom, click-to-inspect detail |
| **Eval** | Evaluation results with pass/fail filtering, per-criterion scores |
| **Artifacts** | Versioned artifact browser with content preview |

## API Endpoints (Embedded)

The web UI also serves REST API endpoints at the same host:

```
POST /api/run              — Execute an agent (JSON request/response)
POST /api/run_sse          — Execute an agent (SSE streaming)
GET  /api/agents           — List registered agents
GET  /api/agents/:name     — Get agent details
GET  /api/sessions         — List sessions
POST /api/sessions         — Create session
GET  /api/sessions/:id     — Get session
DELETE /api/sessions/:id   — Delete session
GET  /api/sessions/:id/events — Get session events
GET  /api/sessions/:id/state  — Get session state
POST /api/sessions/:id/rewind — Rewind to invocation
GET  /api/artifacts/:session_id — List artifacts
GET  /api/debug/health     — Health check
POST /api/eval/run         — Run evaluation
```
