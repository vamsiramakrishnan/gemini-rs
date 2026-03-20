# gemini-adk-api-rs

Standalone headless REST API server for ADK agents. Mirrors upstream `adk api_server` — auto-discovers agent configs from the current directory and exposes them via REST endpoints for programmatic access.

## Prerequisites

- **Rust toolchain** (stable, 1.75+)
- **Agent config file** — at least one `agent.json` or `root_agent.json` in the working directory (or subdirectories)
- **Authentication** — one of:
  - `GOOGLE_API_KEY` environment variable (Google AI)
  - `gcloud auth application-default login` (Vertex AI)

## Agent Config Format

The API server discovers agents by scanning the working directory for `agent.json` files. Example:

```json
{
  "name": "weather_agent",
  "model": "gemini-2.0-flash",
  "instruction": "You are a helpful weather assistant.",
  "description": "Provides weather information for cities.",
  "tools": [
    { "name": "get_weather", "description": "Get weather for a city" },
    { "builtin": "google_search" }
  ],
  "sub_agents": [
    { "name": "forecast_agent", "instruction": "Provide 5-day forecasts." }
  ],
  "temperature": 0.3,
  "output_key": "weather_result",
  "max_llm_calls": 10
}
```

### Config Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Agent identifier |
| `model` | string | no | Model ID (e.g. `gemini-2.0-flash`, `gemini-2.5-pro`) |
| `instruction` | string | no | System instruction |
| `description` | string | no | Human-readable description |
| `tools` | array | no | Tool declarations (custom `name`+`description` or `builtin`) |
| `sub_agents` | array | no | Nested agent configs for multi-agent hierarchies |
| `temperature` | float | no | Generation temperature (0.0–2.0) |
| `max_output_tokens` | int | no | Max output tokens |
| `thinking_budget` | int | no | Thinking budget (Google AI only) |
| `output_key` | string | no | Auto-save agent response to this state key |
| `output_schema` | object | no | JSON Schema for structured output |
| `max_llm_calls` | int | no | Safety limit on LLM calls per run |
| `agent_type` | string | no | `"llm"` (default), `"sequential"`, `"parallel"`, `"loop"` |
| `max_iterations` | int | no | For loop agents: max iterations |
| `voice` | string | no | Voice name for live agents |
| `greeting` | string | no | Model speaks first on connect |
| `transcription` | bool | no | Enable transcription |
| `a2a` | bool | no | Enable A2A protocol endpoint |
| `env` | object | no | Environment variables to set |
| `metadata` | object | no | Custom metadata passed to state/callbacks |

### Built-in Tool Names

Use `{ "builtin": "<name>" }` for built-in tools:

- `google_search` — Google Search grounding
- `code_execution` — Code execution sandbox
- `url_context` — URL content fetching

## Running

### Standalone binary

```bash
# From workspace root — discovers agents from current directory
cargo run -p gemini-adk-api-rs

# Custom port
ADK_API_PORT=8080 cargo run -p gemini-adk-api-rs

# With RUST_LOG for debug output
RUST_LOG=debug cargo run -p gemini-adk-api-rs
```

### Via the CLI

```bash
# Point at an agent directory containing agent.json or agent.toml
adk api my_agent/

# Custom host/port
adk api my_agent/ --host 0.0.0.0 --port 9000

# With CORS origins
adk api my_agent/ --allow-origins "http://localhost:3000,http://localhost:5173"

# With A2A protocol and Cloud Trace
adk api my_agent/ --a2a --trace-to-cloud
```

## REST API Reference

### Agent Execution

#### `POST /run`

Execute an agent with a user message.

```bash
curl -X POST http://localhost:8000/run \
  -H "Content-Type: application/json" \
  -d '{
    "agent": "weather_agent",
    "message": "What is the weather in Tokyo?",
    "user_id": "user-123",
    "session_id": "optional-session-id"
  }'
```

Response:

```json
{
  "session_id": "abc-123",
  "response": "The weather in Tokyo is...",
  "events": [...],
  "state": { "weather_result": "..." }
}
```

#### `POST /run_sse`

Same as `/run` but streams events via Server-Sent Events.

```bash
curl -X POST http://localhost:8000/run_sse \
  -H "Content-Type: application/json" \
  -d '{"agent": "weather_agent", "message": "Forecast for NYC"}' \
  --no-buffer
```

### Agent Discovery

#### `GET /list-apps`

List all discovered agents.

```bash
curl http://localhost:8000/list-apps
```

```json
[
  {
    "name": "weather_agent",
    "description": "Provides weather information",
    "model": "gemini-2.0-flash",
    "agent_type": "llm",
    "tools": ["get_weather", "google_search"],
    "sub_agents": ["forecast_agent"]
  }
]
```

#### `GET /apps/:name`

Get a single agent's details.

### Session Management

All session endpoints follow the pattern `/apps/:app/users/:user/sessions/...` matching the upstream ADK URL structure.

#### `GET /apps/:app/users/:user/sessions`

List sessions for an app+user pair.

```bash
curl "http://localhost:8000/apps/weather_agent/users/user-123/sessions?limit=10"
```

#### `POST /apps/:app/users/:user/sessions`

Create a new session.

```bash
curl -X POST http://localhost:8000/apps/weather_agent/users/user-123/sessions
```

#### `GET /apps/:app/users/:user/sessions/:id`

Get session details including state and events.

#### `DELETE /apps/:app/users/:user/sessions/:id`

Delete a session. Returns `204 No Content` on success.

#### `GET /apps/:app/users/:user/sessions/:id/events`

Get all events in a session.

#### `GET /apps/:app/users/:user/sessions/:id/state`

Get current session state as key-value map.

#### `POST /apps/:app/users/:user/sessions/:id/rewind`

Rewind a session to a previous invocation. All events after the given invocation ID are removed.

```bash
curl -X POST http://localhost:8000/apps/weather_agent/users/user-123/sessions/abc-123/rewind \
  -H "Content-Type: application/json" \
  -d '{"invocation_id": "inv-456"}'
```

### Artifacts

#### `GET /apps/:app/users/:user/sessions/:session/artifacts`

List artifacts for a session.

#### `GET /apps/:app/users/:user/sessions/:session/artifacts/:name`

Get latest version of a named artifact.

#### `GET /apps/:app/users/:user/sessions/:session/artifacts/:name/:version`

Get a specific version of an artifact.

### Debug / Traces

#### `GET /debug/health`

Health check endpoint.

```bash
curl http://localhost:8000/debug/health
```

```json
{
  "status": "healthy",
  "version": "0.3.0",
  "agents_loaded": 2,
  "sessions_active": 5
}
```

#### `GET /debug/trace/:trace_id`

Get spans for a trace ID (requires OpenTelemetry integration).

### Evaluation

#### `POST /eval/run`

Submit an evaluation run.

```bash
curl -X POST http://localhost:8000/eval/run \
  -H "Content-Type: application/json" \
  -d '{
    "agent": "weather_agent",
    "eval_set": "weather_tests.evalset.json",
    "criteria": ["response_match_score", "tool_trajectory_avg_score"]
  }'
```

#### `GET /eval/results`

List previous evaluation results.

## Architecture

```
gemini-adk-api-rs
├── src/
│   ├── main.rs        — Server startup, agent discovery, router setup
│   └── handlers.rs    — All REST endpoint handler functions
└── Cargo.toml
```

The server uses:
- **Axum** for HTTP routing
- **gemini-adk-rs** `discover_agent_configs()` for scanning `agent.json` files
- **In-memory stores** for sessions and artifacts (swap for DB-backed in production via `SessionService` trait)
- **tower-http CORS** for cross-origin access

## Production Deployment

For production, replace the in-memory stores:

```rust
// Use SQLite for sessions
let session_service = SqliteSessionService::new("sessions.db").await?;

// Use PostgreSQL for sessions
let session_service = PostgresSessionService::new(postgres_config).await?;

// Use Vertex AI for sessions
let session_service = VertexAiSessionService::new(vertex_config);

// Use file-based artifact storage
let artifact_service = FileArtifactService::new("/data/artifacts");

// Use GCS for artifacts (feature-gated)
let artifact_service = GcsArtifactService::new("my-bucket", "prefix/");
```

Deploy via the CLI:

```bash
# Cloud Run
adk deploy cloud_run my_agent/ --project my-gcp-project --region us-central1

# GKE
adk deploy gke my_agent/ --project my-gcp-project

# Vertex AI Agent Engine
adk deploy agent_engine my_agent/ --project my-gcp-project
```
