# Transcription & Session Config Example

Comprehensive Gemini Live session configuration showcase — every configurable property in one example.

## Run

```bash
export GOOGLE_GENAI_API_KEY="your-key"
cargo run -p example-transcription
# Open http://127.0.0.1:3004
```

## What it demonstrates

- Input and output transcription
- Voice Activity Detection (VAD) settings
- Activity handling and barge-in behavior
- Turn coverage configuration
- Context window compression
- Session resumption (`ResumeInfo`)
- Affective dialog
- All `SessionConfig` fields wired through `ConnectBuilder`
