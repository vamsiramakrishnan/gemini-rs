# Voice & Live Sessions

This guide covers everything you need to build voice-enabled agents with
the Gemini Multimodal Live API using gemini-genai-rs.

## What is a Live Session?

A Live Session is a full-duplex WebSocket connection to the Gemini API that
supports simultaneous audio/video input and audio/text output. Unlike the
standard `generateContent` REST API, a Live Session:

- Streams audio bidirectionally (you talk while the model talks)
- Uses server-side VAD (Voice Activity Detection) for turn management
- Supports barge-in (interrupt the model mid-sentence)
- Handles function calling inline with speech
- Maintains conversation context server-side
- Runs for up to ~10 minutes per session (with resumption support)

Audio formats:
- **Input**: PCM16, 16 kHz, mono
- **Output**: PCM16, 24 kHz, mono

## Quick Start

A minimal live session in under 15 lines:

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let handle = Live::builder()
        .model(GeminiModel::Gemini2_0FlashLive)
        .instruction("You are a helpful voice assistant.")
        .on_text(|t| print!("{t}"))
        .on_turn_complete(|| async { println!() })
        .connect_google_ai(std::env::var("GEMINI_API_KEY")?)
        .await?;

    handle.send_text("What is the capital of France?").await?;
    handle.done().await?;
    Ok(())
}
```

This connects to Gemini, sends a text message, prints the streamed response,
and waits for the session to end. For audio, replace `send_text()` with
`send_audio()` and add an `on_audio` callback.

## The Live Builder

`Live::builder()` returns a chainable builder that configures the entire
session. Here is the full chain with all major options:

```rust,ignore
let handle = Live::builder()
    // Model and voice
    .model(GeminiModel::Gemini2_5FlashNativeAudio)
    .voice(Voice::Kore)
    .temperature(0.7)
    .instruction("You are a restaurant order assistant.")

    // Tools (auto-dispatched when model calls them)
    .tools(dispatcher)

    // Audio/transcription config
    .transcription(true, true)   // input, output
    .affective_dialog(true)      // emotionally expressive responses

    // Server-side VAD
    .vad(AutomaticActivityDetection::default())

    // Session lifecycle
    .session_resume(true)
    .context_compression(4000, 2000)  // trigger_tokens, target_tokens

    // Greeting (model speaks first)
    .greeting("Greet the customer and ask what they'd like to order.")

    // Fast-lane callbacks (sync, <1ms)
    .on_audio(|data| { /* forward to speaker */ })
    .on_text(|t| print!("{t}"))
    .on_input_transcript(|text, _is_final| { /* display what user said */ })
    .on_output_transcript(|text, _is_final| { /* display what model said */ })
    .on_vad_start(|| { /* user started speaking */ })
    .on_vad_end(|| { /* user stopped speaking */ })

    // Telemetry callback (sync, telemetry lane)
    .on_usage(|usage| {
        if let Some(total) = usage.total_token_count {
            println!("Tokens: {total}");
        }
    })

    // Control-lane callbacks (async)
    .on_tool_call(|calls, state| async move { None })  // None = auto-dispatch
    .on_interrupted(|| async { /* flush playback buffer */ })
    .on_turn_complete(|| async { /* turn finished */ })
    .on_connected(|writer| async move { /* session ready */ })
    .on_disconnected(|reason| async move { /* session ended */ })
    .on_error(|msg| async move { eprintln!("Error: {msg}") })

    // Connect
    .connect_vertex("my-project", "us-central1", access_token)
    .await?;
```

The builder validates configuration at connect time, not at each method call.
All methods are optional except the connect method.

## Callbacks

Callbacks are split into two categories based on latency requirements.

### Fast Lane (Sync)

These fire on the fast lane and must complete in under 1ms. They receive
references (not owned values) and cannot be `async`.

```rust,ignore
// Audio: receives zero-copy Bytes (cloning bumps an Arc refcount, not data)
.on_audio(|data: &Bytes| {
    playback_tx.send(data.clone()).ok();
})

// Text: incremental deltas as the model generates
.on_text(|text: &str| {
    print!("{text}");
})

// Text complete: full text when model finishes a text response
.on_text_complete(|text: &str| {
    println!("\nComplete: {text}");
})

// Transcription: text version of audio (input or output)
// Second parameter is `is_final` (true when transcription is finalized)
.on_input_transcript(|text: &str, is_final: bool| {
    if is_final { println!("User said: {text}"); }
})

// VAD: voice activity detection events from the server
.on_vad_start(|| { /* user started talking */ })
.on_vad_end(|| { /* user stopped talking */ })

// Usage metadata: token counts from the server (fires on telemetry lane)
.on_usage(|usage: &UsageMetadata| {
    if let Some(total) = usage.total_token_count {
        println!("Total tokens: {total}");
    }
    // Also available: prompt_token_count, response_token_count,
    // cached_content_token_count, thoughts_token_count,
    // tool_use_prompt_token_count, plus per-modality breakdowns
})
```

### Control Lane (Async)

These fire on the control lane and can perform I/O, state access, or any
async work. They block the control lane while running (other control events
queue behind them).

```rust,ignore
// Tool calls: return None for auto-dispatch, Some for manual responses
.on_tool_call(|calls: Vec<FunctionCall>, state: State| async move {
    // Read state if needed
    let user_id: Option<String> = state.get("user_id");
    // Return None to let the ToolDispatcher handle it
    None
})

// Interrupted: model was interrupted by barge-in
.on_interrupted(|| async {
    playback_buffer.flush().await;
})

// Turn complete: model finished its response
.on_turn_complete(|| async {
    println!("--- turn complete ---");
})

// Connected: session is ready (receives SessionWriter for advanced use)
.on_connected(|writer: Arc<dyn SessionWriter>| async move {
    println!("Session connected");
})

// Disconnected: session ended (receives optional reason string)
.on_disconnected(|reason: Option<String>| async move {
    println!("Disconnected: {reason:?}");
})

// Error: non-fatal error (session continues)
.on_error(|msg: String| async move {
    eprintln!("Error: {msg}");
})
```

### Callback Execution Modes

Control-lane callbacks support two execution modes via `CallbackMode`:

**Blocking (default)** — the event loop waits for the callback to complete.
Use when subsequent events depend on the callback's side effects, or when
ordering guarantees are required.

**Concurrent** — the callback is spawned as a detached tokio task. The event
loop continues immediately. Use for fire-and-forget work: logging, analytics,
webhook dispatch, or background agent triggering.

Use `_concurrent` suffixed methods to opt in:

```rust,ignore
Live::builder()
    // Blocking (default) — client depends on TurnComplete ordering
    .on_turn_complete(|| async { tx.send(TurnComplete).ok(); })

    // Concurrent — fire-and-forget broadcast, doesn't block the pipeline
    .on_extracted_concurrent(|name, val| async move {
        tx.send(StateUpdate { key: name, value: val }).ok();
    })
    .on_error_concurrent(|e| async move {
        webhook::send_alert(&e).await;
    })
    .on_disconnected_concurrent(|reason| async move {
        info!("Disconnected: {reason:?}");
    })
```

**Forced-blocking callbacks** (no concurrent variant):

| Callback | Reason |
|----------|--------|
| `on_interrupted` | Must clear interrupted flag before audio resumes |
| `on_tool_call` | Return value is the tool response |
| `before_tool_response` | Transforms data in the pipeline |
| `on_turn_boundary` | Content injection must complete before turn_complete |

### Tool Dispatch

When the model calls a tool, the dispatch logic follows this priority:

1. If `on_tool_call` is registered and returns `Some(responses)` -- use
   those responses.
2. If `on_tool_call` returns `None` (or is not registered) and a
   `ToolDispatcher` is set -- auto-dispatch to the registered tool, send
   the result back to the model automatically.
3. If neither -- log a warning and skip.

Register tools with the dispatcher:

```rust,ignore
use gemini_adk_rs::{SimpleTool, ToolDispatcher};

let mut dispatcher = ToolDispatcher::new();
dispatcher.register(SimpleTool::new(
    "get_weather",
    "Get current weather for a city",
    |args| async move {
        let city = args["city"].as_str().unwrap_or("unknown");
        Ok(serde_json::json!({ "city": city, "temp_c": 22, "condition": "sunny" }))
    },
));

let handle = Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .instruction("You are a weather assistant. Use get_weather to answer questions.")
    .tools(dispatcher)
    .on_text(|t| print!("{t}"))
    .connect_google_ai(api_key)
    .await?;
```

## Audio Pipeline

The audio pipeline for a typical voice agent:

```
Microphone (PCM16 16kHz)
    |
    v
handle.send_audio(bytes)       --- outbound --->  Gemini Live API
                                                       |
                                                  Server-side VAD
                                                  Model inference
                                                       |
on_audio(|data: &Bytes|)       <--- inbound ---   Audio response
    |                                             (PCM16 24kHz)
    v
Speaker / Playback buffer
```

Key points:

- **Input format**: PCM16, 16 kHz, mono. Send raw bytes, not base64.
  The SDK handles base64 encoding on the wire.
- **Output format**: PCM16, 24 kHz, mono. The `on_audio` callback receives
  decoded bytes ready for playback.
- **Buffer sizes**: Audio arrives in variable-size chunks. Use an
  `AudioJitterBuffer` (from L0) if you need smooth playback.
- **Barge-in**: When the user speaks while the model is responding, the
  server sends an `Interrupted` event. The fast lane sets the interrupted
  flag and stops forwarding audio; the control lane fires `on_interrupted`.

## Greeting

Use `.greeting()` to make the model speak first without waiting for user
input. The greeting prompt is sent immediately after the WebSocket setup
completes.

```rust,ignore
let handle = Live::builder()
    .model(GeminiModel::Gemini2_5FlashNativeAudio)
    .voice(Voice::Kore)
    .instruction("You are a receptionist at a dental clinic.")
    .greeting("Greet the caller and ask how you can help them today.")
    .on_audio(|data| { playback_tx.send(data.clone()).ok(); })
    .connect_vertex(project, location, token)
    .await?;
// Model will immediately start speaking a greeting
```

The greeting text is sent as a user-role `client_content` message with
`turn_complete: true`, which triggers the model to generate a response.

## Transcription

Enable text transcription of audio streams to get text versions of what
the user said and what the model said:

```rust,ignore
let handle = Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .transcription(true, true)  // input, output
    .on_input_transcript(|text, is_final| {
        if is_final {
            println!("User: {text}");
        }
    })
    .on_output_transcript(|text, is_final| {
        if is_final {
            println!("Model: {text}");
        }
    })
    .connect_google_ai(api_key)
    .await?;
```

Transcription is required for turn extraction (`.extract_turns()`) to work.
When you add an extractor, transcription is enabled automatically.

## Session Lifecycle

A session progresses through these phases:

```
Disconnected --> Connecting --> SetupSent --> Active --> Disconnected
                                               |
                                               +--> GoAway (60s warning)
                                               +--> Interrupted (barge-in)
```

| Phase | Description |
|-------|-------------|
| `Disconnected` | Initial state, or after clean/unclean disconnect |
| `Connecting` | WebSocket handshake in progress |
| `SetupSent` | Setup message sent, waiting for `setupComplete` |
| `Active` | Session is live, audio/text flowing |

The `GoAway` event signals the server will disconnect in ~60 seconds.
Save state and prepare to reconnect. With `.session_resume(true)`, you
receive a `SessionResumeHandle` that can be used to continue the
conversation in a new session.

### Interacting with a Running Session

The `LiveHandle` returned by `.connect_*()` provides the runtime API:

```rust,ignore
// Send audio (raw PCM16 16kHz bytes)
handle.send_audio(pcm_bytes).await?;

// Send text
handle.send_text("What's the weather?").await?;

// Send video frame (raw JPEG bytes)
handle.send_video(jpeg_bytes).await?;

// Update system instruction mid-session
handle.update_instruction("Now focus on dessert orders.").await?;

// Read state (populated by extractors)
let order: Option<OrderState> = handle.extracted("OrderState");

// Access telemetry
let snapshot = handle.telemetry().snapshot();

// Get current session phase
let phase = handle.phase();

// Subscribe to raw events (for custom processing)
let mut events = handle.subscribe();

// Graceful disconnect
handle.disconnect().await?;

// Wait for session to end naturally
handle.done().await?;
```

## Vertex AI vs Google AI

The SDK supports both Google AI (API key) and Vertex AI (OAuth2 token)
backends. The wire protocol is the same; only the endpoint URL and
authentication differ.

### Google AI

```rust,ignore
let handle = Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .connect_google_ai("YOUR_API_KEY")
    .await?;
```

- Endpoint: `wss://generativelanguage.googleapis.com/v1beta/models/{model}`
- Auth: API key in query parameter

### Vertex AI

```rust,ignore
// Get token via gcloud: gcloud auth print-access-token
let token = std::env::var("GCLOUD_ACCESS_TOKEN")?;

let handle = Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .connect_vertex("my-gcp-project", "us-central1", token)
    .await?;
```

- Endpoint: `wss://aiplatform.googleapis.com/v1beta1/projects/{project}/locations/{location}/publishers/google/models/{model}`
- Auth: Bearer token in WebSocket upgrade headers
- Note: Uses the global endpoint (`aiplatform.googleapis.com`), not `global-aiplatform.googleapis.com`

### Pre-configured SessionConfig

For advanced scenarios (custom auth, proxy, etc.), build the config yourself
and pass it to `.connect()`:

```rust,ignore
use gemini_genai_rs::prelude::*;

let config = SessionConfig::from_endpoint(
    ApiEndpoint::vertex("my-project", "us-central1", token)
)
    .model(GeminiModel::Gemini2_5FlashNativeAudio)
    .voice(Voice::Kore)
    .response_modalities(vec![Modality::Audio])
    .system_instruction("You are a helpful assistant.")
    .enable_input_transcription()
    .enable_output_transcription();

let handle = Live::builder()
    .on_audio(|data| { /* play audio */ })
    .connect(config)
    .await?;
```

When using `.connect(config)`, model/voice/instruction settings on the
`SessionConfig` take precedence. The `.model()` / `.voice()` / `.instruction()`
methods on the Live builder configure the same underlying `SessionConfig`, so
you can use either approach -- just not both for the same setting.

### Key Differences

| Feature | Google AI | Vertex AI |
|---------|-----------|-----------|
| Auth | API key (string) | OAuth2 Bearer token |
| API version | `v1beta` | `v1beta1` |
| Frame format | Text WebSocket frames | Binary WebSocket frames |
| Billing | Per-token pricing | GCP billing account |
| Region | Global | Regional (e.g., `us-central1`) |
