# gemini-genai-rs

The wire layer for Google's Gemini Live API in Rust: a WebSocket session
with typed events, the setup/realtime message vocabulary, Google AI and
Vertex AI authentication, and the audio primitives a realtime client needs.
It is the L0 crate of the gemini-rs workspace, with no agent abstractions.
Applications usually want the L2 crate, `gemini-adk-fluent-rs`; this crate
is for anyone who needs the protocol itself.

## Quick start

```rust,no_run
use gemini_genai_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), SessionError> {
    // Unset model → the platform's current native-audio Live model
    // (`GEMINI_MODEL` overrides). Output transcription makes the answer readable.
    let config = SessionConfig::new(std::env::var("GEMINI_API_KEY").unwrap())
        .output_transcription(true);
    let session = connect(config).await?;

    let mut events = session.subscribe();
    session.send_text("What is the speed of light?").await?;
    while let Some(event) = recv_event(&mut events).await {
        match event {
            SessionEvent::OutputTranscription(text) => print!("{text}"),
            SessionEvent::TurnComplete => break,
            SessionEvent::Error(e) => eprintln!("{e}"),
            _ => {}
        }
    }
    session.disconnect().await
}
```

Vertex AI is the same session with a different endpoint. Tokens live about
an hour, so give a long-lived session a refreshing source:

```rust,no_run
use gemini_genai_rs::prelude::*;

# fn fetch_token() -> String { String::new() }
let config = SessionConfig::from_endpoint(ApiEndpoint::vertex_refreshing(
    "my-project",
    "us-central1",
    fetch_token,
));
```

Timeouts, reconnection policy, a custom transport or codec, and wire
recording go through `ConnectBuilder`; `connect(config)` is the same path
with none of the options.

## What is in the box

- **Protocol types** mapping one-to-one to the Live API wire format, with
  builders for the parts you write (`Content::user(..)`, `Part::text(..)`,
  `Tool::function(..)`).
- **A session** (`SessionHandle`): `send_audio`/`send_text`/`send_video`/
  tool responses in, a broadcast of `SessionEvent` out, reconnection with
  backoff and session-resumption handles, GoAway as a typed event.
- **Authentication** for Google AI (API key or OAuth token) and Vertex AI
  (bearer token, static or refreshing), and platform differences handled on
  the wire (Vertex strips async-tool and thinking fields it does not accept).
- **Audio primitives**: a lock-free SPSC ring, an adaptive jitter buffer,
  client-side voice activity detection, barge-in and turn detection.
- **REST surfaces** behind feature flags: `generateContent`, embeddings,
  token counting, files, caches, tunings, batches, chats.

## Feature flags

The default build is the Live protocol plus a TLS backend. Everything
heavier is opt-in.

| Feature | Enables | Default |
|---------|---------|---------|
| `live` | Live WebSocket session types and transport | yes |
| `tls-native` | TLS via the platform's native library (enable exactly one TLS backend) | yes |
| `tls-rustls` | TLS via rustls with native root certificates | no |
| `vad` | Energy-based client-side voice activity detection | no |
| `vad-wavekat` | VAD backed by the `wavekat-vad` model (implies `vad`) | no |
| `http` | HTTP client (`reqwest`), required by every REST feature | no |
| `generate`, `embed`, `tokens`, `models`, `files`, `caches`, `tunings`, `batches` | The corresponding REST endpoint (each implies `http`) | no |
| `chats` | Multi-turn chat sessions over `generate` | no |
| `all-apis` | Every REST feature above | no |
| `tracing-subscriber` | The `fmt`/`EnvFilter` subscriber behind `TelemetryConfig::init` | no |
| `metrics` | Prometheus metrics exporter | no |
| `otel-otlp` / `otel-gcp` | OpenTelemetry export over OTLP, or to Google Cloud Trace and Monitoring | no |

The `tracing` facade itself is always compiled; spans are no-ops until a
subscriber is installed.

```toml
[dependencies]
gemini-genai-rs = { version = "2", features = ["generate", "tokens"] }
```

## Voice activity detection

`VoiceActivityDetector` (feature `vad`) runs client-side, alongside or
instead of the server's detection: an adaptive noise floor over incoming
PCM frames that emits speech start and end events. `vad-wavekat` swaps in
the `wavekat-vad` model for tighter speech boundaries. The L1 runtime uses
it for soft-turn detection and client-authority interruption.

## Documentation

[API reference on docs.rs](https://docs.rs/gemini-genai-rs) · the
[gemini-rs book](https://vamsiramakrishnan.github.io/gemini-rs/) for the
full stack.

## License

MIT
