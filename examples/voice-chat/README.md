# Voice Chat Example

Bidirectional audio chat with Gemini Live on the L2 fluent crate (`gemini-adk-fluent-rs`).

Streams microphone PCM from the browser into a native-audio Live session and plays the model's voice back, with real-time input and output transcription. The voice is selectable from the UI (Puck, Charon, Kore, Fenrir, Aoede).

## Run

```bash
export GEMINI_API_KEY="your-key"   # or the Vertex AI env vars — see ../INDEX.md
cargo run -p example-voice-chat
# Open http://127.0.0.1:3002
```

## What it demonstrates

- `Live::builder().voice(..).transcription().connect_from_env()` — a native-audio session with no auth ceremony and no hand-picked model (the platform default is used; `GEMINI_LIVE_MODEL` overrides it)
- `on_audio` — the model's PCM16 24 kHz audio, relayed to the browser as base64
- `on_input_transcript` / `on_output_transcript` — what the user said and what the model said, as text
- `on_vad_start` / `on_vad_end` — server-side voice activity, driving the speaking indicator
- Fast-lane discipline: every callback only pushes onto an `mpsc` channel; the WebSocket task does the encoding and the network I/O
- Browser microphone → WebSocket → `handle.send_audio(..)` pipeline
