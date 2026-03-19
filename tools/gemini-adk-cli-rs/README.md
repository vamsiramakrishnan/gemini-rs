# gemini-adk-cli-rs

Command-line interface for ADK agent development. Mirrors the upstream `adk` CLI — scaffold, run, serve, evaluate, and deploy agents.

## Prerequisites

- **Rust toolchain** (stable, 1.75+)
- **Google API key or GCP credentials** for agent execution
- **Docker** (for `deploy cloud_run` and `deploy gke`)
- **gcloud CLI** (for `deploy cloud_run` and `deploy agent_engine`)
- **kubectl** (for `deploy gke`)

## Installation

```bash
# From workspace root
cargo install --path tools/gemini-adk-cli-rs

# Or run directly
cargo run -p gemini-adk-cli-rs -- <command>
```

The binary is named `adk`.

## Commands

### `adk create` — Scaffold a new agent

Creates a new agent project directory with boilerplate files.

```bash
adk create my_agent
adk create my_agent --model gemini-2.5-pro
adk create my_agent --api-key sk-abc123
```

**Generated files:**

```
my_agent/
├── agent.toml       # Agent configuration
├── Cargo.toml       # Rust project manifest
├── src/main.rs      # Entry point
├── .env             # API key
└── .gitignore
```

**`agent.toml` format (TOML):**

```toml
name = "my_agent"
description = "A new ADK agent"
model = "gemini-2.0-flash"
instruction = "You are a helpful assistant."
tools = []
sub_agents = []
```

### `adk run` — Interactive terminal REPL

Run an agent interactively in the terminal.

```bash
# Basic usage — point at agent directory
adk run my_agent/

# Resume a session
adk run my_agent/ --session-id "session-abc-123"

# Save transcript on exit
adk run my_agent/ --save-session transcript.json

# Replay a saved session (non-interactive)
adk run my_agent/ --replay transcript.json
```

**REPL commands:**
- Type messages normally to chat with the agent
- `/quit` or `/exit` — exit the REPL
- `Ctrl+D` (EOF) — exit the REPL

**Saved session format:**

```json
[
  { "user": "What's the weather in Tokyo?", "agent": "The weather in Tokyo is..." },
  { "user": "How about NYC?", "agent": "In New York City..." }
]
```

### `adk web` — Development web server with UI

Start the web UI for interactive agent development.

```bash
adk web my_agent/
adk web my_agent/ --port 9000
adk web my_agent/ --host 0.0.0.0 --reload
adk web my_agent/ --a2a --trace-to-cloud
```

**Flags:**

| Flag | Default | Description |
|------|---------|-------------|
| `--host` | `127.0.0.1` | Bind address |
| `--port` | `8000` | Port |
| `--allow-origins` | all | Comma-separated CORS origins |
| `--log-level` | `info` | Log level (trace/debug/info/warn/error) |
| `--reload` | off | Auto-reload on file changes |
| `--a2a` | off | Enable A2A protocol endpoint |
| `--trace-to-cloud` | off | Export traces to Cloud Trace |
| `--session-service-uri` | in-memory | External session service URI |
| `--artifact-storage-uri` | in-memory | External artifact storage URI |

### `adk api` — Headless API server

Start a REST API server without the web UI. Same flags as `adk web`.

```bash
adk api my_agent/
adk api my_agent/ --port 8080 --allow-origins "http://localhost:3000"
```

**Endpoints served:**

```
GET  /list-apps                                    — List discovered agents
POST /run                                          — Execute agent (JSON)
POST /run_sse                                      — Execute agent (SSE stream)
GET  /apps/:app/users/:user/sessions/:session      — Get session
POST /apps/:app/users/:user/sessions               — Create session
DELETE /apps/:app/users/:user/sessions/:session     — Delete session
GET  /debug/trace/:event_id                        — Get trace spans
GET  /debug/trace/session/:session_id              — Get session traces
```

### `adk eval` — Run evaluations

Evaluate an agent against a test set.

```bash
# Basic eval
adk eval my_agent/ tests/weather.evalset.json

# With scoring config and detailed output
adk eval my_agent/ tests/weather.evalset.json \
  --config-file tests/test_config.json \
  --print-detailed-results
```

**`.evalset.json` format:**

```json
{
  "name": "Weather Agent Tests",
  "cases": [
    {
      "id": "tokyo_weather",
      "inputs": ["What is the weather in Tokyo?"],
      "expected": ["sunny", "temperature"],
      "tags": ["basic"]
    },
    {
      "id": "multi_turn",
      "inputs": [
        "What's the weather in NYC?",
        "How about tomorrow?"
      ],
      "expected": ["forecast"],
      "tags": ["multi-turn"]
    }
  ]
}
```

**`test_config.json` format:**

```json
{
  "pass_threshold": 0.7,
  "criteria": [
    { "name": "response_match_score", "weight": 1.0 },
    { "name": "tool_trajectory_avg_score", "weight": 0.5, "description": "Tool usage accuracy" }
  ]
}
```

**Available evaluation criteria:**

| Criterion | Description |
|-----------|-------------|
| `response_match_score` | Fuzzy match against expected outputs |
| `final_response_match_v2` | Improved response matching with semantic similarity |
| `tool_trajectory_avg_score` | Tool call sequence accuracy |
| `rubric_based_final_response_quality_v1` | LLM-as-judge response quality scoring |
| `rubric_based_tool_use_quality_v1` | LLM-as-judge tool use scoring |
| `hallucinations_v1` | Hallucination detection (PII, prompt injection, data leakage) |
| `safety_v1` | Safety policy compliance |
| `per_turn_user_simulator_quality_v1` | Multi-turn conversation quality |

### `adk deploy` — Deploy to cloud

Deploy an agent to a cloud target.

```bash
# Cloud Run
adk deploy cloud_run my_agent/ --project my-gcp-project --region us-central1

# With web UI bundled
adk deploy cloud_run my_agent/ --project my-gcp-project --with-ui

# GKE
adk deploy gke my_agent/ --project my-gcp-project --service-name weather-svc

# Vertex AI Agent Engine
adk deploy agent_engine my_agent/ --project my-gcp-project
```

**Cloud Run** generates a `Dockerfile` and prints the `gcloud run deploy` command.

**GKE** generates a `Dockerfile` + `k8s.yaml` (Deployment + Service) and prints `docker build` / `kubectl apply` commands.

**Agent Engine** deploys to Vertex AI Agent Engine (API integration pending).

**Flags:**

| Flag | Default | Description |
|------|---------|-------------|
| `--project` | required | GCP project ID |
| `--region` | `us-central1` | GCP region |
| `--service-name` | agent name | Override service name |
| `--with-ui` | off | Bundle the web UI |
| `--trace-to-cloud` | off | Enable Cloud Trace export |

## Agent Discovery

The CLI discovers agents by looking for manifest files:

1. **`agent.toml`** — TOML format (used by `adk create`, `adk run`, `adk eval`, `adk deploy`)
2. **`agent.json` / `root_agent.json`** — JSON format (used by `gemini-adk-api-rs`, matches upstream ADK)

Both formats support the same fields. The CLI scans:
- `<agent_dir>/agent.toml` (single agent)
- `<agent_dir>/<subdir>/agent.toml` (multi-agent project with sub-agents in subdirectories)

## Project Structure

```
tools/gemini-adk-cli-rs/
├── src/
│   ├── main.rs              — CLI entry point (clap derive)
│   ├── manifest.rs          — Agent manifest loading and discovery
│   └── commands/
│       ├── mod.rs           — Command module re-exports
│       ├── create.rs        — Project scaffolding
│       ├── run.rs           — Interactive REPL
│       ├── web.rs           — Web server with UI
│       ├── api.rs           — Headless API server
│       ├── eval.rs          — Evaluation runner
│       └── deploy.rs        — Cloud deployment
└── Cargo.toml
```
