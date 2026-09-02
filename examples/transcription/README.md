# Transcription & Session Config Example

A tour of the voice-session configuration surface of the L2 `Live` builder (`gemini-adk-fluent-rs`) — every option in one place, behind a WebSocket-backed web UI.

## Run

```bash
export GEMINI_API_KEY="your-key"   # or the Vertex AI env vars — see ../INDEX.md
cargo run -p example-transcription
# Open http://127.0.0.1:3004
```

## What it demonstrates

| Option | Builder call |
|--------|--------------|
| Input + output transcription | `.transcription()`, delivered to `on_input_transcript` / `on_output_transcript` |
| Server VAD sensitivity | `.vad(AutomaticActivityDetection { .. })` |
| Activity handling (barge-in) | `.activity_handling(ActivityHandling::StartOfActivityInterrupts)` |
| Turn coverage | `.turn_coverage(TurnCoverage::TurnIncludesOnlyActivity)` |
| Context window compression | `.context_compression(4096, 2048)` — trigger at 4096 tokens, keep a 2048-token window |
| Session resumption | `.session_resume()`; pass the issued handle to `.session_resume_from(..)` on a later connect |
| Affective dialog | `.affective_dialog()` |
| Voice | `.voice(..)`, selectable from the UI |

Plus the callbacks the UI needs — `on_audio`, `on_text`, `on_vad_start` / `on_vad_end`, `on_turn_complete`, `on_interrupted`, `on_error` — and `.connect_from_env()` for auth and the platform-default model (`GEMINI_LIVE_MODEL` overrides it).

Thinking (`.thinking(..).include_thoughts()`) is deliberately left out: the native-audio Live models don't support it.
