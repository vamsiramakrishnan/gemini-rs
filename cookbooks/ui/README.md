# Cookbooks UI

Web-based tester for `gemini-live-rs` voice AI applications. Provides a browser
interface with real-time audio streaming, text chat, tool call visualization,
phase transitions, and state inspection.

## Prerequisites

- Rust toolchain (stable)
- A Google Cloud project with the Gemini API enabled
- `gcloud` CLI authenticated, **or** a valid access token

## Environment Setup

Create a `.env` file in the **repository root** (not in `cookbooks/ui/`):

### Vertex AI (recommended)

```env
# Google Cloud project ID
GOOGLE_CLOUD_PROJECT=vital-octagon-19612

# Location for Live session models (native audio is only in us-central1)
GOOGLE_CLOUD_LOCATION=us-central1

# Enable Vertex AI backend
GOOGLE_GENAI_USE_VERTEXAI=TRUE

# Default Live session model
GEMINI_MODEL=gemini-live-2.5-flash-native-audio
```

### Google AI (API key)

```env
GOOGLE_GENAI_API_KEY=your-api-key-here
GEMINI_MODEL=gemini-live-2.5-flash-native-audio
```

### Authentication

The server resolves credentials in this order:

1. `GOOGLE_ACCESS_TOKEN` environment variable
2. `GCLOUD_ACCESS_TOKEN` environment variable
3. `gcloud auth print-access-token` (automatic CLI fallback)

For local development, `gcloud auth print-access-token` works automatically if
you are logged in. For CI or containers, set `GOOGLE_ACCESS_TOKEN` explicitly.

## Model Routing

The cookbook apps use **two different models** that route to **different Vertex AI
locations**:

| Model | Purpose | Location | Endpoint |
|---|---|---|---|
| `gemini-live-2.5-flash-native-audio` | Live WebSocket voice session | `us-central1` | `us-central1-aiplatform.googleapis.com` |
| `gemini-2.5-flash-lite` | Background LLM extraction agent | `global` | `aiplatform.googleapis.com` |

The Live session model and location come from `.env` (`GEMINI_MODEL` and
`GOOGLE_CLOUD_LOCATION`). The background LLM model and location are set
explicitly per-app in code via `GeminiLlmParams`:

```rust
let llm: Arc<dyn BaseLlm> = Arc::new(GeminiLlm::new(GeminiLlmParams {
    model: Some("gemini-2.5-flash-lite".to_string()),
    location: Some("global".to_string()),
    ..Default::default()  // inherits project + vertexai from env
}));
```

This separation is necessary because the native audio model is region-locked to
`us-central1`, while `gemini-2.5-flash-lite` is available at the `global`
endpoint.

## OpenTelemetry Export (Optional)

Two export backends are available, each behind its own feature flag:

| Feature | Backend | Auth | Use Case |
|---|---|---|---|
| `otel` / `otel-otlp` | Generic OTLP (gRPC) | None (collector handles auth) | Jaeger, any OTLP collector |
| `otel-gcp` | Google Cloud Trace + Cloud Monitoring | Automatic ADC | Direct export to Google Cloud |

### Google Cloud (recommended for Vertex AI users)

```bash
cargo run -p rs-genai-ui --features otel-gcp
```

Set your project in `.env`:

```env
GOOGLE_CLOUD_PROJECT=your-project-id
```

Auth is automatic via Application Default Credentials (ADC) — the same
`gcloud auth application-default login` credentials used for Vertex AI. No
collector or proxy required.

Traces appear in [Cloud Trace](https://console.cloud.google.com/traces) and
metrics in [Cloud Monitoring](https://console.cloud.google.com/monitoring)
under `custom.googleapis.com/gemini-live-cookbooks/`.

### Jaeger (local development)

```bash
# Start Jaeger with OTLP support
docker run -d --name jaeger \
  -p 4317:4317 \
  -p 16686:16686 \
  jaegertracing/jaeger:latest

# Run with OTLP enabled
cargo run -p rs-genai-ui --features otel-otlp

# View traces at http://localhost:16686
```

Configure the OTLP endpoint in `.env`:

```env
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
OTEL_SERVICE_NAME=gemini-live-cookbooks
```

### Exported Telemetry

**Traces** (14 span types across L0 + L1):

| Layer | Span | Key Attributes |
|---|---|---|
| L0 | `rs_genai.session` | `session_id` |
| L0 | `rs_genai.connect` | `url` |
| L0 | `rs_genai.setup` | `session_id` |
| L0 | `rs_genai.send_audio` | `chunk_size`, `session_id` |
| L0 | `rs_genai.receive_content` | `session_id` |
| L0 | `rs_genai.tool_call` | `function_name`, `session_id` |
| L0 | `rs_genai.tool_response` | `session_id` |
| L0 | `rs_genai.disconnect` | `session_id`, `reason` |
| L0 | `rs_genai.http_request` | `http.method`, `http.url` |
| L1 | `gemini.agent.run` | `agent_name`, `session_id` |
| L1 | `gemini.agent.transfer` | `from`, `to`, `session_id` |
| L1 | `gemini.agent.tool_dispatch` | `tool_name`, `tool_class`, `session_id` |
| L1 | `gemini.agent.agent_tool` | `agent_name`, `parent_agent` |
| L1 | `gemini.agent.runner` | `root_agent` |

**Metrics** (12 counters/histograms/gauges via Prometheus-style `metrics` crate):

Sessions, audio latency, response latency, jitter buffer, tool calls, VAD
events, reconnections, WebSocket bytes, HTTP requests.

## Running

```bash
cargo run -p rs-genai-ui
```

The server binds to `0.0.0.0:25125`. Open your browser at:

```
http://localhost:25125
```

### Port Configuration

The server listens on **port 25125** on all interfaces (`0.0.0.0`). If you need
to expose this from a container or firewall:

```bash
# Docker
docker run -p 25125:25125 ...

# Firewall (ufw)
ufw allow 25125/tcp
```

## Available Apps

The landing page lists all registered cookbook apps. Each app demonstrates
different SDK features:

| App | Features |
|---|---|
| `voice-chat` | Basic voice streaming |
| `text-chat` | Text-only mode |
| `tool-calling` | Function calling with mock tools |
| `call-screening` | Phase machine, LLM extraction, watchers, temporal patterns |
| `clinic` | Symptom triage, department routing, patient registration |
| `restaurant` | Order management, multi-phase flow |
| `debt-collection` | Compliance guardrails, payment processing |
| `guardrails` | Rule-based output filtering |
| `playbook` | Scripted conversation flows |
| `support` | Customer support patterns |
| `all-config` | Full configuration showcase |

## Usage

1. Select an app from the landing page
2. Optionally configure voice and system instruction overrides
3. Click **Connect** to establish a WebSocket session
4. Use the microphone button for voice input, or type text messages
5. Audio responses play automatically; transcripts appear in real time
6. The devtools panel shows state updates, phase transitions, tool calls, and
   telemetry
