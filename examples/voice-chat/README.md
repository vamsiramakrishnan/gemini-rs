# Voice Chat Example

Bidirectional audio chat with Gemini Live using the L0 wire protocol (`gemini-live`).

Streams microphone PCM audio to `gemini-2.0-flash-live` and plays back the model's audio response, with real-time input and output transcription.

## Run

```bash
export GOOGLE_GENAI_API_KEY="your-key"
cargo run -p example-voice-chat
# Open http://127.0.0.1:3002
```

## What it demonstrates

- Native audio model with `Modality::Audio` output
- `SessionEvent::Audio` for playback, `SessionEvent::InputTranscript` / `SessionEvent::OutputTranscript`
- Voice selection (`Voice::Kore`)
- Browser MediaRecorder to WebSocket to Gemini Live pipeline
