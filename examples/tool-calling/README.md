# Tool Calling Example

Function calling in a Gemini Live session on the L2 fluent crate (`gemini-adk-fluent-rs`).

The two tools are plain `async fn`s under `#[tool("…")]`: the macro derives the JSON Schema from the parameter list, and `.tool(get_weather())` registers each with the session's dispatcher. When the model calls a tool the runtime executes it and sends the response back on its own; the example's `on_tool_call` hook only tells the browser what is being called.

## Run

```bash
export GEMINI_API_KEY="your-key"   # or the Vertex AI env vars — see ../INDEX.md
cargo run -p example-tool-calling
# Open http://127.0.0.1:3003
```

## What it demonstrates

- `#[tool("description")] async fn get_weather(city: String) -> Result<Value, ToolError>` — a typed tool with no args struct, no `TypedTool::new`, no dispatcher wiring
- `Live::builder().text_only().tool(get_weather()).tool(calculate())` — registration
- `on_tool_call` returning `None` — observe the model's calls and let the dispatcher run them; `on_tool_cancelled` when the server withdraws a call
- `.connect_from_env()` and the platform-default Live model (`GEMINI_LIVE_MODEL` overrides it)
- Axum WebSocket bridge, with the model's tool calls surfaced in the chat as `[Calling tool: …]`

## Tools

| Tool | Arguments | Returns |
|------|-----------|---------|
| `get_weather` | `city: String` | mock temperature, condition, humidity, wind |
| `calculate` | `expression: String` | result of `+ - * /` arithmetic (`*` and `/` bind tighter) |

`#[tool]` expands to code rooted at `::gemini_adk_rs`, so `gemini-adk-rs` must be a direct dependency of any crate using it — see `Cargo.toml`.
