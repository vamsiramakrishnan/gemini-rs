# rs-adk

Agent runtime for Gemini Live — tools, streaming, agent transfer, middleware. This is the L1 (runtime) crate that builds on `rs-genai` to provide agent lifecycle, tool dispatch, state management, and the three-lane processor architecture.

## Features

- **Agent trait** with lifecycle hooks for text and live (voice) sessions
- **Tool system** — `ToolFunction`, `StreamingTool`, `TypedTool` with JSON Schema generation
- **State management** — prefixed key-value store with atomic `modify()`, delta tracking
- **Three-lane processor** — fast (audio), control (tools/phases), telemetry (signals)
- **LLM extractors** — structured data extraction from conversation transcripts
- **Phase system** — instruction-scoped conversation phases with tool filtering
- **Middleware chain** — composable request/response processing pipeline
- **Text agents** — 15+ combinators (sequential, parallel, race, route, loop, etc.)

## Quick Start

```rust,ignore
use rs_adk::*;

let tool = SimpleTool::new("get_weather", "Get current weather", |args| async {
    Ok(serde_json::json!({"temp": 72, "unit": "F"}))
});

let session = LiveSessionBuilder::new()
    .model(rs_genai::prelude::GeminiModel::Gemini2_0Flash)
    .instruction("You are a weather assistant.")
    .tool(tool)
    .build()
    .await?;
```

## Documentation

[API Reference (docs.rs)](https://docs.rs/rs-adk)

## License

Apache-2.0
