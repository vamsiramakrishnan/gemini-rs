# Text Chat Example

Simple text-only chat with Gemini Live on the L2 fluent crate (`gemini-adk-fluent-rs`).

One `Live::builder()` per browser tab: `.text_only()` asks the Live model for text responses, `.connect_from_env()` resolves the platform and credentials, and a handful of callbacks stream the reply to a WebSocket-backed web UI. No microphone required.

## Run

```bash
export GEMINI_API_KEY="your-key"   # or the Vertex AI env vars — see ../INDEX.md
cargo run -p example-text-chat
# Open http://127.0.0.1:3001
```

## What it demonstrates

- `Live::builder().text_only()` — text responses from a Live session
- `.connect_from_env()` — Google AI vs Vertex AI resolved from `GOOGLE_GENAI_USE_VERTEXAI`, credentials from the standard env vars, `gcloud auth print-access-token` as the Vertex fallback
- No `.model(..)` — connect picks the model the platform serves; `GEMINI_LIVE_MODEL` overrides it
- `on_text` / `on_text_complete` / `on_turn_complete` / `on_interrupted` / `on_error` in place of a hand-written event loop
- Axum WebSocket bridge between the browser and the session

## A note on models

Google AI's Live catalog serves native-audio models (the default here is `models/gemini-2.5-flash-native-audio-latest`); they answer in audio unless asked for text, which is exactly what `.text_only()` does. On Vertex AI the default is `gemini-live-2.5-flash-native-audio`, likewise switched to text output.
