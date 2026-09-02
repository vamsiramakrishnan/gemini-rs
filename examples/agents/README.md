# Agent Examples

Standalone agent examples on the L2 fluent DX (`gemini-adk-fluent-rs`).

## Examples

### Weather Agent

CLI demo: opens a text-only Live session with two `#[tool]` functions (`get_weather`, `get_forecast`), asks about the weather, lets the runtime dispatch the model's tool calls, and prints the streamed answer. Auth and model come from the environment via `connect_from_env()` (`GEMINI_LIVE_MODEL` overrides the platform default).

```bash
export GEMINI_API_KEY="your-key"   # or the Vertex AI env vars — see ../INDEX.md
cargo run -p example-agents --bin weather-agent
```

### Research Pipeline

Demonstrates the full L2 fluent API: `AgentBuilder`, operator combinators (`>>`, `|`, `*`, `/`), composition modules (`S`, `P`, `T`), and pre-built patterns. Builds and validates the pipeline offline — no API key required.

```bash
cargo run -p example-agents --bin research-pipeline
```
