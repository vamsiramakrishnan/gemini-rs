# Tool Calling Example

Function calling with `TypedTool` and automatic dispatch in a Gemini Live session.

Defines a weather tool with a typed Rust struct (`schemars::JsonSchema`), auto-generates the JSON Schema, and dispatches tool calls from the model automatically.

## Run

```bash
export GOOGLE_GENAI_API_KEY="your-key"
cargo run -p example-tool-calling
# Open http://127.0.0.1:3003
```

## What it demonstrates

- `TypedTool::new::<WeatherArgs>(...)` with auto-generated JSON Schema
- `ToolDispatcher` for routing function calls
- `SessionEvent::ToolCall` handling and `FunctionResponse` submission
- Axum WebSocket bridge with tool call results forwarded to the browser
