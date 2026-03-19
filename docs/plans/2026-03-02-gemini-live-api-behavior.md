# Gemini Live API: Behavioral Reference

> Study of the Gemini Live API's wire protocol, session lifecycle, audio/video streaming,
> voice/language configuration, and runtime constraints — derived from official Vertex AI
> documentation and reference notebooks.

**Sources:**
- [Session Management](https://docs.cloud.google.com/vertex-ai/generative-ai/docs/live-api/start-manage-session)
- [Audio & Video Streams](https://docs.cloud.google.com/vertex-ai/generative-ai/docs/live-api/send-audio-video-streams)
- [Language & Voice Config](https://docs.cloud.google.com/vertex-ai/generative-ai/docs/live-api/configure-language-voice)
- [Wire Protocol Notebook](https://github.com/GoogleCloudPlatform/generative-ai/blob/main/gemini/multimodal-live-api/intro_multimodal_live_api.ipynb)

---

## 1. Connection & Authentication

### 1.1 WebSocket Endpoint

```
wss://{LOCATION}-aiplatform.googleapis.com/ws/google.cloud.aiplatform.v1.LlmBidiService/BidiGenerateContent
```

- **Regional**: `us-central1-aiplatform.googleapis.com`, etc.
- **Global**: `aiplatform.googleapis.com` (no location prefix)

> **Note (from wire crate):** Vertex AI uses the global endpoint
> `wss://aiplatform.googleapis.com/...` — NOT `global-aiplatform.googleapis.com`.

### 1.2 Authentication

OAuth 2.0 bearer token in WebSocket upgrade headers:

```
Authorization: Bearer {ACCESS_TOKEN}
Content-Type: application/json
```

- Token lifetime: 3600 seconds
- Obtain via: `gcloud auth application-default print-access-token`

### 1.3 Connection Lifecycle

```
Client                          Server
  │                               │
  ├── WS upgrade ────────────────►│
  │◄──────────────── 101 Switch ──┤
  │                               │
  ├── setup {} ──────────────────►│
  │◄────────────── setupComplete ─┤
  │                               │
  │  ┌── Full-duplex streaming ──┐│
  │  │  realtime_input ──────────►││
  │  │  client_content ──────────►││
  │  │◄──────── serverContent ───┤│
  │  │◄──────── toolCall ────────┤│
  │  │  tool_response ──────────►││
  │  └───────────────────────────┘│
  │                               │
  │◄──────── goAway (60s warn) ──┤
  │                               │
  │◄──── WS close ───────────────┤
```

---

## 2. Session Lifecycle

### 2.1 Duration Limits

| Session Type   | Max Duration  |
|----------------|---------------|
| Audio-only     | ~15 minutes   |
| Audio + Video  | ~2 minutes    |

The server sends a **GoAway** message 60 seconds before forced termination:

```json
{
  "goAway": {
    "timeLeft": "60s"
  }
}
```

### 2.2 Concurrent Session Limits

| Tier                    | Limit     |
|-------------------------|-----------|
| Pay-as-you-go           | 1,000     |
| Provisioned Throughput  | Unlimited |

### 2.3 Context Window

- **Max tokens:** 128,000
- **Token rates:**
  - Audio: ~25 tokens/second
  - Video: ~258 tokens/second (at 1 FPS)
- At 25 tokens/sec, a 15-minute audio session ≈ 22,500 tokens (well within limit)

### 2.4 Context Window Compression

When the context approaches the limit, sliding window compression prunes the oldest turns:

```json
{
  "generation_config": {
    "context_window_compression": {
      "trigger_tokens": 10000,
      "sliding_window": {
        "target_tokens": 512
      }
    }
  }
}
```

| Parameter        | Range          | Description                                    |
|------------------|----------------|------------------------------------------------|
| `trigger_tokens` | 5,000–128,000  | Token count that triggers compression           |
| `target_tokens`  | 0–128,000      | Target token count after oldest turns are pruned |

This enables theoretically unlimited sessions by continuously pruning old context.

### 2.5 Session Resumption

Sessions can be resumed after disconnection within a ~10-minute window (up to 24 hours in some configurations).

**Enable in setup:**
```json
{
  "generation_config": {
    "session_resumption": {
      "handle": null,
      "transparent": true
    }
  }
}
```

**Server sends periodic `SessionResumptionUpdate`:**
```json
{
  "sessionResumptionUpdate": {
    "sessionId": "...",
    "resumable": true,
    "newHandle": "opaque_token"
  }
}
```

**To resume — reconnect and include the handle:**
```json
{
  "setup": {
    "generation_config": {
      "session_resumption": {
        "handle": "opaque_token_from_server",
        "transparent": true
      }
    }
  }
}
```

**Transparent mode** (`"transparent": true`): Server includes the client message index corresponding to the context snapshot, enabling seamless recovery — the client knows exactly which messages the server already has.

---

## 3. Wire Protocol Messages

### 3.1 Client → Server

#### A. Setup (session initialization)

Sent once, immediately after WebSocket handshake. Immutable after send (except system instructions, which can be updated mid-session via `client_content` with `role: "system"`).

```json
{
  "setup": {
    "model": "projects/{PROJECT}/locations/{LOCATION}/publishers/google/models/{MODEL}",
    "system_instruction": {
      "parts": [{ "text": "You are a helpful assistant." }]
    },
    "generation_config": {
      "response_modalities": ["AUDIO"],
      "speech_config": {
        "voice_config": {
          "prebuilt_voice_config": { "voice_name": "Kore" }
        },
        "language_code": "en-US"
      },
      "context_window_compression": { ... },
      "session_resumption": { ... },
      "input_audio_transcription": {},
      "output_audio_transcription": {},
      "enable_affective_dialog": true,
      "media_resolution": "low"
    },
    "tools": {
      "function_declarations": [
        {
          "name": "get_weather",
          "description": "Get weather for a location",
          "parameters": {
            "type": "object",
            "properties": {
              "location": { "type": "string" }
            },
            "required": ["location"]
          }
        }
      ],
      "google_search": {}
    },
    "realtime_input_config": {
      "automatic_activity_detection": {
        "disabled": false,
        "start_of_speech_sensitivity": "START_SENSITIVITY_HIGH",
        "end_of_speech_sensitivity": "END_SENSITIVITY_HIGH",
        "prefix_padding_ms": 20,
        "silence_duration_ms": 100
      }
    },
    "proactivity": {
      "proactive_audio": true
    }
  }
}
```

#### B. Realtime Input (streaming audio/video)

Audio chunks (PCM16, 16kHz, little-endian, base64-encoded):
```json
{
  "realtime_input": {
    "media_chunks": [
      {
        "mime_type": "audio/pcm;rate=16000",
        "data": "<base64_pcm_bytes>"
      }
    ]
  }
}
```

Video frames (JPEG, 768x768, 1 FPS):
```json
{
  "realtime_input": {
    "media_chunks": [
      {
        "mime_type": "image/jpeg",
        "data": "<base64_jpeg_bytes>"
      }
    ]
  }
}
```

Typical audio chunk size: 4096 bytes raw → base64 encoded.

#### C. Client Content (text / discrete turns)

```json
{
  "client_content": {
    "turns": [
      {
        "role": "user",
        "parts": [{ "text": "What is the weather?" }]
      }
    ],
    "turn_complete": true
  }
}
```

- `turn_complete: true` signals the model should respond
- `turn_complete: false` allows multi-part user messages before triggering generation
- `role: "system"` updates the system instruction mid-session

#### D. Tool Response

```json
{
  "tool_response": {
    "function_responses": [
      {
        "name": "get_weather",
        "response": { "temperature": 72, "unit": "F", "condition": "sunny" }
      }
    ]
  }
}
```

### 3.2 Server → Client

#### A. Setup Complete

```json
{
  "setupComplete": {}
}
```

Sent exactly once, confirming session initialization. No other messages should be sent before receiving this.

#### B. Server Content (model output)

```json
{
  "serverContent": {
    "modelTurn": {
      "parts": [
        { "text": "The weather is sunny." },
        {
          "inlineData": {
            "mimeType": "audio/pcm",
            "data": "<base64_24khz_pcm>"
          }
        }
      ]
    },
    "turnComplete": true
  }
}
```

- `modelTurn.parts` contains text and/or audio chunks
- `turnComplete: true` signals the model has finished its response
- Audio arrives as multiple messages before `turnComplete`

**With transcriptions enabled:**
```json
{
  "serverContent": {
    "inputTranscription": {
      "text": "What the user said"
    },
    "outputTranscription": {
      "text": "What the model said"
    },
    "modelTurn": { ... },
    "turnComplete": true
  }
}
```

#### C. Tool Call

```json
{
  "toolCall": {
    "functionCalls": [
      {
        "name": "get_weather",
        "args": { "location": "San Francisco" }
      }
    ]
  }
}
```

Multiple function calls can arrive in a single `toolCall` message.

#### D. Interrupted (barge-in)

```json
{
  "serverContent": {
    "interrupted": true
  }
}
```

Sent when VAD detects user speech during model output. Client **must**:
1. Immediately stop audio playback
2. Flush playback buffers
3. Clear queued audio chunks

The interrupted generation is discarded server-side — only audio already sent to the client is retained in session history.

#### E. GoAway

```json
{
  "goAway": {
    "timeLeft": "60s"
  }
}
```

Warning that the session will terminate in `timeLeft` duration.

#### F. Session Resumption Update

```json
{
  "sessionResumptionUpdate": {
    "sessionId": "...",
    "resumable": true,
    "newHandle": "opaque_token"
  }
}
```

Sent periodically. Client should persist the latest `newHandle` for reconnection.

---

## 4. Audio Specifications

### 4.1 Format Requirements

| Direction | Format               | Sample Rate | Bit Depth | Endianness    |
|-----------|----------------------|-------------|-----------|---------------|
| Input     | Raw PCM (linear16)   | 16,000 Hz   | 16-bit    | Little-endian |
| Output    | Raw PCM (linear16)   | 24,000 Hz   | 16-bit    | Little-endian |

- Input MIME type: `audio/pcm;rate=16000`
- Output MIME type: `audio/pcm` (24kHz implied)
- All binary data transmitted as base64 over JSON/WebSocket

### 4.2 Audio Transcription

Both input and output audio can be transcribed. Enable in setup:

```json
{
  "generation_config": {
    "input_audio_transcription": {},
    "output_audio_transcription": {}
  }
}
```

Transcriptions arrive in `serverContent` alongside (or instead of) model turns:
- `inputTranscription.text` — what the user said
- `outputTranscription.text` — what the model said

### 4.3 Interruption Model

Gemini Live supports **full barge-in**: users can interrupt the model at any point during audio output.

**Server-side behavior:**
1. VAD detects user speech onset during model generation
2. Server immediately stops generating
3. Server sends `serverContent.interrupted = true`
4. All un-sent generation is discarded
5. Only audio already transmitted to client is retained in session history

**Client-side requirements:**
1. Stop audio playback immediately
2. Flush hardware audio buffers
3. Clear any queued but unplayed audio chunks
4. Resume listening for new `serverContent` messages

---

## 5. Video Specifications

### 5.1 Format Requirements

| Parameter  | Value                           |
|------------|---------------------------------|
| Encoding   | JPEG (quality ~90%)             |
| Resolution | 768 x 768 pixels (optimal max) |
| Frame rate | 1 FPS (discrete frames)        |
| MIME type  | `image/jpeg`                    |
| Token cost | ~258 tokens/second              |

### 5.2 Media Resolution

Trade latency/tokens for visual detail:

```json
{
  "generation_config": {
    "media_resolution": "low"
  }
}
```

Values: `"low"`, `"medium"`, `"high"`

### 5.3 Video Streaming Pattern

```
Capture frame → Resize to ≤768x768 → Encode JPEG → Base64 → Send as realtime_input → Wait 1s → Repeat
```

Video is sent as discrete frames, not a continuous stream. Each frame is an independent JPEG.

---

## 6. Voice Activity Detection (VAD)

### 6.1 Configuration

```json
{
  "realtime_input_config": {
    "automatic_activity_detection": {
      "disabled": false,
      "start_of_speech_sensitivity": "START_SENSITIVITY_HIGH",
      "end_of_speech_sensitivity": "END_SENSITIVITY_HIGH",
      "prefix_padding_ms": 20,
      "silence_duration_ms": 100
    }
  }
}
```

### 6.2 Parameters

| Parameter                      | Values / Range      | Default | Description                                      |
|--------------------------------|---------------------|---------|--------------------------------------------------|
| `disabled`                     | `true` / `false`    | `false` | Disable automatic VAD entirely                   |
| `start_of_speech_sensitivity`  | `low` / `high`      | —       | How eagerly speech onset is detected              |
| `end_of_speech_sensitivity`    | `low` / `high`      | —       | How eagerly speech end is detected                |
| `prefix_padding_ms`            | milliseconds        | 20      | Audio retained before detected speech onset       |
| `silence_duration_ms`          | milliseconds        | 100     | Silence duration that signals end-of-speech       |
| `voice_activity_timeout`       | seconds             | —       | Timeout after which session may idle              |

### 6.3 Behavioral Notes

- **Low start sensitivity**: Requires more definitive speech to trigger — reduces false positives from background noise
- **High start sensitivity**: Triggers on quieter/shorter utterances — more responsive but noisier
- **Low end sensitivity**: Waits longer before concluding speech ended — better for pauses within sentences
- **High end sensitivity**: Triggers end-of-speech quickly — faster turn-taking but may cut off hesitations
- **prefix_padding_ms**: Captures the leading edge of speech that occurs before detection triggers (the model hears the start of the word, not just from the detection point)
- When VAD triggers during model output → **barge-in/interruption** (see Section 4.3)

---

## 7. Voice & Language Configuration

### 7.1 Available Voices (30)

| Voice            | Character      | Voice            | Character       |
|------------------|----------------|------------------|-----------------|
| Zephyr           | Bright         | Puck             | Upbeat          |
| Kore             | Firm           | Fenrir           | Excitable       |
| Orus             | Firm           | Aoede            | Breezy          |
| Autonoe          | Bright         | Enceladus        | Breathy         |
| Umbriel          | Easy-going     | Algieba          | Smooth          |
| Erinome          | Clear          | Algenib          | Gravelly        |
| Laomedeia        | Upbeat         | Achernar         | Soft            |
| Schedar          | Even           | Gacrux           | Mature          |
| Achird           | Friendly       | Zubenelgenubi    | Casual          |
| Sadachbia        | Lively         | Sadaltager       | Knowledgeable   |
| Charon           | Informative    | Leda             | Youthful        |
| Callirrhoe       | Easy-going     | Iapetus          | Clear           |
| Despina          | Smooth         | Rasalgethi       | Informative     |
| Alnilam          | Firm           | Pulcherrima      | Forward         |
| Vindemiatrix     | Gentle         | Sulafat          | Warm            |

### 7.2 Supported Languages (24)

| Language            | BCP-47 Code | Language            | BCP-47 Code |
|---------------------|-------------|---------------------|-------------|
| Arabic (Egyptian)   | `ar-EG`     | Marathi (India)     | `mr-IN`     |
| Bengali (Bangladesh)| `bn-BD`     | Polish (Poland)     | `pl-PL`     |
| Dutch (Netherlands) | `nl-NL`     | Portuguese (Brazil) | `pt-BR`     |
| English (India)     | `en-IN`     | Romanian (Romania)  | `ro-RO`     |
| English (US)        | `en-US`     | Russian (Russia)    | `ru-RU`     |
| French (France)     | `fr-FR`     | Spanish (US)        | `es-US`     |
| German (Germany)    | `de-DE`     | Tamil (India)       | `ta-IN`     |
| Hindi (India)       | `hi-IN`     | Telugu (India)      | `te-IN`     |
| Indonesian          | `id-ID`     | Thai (Thailand)     | `th-TH`     |
| Italian (Italy)     | `it-IT`     | Turkish (Turkey)    | `tr-TR`     |
| Japanese (Japan)    | `ja-JP`     | Ukrainian (Ukraine) | `uk-UA`     |
| Korean              | `ko-KR`     | Vietnamese (Vietnam)| `vi-VN`     |

### 7.3 Native vs Non-Native Audio Models

| Feature                  | Native Audio Model                          | Non-Native Audio Model             |
|--------------------------|---------------------------------------------|-------------------------------------|
| Model ID                 | `gemini-genai-2.5-flash-native-audio`        | `gemini-genai-2.5-flash`             |
| Language switching       | Natural mid-conversation switching          | Fixed via `language_code`           |
| Output modality          | AUDIO only (no TEXT output)                 | AUDIO and/or TEXT                   |
| Affective dialog         | Supported (`enable_affective_dialog: true`) | Not available                       |
| Proactive audio          | Supported (`proactive_audio: true`)         | Not available                       |

**Native audio model constraint:** Only supports `response_modalities: ["AUDIO"]` — cannot output text. Transcription is a separate feature that works alongside audio output.

### 7.4 Configuration in Setup

```json
{
  "generation_config": {
    "response_modalities": ["AUDIO"],
    "speech_config": {
      "voice_config": {
        "prebuilt_voice_config": {
          "voice_name": "Kore"
        }
      },
      "language_code": "en-US"
    }
  }
}
```

### 7.5 Special Audio Features

**Affective Dialog** — Enables emotionally-aware voice responses:
```json
{
  "generation_config": {
    "enable_affective_dialog": true
  }
}
```

**Proactive Audio** — Model can speak without being prompted:
```json
{
  "proactivity": {
    "proactive_audio": true
  }
}
```

---

## 8. Tool Calling (Function Calling)

### 8.1 Tool Declaration

Tools are declared in the setup message and are immutable for the session:

```json
{
  "tools": {
    "function_declarations": [
      {
        "name": "get_weather",
        "description": "Get current weather for a location",
        "parameters": {
          "type": "object",
          "properties": {
            "location": { "type": "string", "description": "City name" }
          },
          "required": ["location"]
        }
      }
    ],
    "google_search": {}
  }
}
```

`google_search: {}` enables grounded search alongside custom functions.

### 8.2 Tool Call Flow

```
Server ──► toolCall { functionCalls: [{ name, args }] }
                          │
                    Client executes function
                          │
Client ──► tool_response { function_responses: [{ name, response }] }
                          │
Server ──► serverContent { modelTurn: { parts: [...] }, turnComplete: true }
```

Multiple functions can be called in a single `toolCall` message. The client must respond with all results in a single `tool_response`.

### 8.3 Behavioral Notes

- Tool calls **pause** audio generation — the model stops speaking while waiting for results
- Tool response triggers model to resume generation, incorporating the function output
- There is no explicit timeout on tool responses from the server side, but the session duration limit still applies
- Tool declarations are immutable post-setup; to change tools, start a new session

---

## 9. Session Concurrency Pattern

The standard client implementation uses two concurrent loops over a single WebSocket:

```
┌─────────────────────────────────────┐
│           WebSocket Connection       │
│                                     │
│  ┌──────────┐     ┌──────────────┐ │
│  │ Send Loop │     │ Receive Loop │ │
│  │           │     │              │ │
│  │ Audio mic ──►   │ ◄── Audio    │ │
│  │ Video cam ──►   │ ◄── Text     │ │
│  │ Text      ──►   │ ◄── ToolCall │ │
│  │ ToolResp  ──►   │ ◄── Interrup │ │
│  │           │     │ ◄── GoAway   │ │
│  └──────────┘     └──────────────┘ │
└─────────────────────────────────────┘
```

Both loops run concurrently (`asyncio.gather` / `tokio::join!`). The send loop captures and streams media; the receive loop processes server messages and dispatches to audio playback, tool execution, or state handlers.

---

## 10. Key Constraints & Invariants

### 10.1 Immutability After Setup

These are set once in the `setup` message and cannot be changed:
- Model selection
- Voice configuration
- Tool declarations
- Response modalities
- VAD configuration

**Exception:** System instructions can be updated mid-session by sending `client_content` with `role: "system"`.

### 10.2 Ordering Guarantees

- Server processes `realtime_input` in order received
- `serverContent` messages for a single turn arrive in order
- `turnComplete` is always the last message for a turn
- `setupComplete` must be received before sending any other message
- `interrupted` can arrive at any point during a model turn

### 10.3 Binary Encoding

- All audio/video data is **base64-encoded** in JSON
- Vertex AI sends **Binary WebSocket frames** (not Text) — the client must handle both frame types
- Input audio chunks are typically 4096 bytes raw (before base64 encoding)

### 10.4 Rate & Resource Limits

| Resource                   | Limit                |
|----------------------------|----------------------|
| Concurrent sessions        | 1,000 (PayGo)       |
| Audio-only session         | ~15 minutes          |
| Audio+video session        | ~2 minutes           |
| Context window             | 128,000 tokens       |
| Audio token rate           | ~25 tokens/sec       |
| Video token rate           | ~258 tokens/sec      |
| Video frame rate           | 1 FPS max            |
| Video resolution           | 768x768 max          |
| GoAway warning             | 60 seconds before    |
| Session resume window      | ~10 min (up to 24h)  |
| OAuth token lifetime       | 3600 seconds         |

---

## 11. Implications for gemini-genai-rs Architecture

### 11.1 Wire Crate (L0: gemini-genai-wire)

The wire crate already handles most of the protocol correctly. Key alignment points:

| API Behavior                   | Wire Crate Status          | Notes                                    |
|--------------------------------|----------------------------|------------------------------------------|
| Binary WS frames from Vertex   | Handled                    | `TungsteniteTransport::recv()` handles both |
| Base64 audio encoding          | Handled                    | In `Part::inline_data`                   |
| Setup/SetupComplete handshake  | Handled                    | `connection.rs` waits for setup complete  |
| GoAway message                 | Needs verification         | Ensure `SessionEvent::GoAway` exists      |
| Session resumption             | Not yet implemented        | Need `SessionResumptionUpdate` type       |
| Input/output transcription     | Partially handled          | Verify `inputTranscription` field parsing |
| Interrupted signal             | Handled                    | `SessionEvent::Interrupted`              |
| Tool call/response             | Handled                    | `SessionEvent::ToolCall`, `SessionCommand::ToolResponse` |

### 11.2 Runtime Crate (L1: gemini-adk-rs)

The runtime must enforce session-level constraints:

- **Session timer**: Track elapsed time, surface GoAway to application
- **Interruption handler**: Clear audio pipeline, update transcript state
- **Turn boundary detection**: Use `turnComplete` to trigger the evaluation pipeline
- **Tool execution lifecycle**: Manage concurrent tool calls, enforce session timeout awareness
- **Context budget tracking**: Monitor token accumulation for compression decisions

### 11.3 Fluent Crate (L2: gemini-adk-fluent-rs)

The fluent layer should expose these API behaviors as ergonomic builder options:

```rust
Live::builder()
    .voice("Kore")
    .language("en-US")
    .vad(|v| v
        .start_sensitivity(Sensitivity::High)
        .end_sensitivity(Sensitivity::Low)
        .prefix_padding_ms(20)
        .silence_duration_ms(500))
    .transcribe_input()
    .transcribe_output()
    .affective_dialog()
    .proactive_audio()
    .media_resolution(MediaResolution::Low)
    .context_compression(|c| c
        .trigger_tokens(10_000)
        .target_tokens(512))
    .session_resumption(|r| r
        .transparent(true)
        .on_handle(|handle| persist(handle)))
    .on_interrupted(|ctx| ctx.flush_audio())
    .on_go_away(|ctx, time_left| ctx.graceful_shutdown(time_left))
    .build()
```

---

## 12. Wire Protocol Quick Reference

### Client Messages

| Message Type       | Key Fields                                              | When Sent                    |
|--------------------|---------------------------------------------------------|------------------------------|
| `setup`            | `model`, `generation_config`, `tools`, `system_instruction` | Once, on connect          |
| `realtime_input`   | `media_chunks[{mime_type, data}]`                       | Continuously (audio/video)   |
| `client_content`   | `turns[{role, parts}]`, `turn_complete`                 | Text messages / system updates |
| `tool_response`    | `function_responses[{name, response}]`                  | After executing tool calls   |

### Server Messages

| Message Type                | Key Fields                                          | When Received                |
|-----------------------------|-----------------------------------------------------|------------------------------|
| `setupComplete`             | (empty)                                             | Once, after setup            |
| `serverContent`             | `modelTurn.parts[]`, `turnComplete`, `interrupted`  | During/after model generation |
| `serverContent` (transcript)| `inputTranscription.text`, `outputTranscription.text` | With model turns            |
| `toolCall`                  | `functionCalls[{name, args}]`                       | When model invokes a tool    |
| `goAway`                    | `timeLeft`                                          | 60s before session end       |
| `sessionResumptionUpdate`   | `sessionId`, `resumable`, `newHandle`               | Periodically                 |
