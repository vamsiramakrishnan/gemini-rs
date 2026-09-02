# Text Chat Example

Simple text-only chat with Gemini Live using the L0 wire protocol (`gemini-genai-rs`).

Connects to the native-audio Live model (`ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO`) in text-only mode, sends user text, and streams back text responses over a WebSocket-backed web UI.

## Run

```bash
export GOOGLE_GENAI_API_KEY="your-key"
cargo run -p example-text-chat
# Open http://127.0.0.1:3001
```

## What it demonstrates

- `connect(config)` with `Modality::Text` output
- WebSocket event loop: `SessionEvent::TextDelta`, `SessionEvent::TurnComplete`, `SessionEvent::Error`
- Axum WebSocket bridge between browser and Gemini Live
