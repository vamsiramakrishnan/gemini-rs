# gemini-adk-rs

Agent runtime for Gemini Live — tools, streaming, agent transfer, middleware. This is the L1 (runtime) crate that builds on `gemini-genai-rs` to provide agent lifecycle, tool dispatch, state management, and the three-lane processor architecture.

## Features

- **Agent trait** with lifecycle hooks for text and live (voice) sessions
- **Tool system** — `ToolFunction`, `StreamingTool`, `TypedTool` with JSON Schema generation
- **State management** — prefixed key-value store with atomic `modify()`, delta tracking
- **Three-lane processor** — fast (audio), control (tools/phases), telemetry (signals)
- **LLM extractors** — structured data extraction from conversation transcripts
- **Phase system** — instruction-scoped conversation phases with tool filtering
- **Middleware chain** — composable request/response processing pipeline
- **Text agents** — 15+ combinators: Sequential, Parallel, Loop, Fallback,
  Route, Race, Timeout, MapOver, Tap, Dispatch, Join, and more

## Quick Start

```rust,ignore
use gemini_adk_rs::*;

let tool = SimpleTool::new("get_weather", "Get current weather", |args| async {
    Ok(serde_json::json!({"temp": 72, "unit": "F"}))
});

let session = LiveSessionBuilder::new()
    .model(gemini_genai_rs::prelude::GeminiModel::Gemini2_0Flash)
    .instruction("You are a weather assistant.")
    .tool(tool)
    .build()
    .await?;
```

## Agent-as-Tool

Wrap any `TextAgent` pipeline so the live model can invoke it as a function
call. The agent shares the session `State`, making its writes immediately
visible to watchers and phase transitions:

```rust,ignore
use gemini_adk_rs::TextAgentTool;

let verifier = /* build your TextAgent pipeline */;
let agent_tool = TextAgentTool::from_arc(
    "verify_identity",
    "Verify the caller's identity",
    verifier,
    state.clone(),
);
dispatcher.register(agent_tool);
```

## Documentation

[API Reference (docs.rs)](https://docs.rs/gemini-adk-rs)

## See Also

- [Cookbook examples](../../examples/cookbook) — runnable snippets for tool
  dispatch, state management, phase machines, and text agent combinators.

## License

MIT
