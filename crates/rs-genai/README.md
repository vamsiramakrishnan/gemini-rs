# rs-genai

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
use rs_genai::prelude::*;

let config = TransportConfig::google_ai("YOUR_API_KEY", GeminiModel::Gemini2_0Flash);
let (handle, events) = connect(config).await?;

handle.send_text("Hello!").await?;
while let Some(event) = events.recv().await {
    // Handle server events
}
```

## Documentation

[API Reference (docs.rs)](https://docs.rs/rs-genai)

## License

Apache-2.0
