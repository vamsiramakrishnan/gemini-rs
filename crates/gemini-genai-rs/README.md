# gemini-genai-rs

Raw wire protocol and transport for the Gemini Multimodal Live API. This is the L0 (foundation) crate in the gemini-rs workspace — it handles WebSocket connections, authentication, wire-format types, and audio buffering with no agent abstractions.

## Features

- **Protocol types** mapping 1:1 to the Gemini Live API wire format
- **WebSocket transport** with Vertex AI and Google AI authentication
- **Lock-free audio buffers** (SPSC ring buffer, adaptive jitter buffer)
- **Voice activity detection** with adaptive noise floor
- **Feature-gated REST APIs** (generate, embed, files, models, tokens, caches, tunings, batches)
- **Pluggable architecture** via `Transport`, `Codec`, and `AuthProvider` traits

## Quick Start

```rust,ignore
use gemini_genai_rs::prelude::*;

let config = TransportConfig::google_ai("YOUR_API_KEY", GeminiModel::Gemini2_0Flash);
let (handle, events) = connect(config).await?;

handle.send_text("Hello!").await?;
while let Some(event) = events.recv().await {
    // Handle server events
}
```

## Feature Flags

The crate is split into opt-in feature flags so you only compile what you need.
The default build includes Live WebSocket support, VAD, and tracing.

| Feature | Enables | Default |
|---------|---------|---------|
| `live` | WebSocket Live API session types and transport | yes |
| `vad` | Voice activity detection (shared base) | yes |
| `vad-wavekat` | VAD powered by the `wavekat-vad` model (implies `vad`) | yes |
| `tracing-support` | `tracing` + `tracing-subscriber` integration | yes |
| `http` | HTTP client (`reqwest`) — required by all REST API features | no |
| `generate` | `generateContent` REST endpoint (implies `http`) | no |
| `tokens` | Token counting REST endpoint (implies `http`) | no |
| `models` | Model listing and metadata REST endpoint (implies `http`) | no |
| `files` | File upload and management REST endpoint (implies `http`) | no |
| `embed` | Text embeddings REST endpoint (implies `http`) | no |
| `caches` | Context caching REST endpoint (implies `http`) | no |
| `tunings` | Fine-tuning jobs REST endpoint (implies `http`) | no |
| `batches` | Batch prediction REST endpoint (implies `http`) | no |
| `chats` | Multi-turn chat sessions (implies `generate`) | no |
| `all-apis` | All REST API features above | no |
| `opus` | Opus audio codec via `audiopus` | no |
| `metrics` | Prometheus metrics exporter | no |
| `otel-base` | Shared OpenTelemetry deps (traces + metrics) | no |
| `otel-otlp` | Generic OTLP exporter over gRPC/tonic (implies `otel-base`) | no |
| `otel-gcp` | Google Cloud Trace + Cloud Monitoring exporters (implies `otel-base`) | no |
| `otel` | Alias for `otel-otlp` | no |

**Example — add REST generation and token counting:**

```toml
[dependencies]
gemini-genai-rs = { version = "0.8", features = ["generate", "tokens"] }
```

**Enable everything:**

```toml
gemini-genai-rs = { version = "0.8", features = ["all-apis", "metrics", "opus"] }
```

## Voice Activity Detection (VAD)

The `vad` / `vad-wavekat` features provide a client-side voice activity
detector that can be used alongside or instead of the server-side VAD built
into Gemini Live. It applies an adaptive noise-floor model to incoming PCM
frames and emits start/end events:

- `vad-wavekat` (default): uses the `wavekat-vad` ML model for more accurate
  speech boundary detection.
- `vad` alone: lightweight energy-based detector with no ML dependency.

The detector is exposed as `VoiceActivityDetector` / `VadConfig` and is used
internally by the three-lane processor to power soft-turn detection
(`SoftTurnDetector`) in L1.

## Documentation

[API Reference (docs.rs)](https://docs.rs/gemini-genai-rs)

## See Also

- [Cookbook examples](../../examples/cookbook) — runnable snippets covering
  quick-connect, REST generate, file upload, and more.

## License

MIT
