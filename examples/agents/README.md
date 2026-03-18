# Agent Examples

Standalone agent examples demonstrating the L1 runtime (`gemini-adk`) and L2 fluent DX (`gemini-adk-fluent`).

## Examples

### Weather Agent

CLI demo: connects to Gemini Live, asks about weather, dispatches a `TypedTool` call, and prints the model's response.

```bash
export GOOGLE_GENAI_API_KEY="your-key"
cargo run -p agents-example --bin weather-agent
```

### Research Pipeline

Demonstrates the full L2 fluent API: `AgentBuilder`, operator combinators (`>>`, `|`, `*`, `/`), composition modules (`S`, `P`, `T`), and pre-built patterns.

```bash
cargo run -p agents-example --bin research-pipeline
```
