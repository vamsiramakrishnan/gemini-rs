# Text Chat Example

Simple text-only chat with Gemini Live using the L0 wire protocol (`gemini-genai-rs`).

Connects to `gemini-2.0-flash-live` in text-only mode, sends user text, and streams back text responses over a WebSocket-backed web UI.

## Run

```bash
export GOOGLE_GENAI_API_KEY="your-key"
cargo run -p example-text-chat
# Open http://127.0.0.1:3001
```

## What it demonstrates

- `ConnectBuilder` with `Modality::Text` output
- WebSocket event loop: `SessionEvent::Text`, `SessionEvent::TurnComplete`
- Axum WebSocket bridge between browser and Gemini Live
