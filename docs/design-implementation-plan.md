# gemini-live-rs — Design Document

**Ultra-Low-Latency, High-Concurrency Rust Library for the Gemini Multimodal Live API**

Version 0.1 · March 2026

-----

## 1. Executive Summary

`gemini-live-rs` is a systems-grade Rust library purpose-built for the Gemini Multimodal Live API. It delivers what Pipecat and LiveKit cannot: **true real-time performance without the Python GIL ceiling**, **zero-copy audio pipelines**, **lock-free concurrency**, and **native integration with Google’s agent ecosystem (ADK, A2A, MCP)**.

Where Pipecat and LiveKit are orchestration frameworks that bolt together external STT/LLM/TTS services, `gemini-live-rs` treats the Gemini Live API as a **first-class, unified speech-to-speech system** — eliminating the cascaded pipeline entirely and operating at the wire level where milliseconds are reclaimed.

### Why This Exists

The Gemini Multimodal Live API is fundamentally different from the STT→LLM→TTS pipeline model. It offers native bidirectional audio streaming, server-side VAD, integrated function calling, and speech-to-speech with emotional understanding — all over a single WebSocket. Yet every existing framework forces it through abstractions designed for cascaded pipelines, losing the very advantages that make it exceptional.

This library removes that impedance mismatch.

-----

## 2. Competitive Analysis — Grounded in Real Production Issues

This analysis is derived from a systematic review of open and closed issues on the Pipecat (github.com/pipecat-ai/pipecat) and LiveKit Agents (github.com/livekit/agents) issue trackers. Every claim below references at least one real user-reported issue. This is not a straw-man comparison — both frameworks are good at what they do. The question is whether their architecture can deliver reliable sub-200ms latency for Gemini Live API workloads specifically.

### 2.1 Pipecat: Where the Pipeline Abstraction Breaks Down

Pipecat is a Python framework by Daily.co for orchestrating cascaded voice pipelines (STT → LLM → TTS). It excels at rapid prototyping and has broad provider support. Its limitations for our use case are architectural, not incidental.

**Gap 1: No Jitter Buffer — Audio Quality Degrades Under Real Network Conditions**

Issue #3222 is a user explicitly asking whether Pipecat has any jitter buffering for WebSocket audio output. Their SIP provider reports chunks arriving with 600ms+ inter-packet variance. Pipecat’s `AudioBufferProcessor` handles recording, not jitter compensation. The `audio_out_10ms_chunks` parameter controls send granularity, not receive smoothing. On Twilio, users report choppy/distorted phone audio while server-side recordings are clean (#2551), confirming the problem is in output delivery, not generation.

*Why this matters for gemini-live-rs*: The Gemini Live API streams audio over WebSocket. Network jitter is unavoidable. Without an adaptive jitter buffer on the receive path, audio playback will stutter. We implement a dedicated jitter buffer with EWMA-based depth adaptation and instant flush on barge-in — a component Pipecat does not have.

**Gap 2: Interruption Handling is Broken Across Multiple Transports**

This is the most consistently reported class of issues, spanning years and transport types:

- FastAPI WebSocket: user-speaking events are not detected; workaround adds 3-5 seconds of latency (#2460)
- Twilio: bot keeps speaking despite user interruption (#3191)
- Context loss: partial bot speech not saved to LLM context after interruption, causing the bot to repeat itself from the beginning (#2791)
- Self-interruption: on speaker mode, bot hears its own audio output and interrupts itself (#188)
- Context corruption: with `allow_interruptions=True`, assistant messages stop being appended to context entirely (#1591)
- No dynamic control: once the pipeline starts, you cannot toggle interruption behavior at runtime — a requirement for use cases like reading terms & conditions (#2153)
- Gemini-specific: interrupting a Gemini Live agent causes it to stop responding entirely (#1661)

The root cause is architectural: Pipecat’s frame pipeline propagates interruptions as `CancelFrame` objects that travel linearly through the pipeline. Flushing state across STT → LLM → TTS → Transport boundaries — each operating on different frames at different times — creates race conditions. When the Gemini Live API (which handles VAD server-side) sends an interrupt signal, it conflicts with the pipeline’s own interruption model.

*Why this matters for gemini-live-rs*: We use the Gemini server’s VAD signals as the authoritative interrupt source. Barge-in is a single atomic operation: flush the jitter buffer, send `activityStart`, and transition the session FSM. There is no pipeline of in-flight frames to cancel.

**Gap 3: Pipeline Lifecycle Is Dangerous in Production**

Multiple issues document that stopping a Pipecat pipeline is unreliable:

- `EndFrame` not propagated to sink, causing asyncio task accumulation and memory leak (#953)
- Memory climbs indefinitely after pipeline cancellation until crash (#1809)
- Pipeline stuck when client disconnects mid-speech — `CancelFrame` blocked, 1200-second timeout hit (#3179)
- `AudioBufferProcessor` blocks shutdown; removing it fixes the issue (#2609)
- Crash when user hangup coincides with internally queued `EndFrame` (#2218)
- Audio mixer causes `BaseOutputTransport` to loop infinitely generating silence, causing OOM (#1338)
- Concurrent pipelines via `ThreadPoolExecutor` deadlock — only `SIGKILL` terminates the process (#1912)

Issue #976 captures this pattern most honestly: “I have used Pipecat for several months, stability is always a problem.” The user reports Gemini error 1011, ElevenLabs error 1008/1009, and Cartesia `NoneType` errors all appearing after ~10 minutes of production use.

*Why this matters for gemini-live-rs*: Our actor-per-session model uses independent Tokio task constellations. When a session ends, all its tasks are cancelled and their memory is reclaimed immediately. There is no shared mutable state between sessions, no global pipeline that can block, and no frame propagation chain that can deadlock.

**Gap 4: Gemini Live Integration Is Incomplete**

Despite having a `GeminiMultimodalLiveLLMService`, multiple Gemini-native features are inaccessible:

- VAD configuration, session resumption, context window compression, and media resolution controls are not exposed (#1606)
- Session keepalive / `sessionResumption` errors when configured (#1674)
- Function calling results silently dropped — missing `name` field in context aggregator prevents Gemini from using tool results (#908)
- `UserIdleProcessor` race condition: LLM messages pushed but bot remains silent for multiple retries (#3149)
- Twilio→Gemini audio path broken: mulaw 8kHz to PCM 16kHz conversion not handled (#1823)
- Vertex AI regional endpoints not supported, blocking GDPR compliance (#1783)

*Why this matters for gemini-live-rs*: We implement the Gemini Live wire protocol directly with one-to-one type mappings. Every API feature (VAD params, session resumption, GoAway, activity signals, async function calling) is a first-class Rust type with compile-time validation.

**Gap 5: Context Management Is Manual and Error-Prone**

Issue #147 (from 2024, still open) states: “Pipecat bots don’t automatically maintain an LLM context object for you; instead, you have to use a bunch of aggregators and other tools to manage that yourself.” Downstream effects include context frames being captured rather than passed through (#1322), and Mem0 memory storage blocking the conversation flow synchronously (#1741).

### 2.2 LiveKit Agents: Where the SFU Abstraction Fights Direct API

LiveKit is a Go-based SFU with a Python/Node.js agent framework. It has the strongest WebRTC infrastructure of any voice AI platform and excellent multi-participant support. Its limitations emerge specifically when used with direct-API models like Gemini Live.

**Gap 1: The Agent Silence Problem — State Machine Desynchronization**

Issue #3418 is the most consequential bug in LiveKit’s voice agent system: after an interruption, the agent enters a state where it is marked as “speaking” but no audio is being generated. The agent remains stuck indefinitely until the user speaks again. The framework provides no event or error to indicate the TTS stream failed. This happens more frequently in live phone calls than in testing — exactly the environment where reliability matters most.

The root cause: interruption handling, turn detection, and TTS orchestration operate as loosely coupled components. When an interruption fires during a narrow window of the agent’s state transition (thinking → speaking), the components desynchronize. The state machine says “speaking,” but the TTS pipeline has been cancelled.

*Why this matters for gemini-live-rs*: Our session FSM enforces validated state transitions. Every transition must pass through `can_transition_to()` — the system cannot enter a state like “speaking with no audio source” because the FSM transition is gated on the audio pipeline being active. Phase observers (via `watch` channels) detect and surface stuck states.

**Gap 2: Turn Detection Is Fundamentally Unsolved**

This is LiveKit’s most actively discussed problem area:

- Text-only turn detection misses prosody, pitch, and intonation cues that humans use (#3094) — feature request received 57+ thumbs-up reactions
- Turn detection is too sensitive: interruption logic is shared across agent thinking and speaking states, so you cannot tune sensitivity independently (#3427) — “not possible to make the agent hard to interrupt but still friendly to slow speakers”
- Phone numbers and email addresses trigger premature end-of-turn (#3701) — 700-800ms delay still cuts users; reducing delay makes it worse
- `min_endpointing_delay` behaves differently in VAD vs STT modes (#4325) — VAD uses `max()` semantics while STT uses additive delay, confusing developers
- Leading audio dropped on barge-in (#3261) — first word/phoneme lost during interruptions
- Resume-false-interruption regression (#4039) — feature that worked in 1.2.18 broke in 1.3.3, causing “mhmm” and backchannel signals to interrupt the agent

*Why this matters for gemini-live-rs*: For Gemini Live specifically, turn detection is solved server-side. The Gemini model has full access to audio features (prosody, energy, context) and makes its own end-of-turn decisions. We relay these decisions as authoritative. Our client-side VAD exists only for instant local barge-in feedback — it does not make turn-taking decisions.

**Gap 3: Vertex AI / Gemini Realtime Integration Has Fundamental Conflicts**

- Spurious server VAD events from Vertex AI cancel running tools (#4441) — the agent framework calls `self.interrupt()` on every `InputSpeechStarted` signal, and you *cannot* set `allow_interruptions=False` when server turn detection is enabled. Critical tools like database queries are killed by false-positive VAD.
- `update_chat_ctx()` silently strips system messages for realtime models (#4497) — `remove_instructions()` is called before forwarding context, making proactive state injection impossible (“the user just logged in, here is their name”)
- Native audio model not in allowed model list (#3747) — hard-coded `LiveAPIModels` enum is out of date
- Duplicate responses to text input (#3870) — agent starts responding, then generates a second duplicate response
- Agent literally speaks “tools_output” (#2174) — deprecated `session.send()` API serializes protocol messages as speech text
- Empty Gemini responses processed as success (#4066) — `finish_reason=STOP` with empty content produces silent turn
- WebSocket connection bombardment (#1679) — no rate limiting on reconnection attempts, causing `OVERLOADED_TOO_MANY_RETRIES` errors

*Why this matters for gemini-live-rs*: These are consequences of wrapping a direct-API model in an SFU abstraction designed for separate STT/LLM/TTS providers. Our library speaks the Gemini Live wire protocol natively. There is no abstraction layer to conflict with the API’s own session management, VAD, or tool calling semantics.

**Gap 4: Observability Is Acknowledged as Missing**

Issue #2260 — requesting native OpenTelemetry instrumentation — received 57 thumbs-up and 10 heart reactions, making it one of the most-demanded features. Related issues include:

- Metrics missing model/vendor names, preventing attribution (#2033)
- STT metrics have no speech_id, so turn-level correlation is impossible (#4054)
- Latency measurement formula doesn’t match real measurements (#3824, #2522)
- Pipeline-level metrics were removed in newer versions with no replacement (#2522)
- Enabling recording/observability degrades voice quality to 2x slower and distorted (#4055) — the observability system itself causes production failures

*Why this matters for gemini-live-rs*: OTel spans, structured logging, and Prometheus metrics are built into the architecture from day one, feature-gated to add zero overhead when disabled. Observability does not contend with the audio path because they run on separate Tokio tasks.

**Gap 5: Session Lifecycle and Recovery**

- Auto-reconnect for OpenAI realtime doesn’t always fire (#3145) — production-only failure, 30-minute session expiry silently kills agent
- SIP participant migration vs real hangup indistinguishable (#4705) — disconnect_reason is null
- Participant not unlinked on disconnect (#1124) — old participant state persists, new participant’s VAD/STT not connected
- ElevenLabs TTS retry doesn’t reset state (#4135) — second connection sends nothing, eventually marked unrecoverable
- Worker process killed before disconnect callbacks finish (#322)

### 2.3 What We Are NOT Better At (Honest Assessment)

Intellectual honesty requires acknowledging where Pipecat and LiveKit have genuine advantages:

|Area                       |Advantage                                                                                      |Our Mitigation                                                                     |
|---------------------------|-----------------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------|
|**Provider breadth**       |Pipecat supports 20+ STT/TTS/LLM providers out of the box. LiveKit has a rich plugin ecosystem.|We are Gemini-only by design. For multi-provider needs, use Pipecat or LiveKit.    |
|**Multi-participant**      |LiveKit’s SFU is purpose-built for many-to-many audio routing.                                 |We handle one-to-one sessions. Multi-participant requires an external mixer or SFU.|
|**WebRTC**                 |LiveKit has world-class WebRTC support with SRTP, DTLS, ICE, simulcast.                        |We don’t do WebRTC. If your transport *must* be WebRTC, use LiveKit.               |
|**Ecosystem maturity**     |Both have years of production deployments, community, examples.                                |We are new.                                                                        |
|**Ease of getting started**|Python + 10 lines of code to a working demo.                                                   |Rust has a steeper learning curve; we mitigate with a high-level `connect()` API.  |
|**Client SDKs**            |LiveKit has native iOS, Android, Web, Flutter, Unity SDKs.                                     |We have Rust only (WASM and C FFI are roadmap items).                              |

### 2.4 Comparison Table

This table compares specifically for the Gemini Live API use case, not general-purpose voice AI.

|Dimension                   |Pipecat                                            |LiveKit                                           |**gemini-live-rs**                                     |
|----------------------------|---------------------------------------------------|--------------------------------------------------|-------------------------------------------------------|
|**Language**                |Python                                             |Go SFU + Python/Node agents                       |**Rust**                                               |
|**Concurrency Model**       |async Python + GIL                                 |Go goroutines (SFU) + Python GIL (agents)         |**Tokio async + lock-free data structures**            |
|**Audio Pipeline**          |Frame objects with heap alloc                      |WebRTC media tracks                               |**Lock-free SPSC ring buffers, zero-copy**             |
|**Jitter Buffer**           |❌ None (#3222)                                     |✅ (WebRTC built-in)                               |**✅ Adaptive EWMA with instant barge-in flush**        |
|**Gemini Live API**         |Partial — many features unexposed (#1606)          |Partial — conflicts with agent abstraction (#4441)|**First-class: every wire type is a Rust type**        |
|**VAD**                     |Silero (Python inference)                          |Silero + custom transformer                       |**Client energy+ZCR + server-side signal relay**       |
|**Transport to Gemini**     |WS through pipeline framework                      |WebRTC → SFU → WS                                 |**Direct WSS**                                         |
|**Interruption Reliability**|Broken across transports (#2460, #3191, #1661)     |Silent-agent desync after barge-in (#3418)        |**Atomic: flush buffer + send signal + FSM transition**|
|**Turn Detection**          |Silence-based (Gemini VAD partly exposed)          |Text-based + VAD (no prosody — #3094)             |**Defer to Gemini server VAD (has full audio context)**|
|**Pipeline Shutdown**       |Deadlocks, leaks, stuck frames (#953, #1809, #3179)|Worker killed before cleanup (#322)               |**Drop session → all tasks cancelled immediately**     |
|**Function Calling**        |Tool results silently dropped (#908)               |Agent speaks “tools_output” literally (#2174)     |**Native wire format, compile-time schema validation** |
|**Session Resume**          |Not supported (#1674)                              |Auto-reconnect unreliable (#3145)                 |**First-class resume handle with GoAway awareness**    |
|**Observability**           |Loguru (no OTel, no metrics)                       |No OTel (57+ upvotes requesting it — #2260)       |**Built-in OTel spans + structured logs + Prometheus** |
|**Context Management**      |Manual aggregators, error-prone (#147, #2791)      |System messages silently stripped (#4497)         |**Gemini-managed context + explicit FSM tracking**     |
|**Production Stability**    |“Stability is always a problem” (#976)             |Recording degrades voice quality (#4055)          |**Actor isolation — one session crash ≠ process crash**|
|**Provider Breadth**        |✅ 20+ STT/TTS/LLM providers                        |✅ Rich plugin ecosystem                           |**❌ Gemini-only (by design)**                          |
|**Multi-participant**       |Degrades under load                                |✅ Purpose-built SFU                               |**❌ 1:1 sessions only**                                |
|**Client SDKs**             |❌ Server only                                      |✅ iOS, Android, Web, Flutter                      |**❌ Rust only (WASM/FFI planned)**                     |
|**Ease of Starting**        |✅ Python + 10 lines                                |✅ Python + simple config                          |**⚠️ Rust learning curve**                              |
|**Memory per Session**      |~50-100MB (Python runtime)                         |~30-60MB (SFU + agent)                            |**~2-5MB**                                             |
|**Deployment Targets**      |Server only                                        |Server + client SDKs                              |**Server, edge, embedded, WASM (future)**              |

-----

## 3. Architecture

### 3.1 System Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                        Application Layer                            │
│  ┌──────────┐  ┌──────────────┐  ┌──────────┐  ┌───────────────┐  │
│  │ ADK      │  │ A2A Dispatch │  │ Function │  │ Custom Agent  │  │
│  │ Bridge   │  │ Client       │  │ Registry │  │ Logic         │  │
│  └────┬─────┘  └──────┬───────┘  └────┬─────┘  └──────┬────────┘  │
│       └───────────────┬┼──────────────┘               │            │
│                       ││                               │            │
│  ┌────────────────────┴┴───────────────────────────────┴────────┐  │
│  │                    Session Manager                            │  │
│  │  ┌─────────────┐  ┌──────────────┐  ┌────────────────────┐  │  │
│  │  │ State FSM   │  │ Turn Tracker │  │ Tool Call Dispatch │  │  │
│  │  └─────────────┘  └──────────────┘  └────────────────────┘  │  │
│  └──────────────────────────┬───────────────────────────────────┘  │
│                              │                                      │
│  ┌───────────────────────────┴──────────────────────────────────┐  │
│  │                    Transport Layer                            │  │
│  │  ┌──────────┐  ┌──────────────┐  ┌─────────────────────┐    │  │
│  │  │ WS       │  │ Send/Recv    │  │ Flow Control        │    │  │
│  │  │ Manager  │  │ Split Tasks  │  │ (TokenBucket+Pacing)│    │  │
│  │  └──────────┘  └──────────────┘  └─────────────────────┘    │  │
│  └──────────────────────────┬───────────────────────────────────┘  │
│                              │                                      │
│  ┌───────────────────────────┴──────────────────────────────────┐  │
│  │                    Audio Engine                               │  │
│  │  ┌──────────┐  ┌──────────────┐  ┌────────────────────────┐ │  │
│  │  │ SPSC     │  │ Jitter       │  │ Client VAD             │ │  │
│  │  │ Ring Buf │  │ Buffer       │  │ (Energy+ZCR+Adaptive)  │ │  │
│  │  └──────────┘  └──────────────┘  └────────────────────────┘ │  │
│  └──────────────────────────────────────────────────────────────┘  │
│                                                                     │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │                    Observability Layer                        │  │
│  │  ┌──────────┐  ┌──────────────┐  ┌────────────────────────┐ │  │
│  │  │ OTel     │  │ Structured   │  │ Prometheus Metrics     │ │  │
│  │  │ Tracing  │  │ Logging      │  │ (latency, jitter, etc) │ │  │
│  │  └──────────┘  └──────────────┘  └────────────────────────┘ │  │
│  └──────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
                              │
                              │ WSS (direct, no middleman)
                              ▼
            ┌──────────────────────────────────────┐
            │  Gemini Multimodal Live API           │
            │  wss://generativelanguage.googleapis  │
            │  .com/ws/...BidiGenerateContent       │
            └──────────────────────────────────────┘
```

### 3.2 Core Design Principles

**1. Zero-Copy Hot Path**: Audio data moves through lock-free SPSC ring buffers. No heap allocation on the audio streaming path. Base64 encoding happens in-place on pre-allocated buffers.

**2. Actor-Per-Session**: Each live session runs as an independent Tokio task constellation (send task, receive task, VAD task, flow control task). Sessions share nothing — no global locks, no contention.

**3. Validated State Transitions**: The session lifecycle is an explicit finite state machine (`Disconnected → Connecting → SetupSent → Active ⇄ ModelSpeaking / UserSpeaking → ...`). Invalid transitions are compile-time errors where possible, runtime errors otherwise.

**4. Fail Partial, Not Total**: A WebSocket disconnect doesn’t destroy session state. Turn history, pending tool calls, and conversation context survive reconnection. The session resume handle from Gemini is used for transparent reconnection.

**5. Observability Is Not Optional**: Every layer emits OpenTelemetry spans, structured log events, and Prometheus metrics. Disabled at compile time via feature flags for zero overhead in production.

### 3.3 Module Architecture

```
gemini-live-rs/
├── src/
│   ├── lib.rs                  # Public API surface
│   ├── protocol/               # Wire-format types
│   │   ├── mod.rs              # Shared types (Content, Part, Blob, etc.)
│   │   ├── messages.rs         # Client→Server and Server→Client envelopes
│   │   └── types.rs            # SessionConfig, Voice, AudioFormat, GeminiModel
│   ├── transport/              # WebSocket lifecycle
│   │   ├── connection.rs       # Connect, setup, full-duplex split, reconnection
│   │   └── flow.rs             # Token bucket, congestion window, send pacing
│   ├── buffer/                 # Lock-free audio buffers
│   │   ├── mod.rs              # SPSC ring buffer + chunked reader
│   │   └── jitter.rs           # Adaptive jitter buffer for playback
│   ├── vad/                    # Voice Activity Detection
│   │   └── mod.rs              # Energy+ZCR dual-threshold with adaptive noise floor
│   ├── session/                # Session orchestration
│   │   ├── mod.rs              # SessionState, SessionHandle, events, commands
│   │   └── state.rs            # SessionPhase FSM with validated transitions
│   ├── agent/                  # Agent framework integrations
│   │   ├── mod.rs              # Trait definitions
│   │   ├── function_registry.rs # Local function call dispatch
│   │   ├── adk_bridge.rs       # Google ADK streaming integration
│   │   └── a2a_client.rs       # A2A protocol client (discovery, task, streaming)
│   ├── flow/                   # Conversation flow control
│   │   ├── mod.rs              # Full-duplex coordination
│   │   ├── barge_in.rs         # Interruption detection and handling
│   │   └── turn_detection.rs   # Client-side turn detection (complements server VAD)
│   └── telemetry/              # Observability
│       ├── mod.rs              # OTel + structured logging + metrics init
│       ├── spans.rs            # Span definitions for each operation
│       ├── metrics.rs          # Metric definitions (counters, histograms, gauges)
│       └── logging.rs          # Structured log events
├── examples/
│   ├── simple_conversation.rs  # Minimal voice chat
│   ├── function_calling_agent.rs # Agent with tool use
│   ├── adk_bridge.rs           # ADK integration example
│   ├── a2a_dispatch.rs         # Multi-agent A2A collaboration
│   └── text_only_agent.rs      # Text-mode for testing
└── benches/
    ├── buffer_throughput.rs    # Ring buffer operations/sec
    └── vad_latency.rs          # VAD processing time per frame
```

-----

## 4. Detailed Component Design

### 4.1 Protocol Layer (`protocol/`)

Maps one-to-one to the Gemini Multimodal Live API wire format. Every message Gemini can send or receive has a corresponding Rust type with `serde` derive for zero-effort (de)serialization.

**Key types:**

- `ClientMessage` — enum of all client→server messages (`Setup`, `RealtimeInput`, `ClientContent`, `ToolResponse`, `ActivitySignal`)
- `ServerMessage` — enum of all server→client messages (`SetupComplete`, `ServerContent`, `ToolCall`, `ToolCallCancellation`, `GoAway`, `SessionResume`)
- `SessionConfig` — builder pattern for configuring model, voice, tools, audio format, system instruction

**Design decisions:**

- `#[serde(rename_all = "camelCase")]` everywhere to match Gemini’s JSON schema without runtime transformation
- `Part` uses `#[serde(untagged)]` to handle the polymorphic content parts (text, inlineData, functionCall, etc.)
- `SetupMessage` is pre-serialized once at connection time and reused across reconnections
- Base64 audio encoding uses `base64::engine::general_purpose::STANDARD` with pre-allocated buffers

### 4.2 Transport Layer (`transport/`)

**Connection lifecycle:**

```
connect_async(ws_url)
    → split into (write_half, read_half)
    → send SetupMessage on write_half
    → wait for setupComplete on read_half
    → spawn send_task (reads commands, writes to WS)
    → spawn recv_task (reads from WS, emits events)
    → on disconnect: backoff → reconnect with session resume handle
```

**Full-duplex architecture:** The WebSocket is split into independent send and receive halves using `tokio-tungstenite`’s `StreamExt::split()`. This allows true full-duplex operation — audio can stream to Gemini while simultaneously receiving model responses. No multiplexing, no head-of-line blocking.

**Flow control:** A `TokenBucket` rate limiter prevents overwhelming the WebSocket with audio faster than the network can deliver. The refill rate is calibrated to the audio format’s bitrate (e.g., 16kHz × 16-bit = 256 kbps for PCM16). A congestion window tracks in-flight messages and reduces send rate when acknowledgments slow down.

**Reconnection:** Exponential backoff with jitter (`base_delay × 2^attempt`, capped at `max_delay`). The Gemini session resume handle is preserved across reconnections, allowing the server to restore conversation context without re-sending history.

### 4.3 Audio Engine (`buffer/`)

**SPSC Ring Buffer:**

The hot path for audio data. Lock-free, cache-line-aligned, power-of-two capacity for bitwise modulo.

- **Producer**: Audio capture thread writes PCM samples
- **Consumer**: Transport send task reads and base64-encodes for WS transmission
- **Guarantees**: Wait-free on fast path (atomic store/load only), bounded memory, no allocation after init

Performance target: > 10M samples/sec throughput (verified by benchmark).

**Adaptive Jitter Buffer:**

Network audio arrives in variable-size bursts. The jitter buffer:

1. Accumulates a minimum depth before starting playback (configurable, default 200ms)
1. Measures inter-arrival jitter using EWMA and adjusts target depth dynamically
1. On underrun: fills with silence (click-free), transitions to `Filling` state, waits for buffer to recover
1. On overflow: drops oldest samples (bounded memory)
1. On interruption (barge-in): instant `flush()` — clears buffer, resets state

### 4.4 Voice Activity Detection (`vad/`)

Client-side VAD complements Gemini’s server-side VAD:

**Algorithm**: Dual-threshold energy detector with zero-crossing rate (ZCR) confirmation.

```
                ┌─────────┐
                │ Silence │◄──── energy below stop_threshold
                └────┬────┘      for hangover_duration
                     │
        energy above start_threshold
        AND ZCR in speech range
                     │
                     ▼
            ┌────────────────┐
            │ PendingSpeech  │◄── energy exceeded but
            └───────┬────────┘    min_duration not yet met
                    │
         min_speech_duration elapsed
                    │
                    ▼
              ┌──────────┐
              │  Speech  │──── energy drops below stop_threshold
              └─────┬────┘
                    │
                    ▼
             ┌───────────┐
             │ Hangover  │──── keeps "speaking" state for
             └───────────┘     hangover_duration to prevent
                               choppy segmentation
```

**Adaptive noise floor**: During confirmed silence, the noise floor estimate is updated with a slow EWMA. Start/stop thresholds are raised relative to the measured noise floor, preventing false triggers in noisy environments.

**Pre-speech buffer**: The last N frames before speech onset are captured in a circular buffer and prepended to the first speech packet, preserving the beginning of utterances that would otherwise be clipped.

**Why client-side VAD matters even with server-side VAD:**

- **Bandwidth savings**: Don’t send silence over the network (can save 50-80% of upload bandwidth)
- **Latency reduction**: Signal `activityStart` before the server detects it, shaving server-side detection latency
- **Barge-in pre-emption**: Detect user speech locally and flush the playback jitter buffer instantly, before the server’s `interrupted` signal arrives

### 4.5 Session Manager (`session/`)

**State Machine (FSM):**

The session lifecycle is an explicit state machine with validated transitions:

```
Disconnected ──→ Connecting ──→ SetupSent ──→ Active
                                                 │
                          ┌──────────────────────┤
                          │                      │
                          ▼                      ▼
                    UserSpeaking          ModelSpeaking
                          │                      │
                          │                      ▼
                          │               ToolCallPending
                          │                      │
                          │                      ▼
                          │             ToolCallExecuting
                          │                      │
                          ▼                      ▼
                    Disconnecting ──→ Disconnected
```

Invalid transitions return `Err(SessionError::InvalidTransition)`. The phase is observable via a `watch::Receiver<SessionPhase>` channel — application code can `await wait_for_phase(Active)` without polling.

**Turn Tracking:** Each model response is tracked as a `Turn` with text parts, audio presence, tool calls, timestamps, and completion/interruption status. Turn history is preserved for application use (e.g., displaying transcripts, analytics).

**SessionHandle:** The public API surface. Cheaply cloneable (wraps `Arc`). Provides:

- `send_audio()`, `send_text()`, `send_tool_response()` — command producers
- `subscribe()` — event consumer (broadcast channel)
- `wait_for_phase()` — async phase observation
- `state` — read-only access to `SessionState`

### 4.6 Agent Framework (`agent/`)

Three integration modes for dispatching intelligence:

**4.6.1 Local Function Registry**

```rust
let mut registry = FunctionRegistry::new();
registry.register("get_weather", get_weather_schema(), |args| async {
    let city = args["city"].as_str().unwrap();
    Ok(json!({ "temperature": 22, "condition": "sunny" }))
});

// When Gemini requests a tool call, the registry dispatches locally
session.on_tool_call(|calls| {
    let responses = registry.execute_all(calls).await;
    session.send_tool_response(responses).await;
});
```

Functions are registered with their JSON Schema declaration (sent to Gemini in the setup message) and an async handler. Execution is parallel — multiple tool calls from a single model turn are dispatched concurrently via `tokio::join!`.

**4.6.2 ADK Bridge**

Bridges `gemini-live-rs` sessions to Google’s Agent Development Kit for complex agent orchestration:

```rust
let adk = AdkBridge::new(AdkConfig {
    adk_endpoint: "http://localhost:8080".parse()?,
    agent_name: "financial_advisor",
    ..Default::default()
});

// Tool calls from Gemini Live → dispatched to ADK agent
// ADK agent responses → sent back as tool responses
session.attach_adk_bridge(adk);
```

The bridge translates between Gemini’s `FunctionCall`/`FunctionResponse` protocol and ADK’s `LiveRequestQueue`/event model. This allows complex multi-step agent workflows to run in ADK while the voice interface runs in `gemini-live-rs`.

**4.6.3 A2A Client**

Implements the Agent2Agent protocol for inter-agent collaboration:

```rust
let a2a = A2AClient::new(A2AConfig {
    agent_card_url: "https://remote-agent.example.com/.well-known/agent.json",
    ..Default::default()
});

// Discover remote agent capabilities
let card = a2a.discover().await?;

// Send task to remote agent (via tool call from Gemini)
registry.register("delegate_to_specialist", schema, |args| async {
    let task = a2a.send_task(args["task_description"].as_str()?).await?;
    Ok(task.artifacts)
});
```

Supports all A2A v0.3 features: agent card discovery, JSON-RPC task management, SSE streaming for long-running tasks, and gRPC transport.

### 4.7 Conversation Flow (`flow/`)

**Barge-in Handling:**

```
User starts speaking during model response
    │
    ├── Client VAD detects speech → flush jitter buffer (instant silence)
    │                              → send activityStart signal
    │
    ├── Server detects interruption → sends interrupted=true in serverContent
    │                                → model stops generating
    │
    └── Session state: ModelSpeaking → Interrupted → Active/UserSpeaking
```

The key insight: the jitter buffer flush happens **before** the server’s interrupt confirmation arrives, eliminating the round-trip-time delay that Pipecat suffers from.

**Turn Detection:**

Client-side turn detection complements the server’s VAD:

1. VAD transitions from `Speech → Hangover → Silence` (speech end detected)
1. Configurable end-of-speech delay (default 300ms, tunable per application)
1. Option to use a local turn detection model (e.g., Silero-style) for semantic completion detection
1. Signal `activityEnd` to server, allowing it to start model generation faster

### 4.8 Observability (`telemetry/`)

Three pillars, all modular and feature-gated:

**OpenTelemetry Tracing** (`tracing-support` feature):

```rust
// Every session operation creates a span
#[instrument(skip(audio_data))]
async fn send_audio_chunk(&self, audio_data: &[u8]) -> Result<()> {
    // span: "gemini.live.send_audio"
    // attributes: chunk_size, session_id, phase
}
```

Span hierarchy:

```
gemini.live.session
  ├── gemini.live.connect
  │     └── gemini.live.setup
  ├── gemini.live.send_audio (repeated)
  ├── gemini.live.receive_content
  │     ├── gemini.live.text_delta
  │     └── gemini.live.audio_chunk
  ├── gemini.live.tool_call
  │     ├── gemini.live.tool_execute (per function)
  │     └── gemini.live.tool_response
  └── gemini.live.disconnect
```

**Structured Logging:**

```rust
// Every event is a structured log with consistent fields
info!(
    session_id = %self.session_id,
    phase = %self.state.phase(),
    event = "tool_call_received",
    function_count = calls.len(),
    "Model requested {} function calls", calls.len()
);
```

Log levels follow a clear policy:

- `ERROR`: Unrecoverable failures (connection lost after max retries)
- `WARN`: Recoverable issues (reconnection attempt, jitter buffer underrun)
- `INFO`: Lifecycle events (connected, disconnected, turn complete)
- `DEBUG`: Wire-level detail (message sizes, buffer depths)
- `TRACE`: Per-frame data (VAD energy levels, individual audio chunks)

**Prometheus Metrics** (`metrics` feature):

|Metric                                  |Type     |Description                                    |
|----------------------------------------|---------|-----------------------------------------------|
|`gemini_live_sessions_active`           |Gauge    |Currently active sessions                      |
|`gemini_live_audio_latency_ms`          |Histogram|Time from audio capture to WS send             |
|`gemini_live_response_latency_ms`       |Histogram|Time from end-of-speech to first model response|
|`gemini_live_jitter_buffer_depth_ms`    |Gauge    |Current playback buffer depth                  |
|`gemini_live_jitter_underruns_total`    |Counter  |Playback underrun events                       |
|`gemini_live_vad_speech_segments_total` |Counter  |Speech segments detected                       |
|`gemini_live_tool_calls_total`          |Counter  |Tool calls dispatched (label: function_name)   |
|`gemini_live_tool_call_duration_ms`     |Histogram|Tool execution time                            |
|`gemini_live_reconnections_total`       |Counter  |Reconnection attempts                          |
|`gemini_live_ws_messages_sent_total`    |Counter  |WebSocket frames sent                          |
|`gemini_live_ws_messages_received_total`|Counter  |WebSocket frames received                      |
|`gemini_live_ws_bytes_sent_total`       |Counter  |Total bytes sent                               |
|`gemini_live_ws_bytes_received_total`   |Counter  |Total bytes received                           |

-----

## 5. Key Design Decisions — Motivated by Real Production Failures

Each decision below is motivated by a specific class of production failure observed in Pipecat and/or LiveKit issue trackers.

### 5.1 Why Rust (Motivated by: Pipecat GIL + lifecycle issues)

Pipecat’s `ThreadPoolExecutor` deadlocks (#1912), pipeline task leaks (#953), and OOM after cancellation (#1809) all trace to Python’s concurrency model: the GIL serializes CPU work, asyncio tasks accumulate without deterministic cleanup, and garbage collection introduces latency spikes.

Rust eliminates these by construction:

|Property                  |Benefit                                                                 |Production Failure It Prevents                        |
|--------------------------|------------------------------------------------------------------------|------------------------------------------------------|
|No GC                     |Predictable latency — no stop-the-world pauses                          |Audio stutters during GC in Python pipelines          |
|No GIL                    |True parallelism — VAD, encoding, network I/O run simultaneously        |Pipecat GIL contention on concurrent pipelines (#1912)|
|Ownership system          |Tasks cannot leak resources — `Drop` runs deterministically             |Pipeline task accumulation and OOM (#953, #1809)      |
|`unsafe` for lock-free    |SPSC ring buffer with atomic head/tail — verified safe by SPSC invariant|N/A (Pipecat has no equivalent)                       |
|`async`/`await` with Tokio|Efficient multiplexing of thousands of sessions on a thread pool        |Pipecat’s asyncio deadlocks on shutdown (#3179)       |
|Small binary              |~5MB release binary vs ~100MB+ Python runtime + dependencies            |LiveKit 100MB memory per single call (#386)           |

The practical consequence: where Pipecat runs ~10-20 sessions per core with ~50-100MB each, we target ~500+ sessions per core at ~2-5MB each.

### 5.2 Why No WebRTC (Motivated by: LiveKit SFU conflicts with direct API)

LiveKit’s Gemini integration issues (#4441, #4497, #1679, #3870) demonstrate what happens when you wrap a WebSocket-based direct API in WebRTC infrastructure: spurious VAD events cancel tools, context injection strips system messages, connection bombardment triggers rate limits, and duplicate responses appear.

WebRTC’s protocol stack (ICE negotiation ~200-500ms, STUN/TURN dependency, DTLS handshake ~100ms, SRTP per-packet overhead, SDP offer/answer complexity) exists to solve peer-to-peer NAT traversal. The Gemini Live API is a client-server WebSocket — the simplest possible transport. We need a reliable, ordered, bidirectional channel; that is exactly what WSS provides.

### 5.3 Why Direct API Access (Motivated by: abstraction layer conflicts)

Both frameworks suffer from their abstraction layers conflicting with Gemini’s native capabilities:

- Pipecat’s pipeline model (STT → LLM → TTS stages) doesn’t map to Gemini Live (which does everything in one model). This causes function calling results to be silently dropped (#908) because the context aggregator designed for separate LLM providers doesn’t handle Gemini’s unified response format.
- LiveKit’s `AgentActivity` blindly calls `self.interrupt()` on every Gemini VAD signal (#4441), because the interruption model was designed for scenarios where the framework owns VAD. The agent literally speaks protocol text “tools_output” (#2174) because deprecated serialization methods send raw wire format as speech.

```
Pipecat:  Client → Daily SFU → Pipecat Server → Gemini API
LiveKit:  Client → LiveKit SFU → Agent Process → Gemini API
Ours:     Client → gemini-live-rs → Gemini API (direct)
```

We speak the wire protocol directly. Every Gemini message type is a Rust enum variant. `serde` handles serialization. There is no abstraction layer to misinterpret, silently drop, or incorrectly transform Gemini’s signals.

### 5.4 Why Actor-Per-Session (Motivated by: pipeline lifecycle failures)

Pipecat’s shared-pipeline model means one misbehaving component blocks all sessions: audio mixer infinite loops cause global OOM (#1338), `EndFrame` propagation failures cause global task accumulation (#953), and `ThreadPoolExecutor` deadlocks freeze the entire process (#1912).

LiveKit’s agent model is better (one agent process per session) but still shares state within the agent: participant not unlinked on disconnect (#1124), second connection fails because old state persists (#3353), worker killed before cleanup callbacks finish (#322).

Each `SessionHandle` in our library is an independent constellation of Tokio tasks:

```
Session N
  ├── send_task    (reads commands, writes to WS)
  ├── recv_task    (reads from WS, dispatches events)
  └── [vad_task]   (optional, processes audio for VAD)
```

Sessions share no mutable state. Session A crashing does not affect session B. When a session ends (gracefully or not), its tasks are cancelled and memory is reclaimed by Rust’s ownership system — no GC needed, no cleanup callbacks to race against process termination.

### 5.5 Why Built-in Observability (Motivated by: LiveKit #2260 and #4055)

LiveKit’s most-upvoted feature request (57+ 👍) is native OTel instrumentation (#2260). When they did add recording/observability, it degraded voice quality to 2x slower and distorted (#4055) — because the recording thread contends with the audio encoding thread in Python’s GIL.

We build OTel, structured logging, and Prometheus metrics into the architecture from the start, feature-gated behind `tracing-support` and `metrics` flags. When disabled, they compile to zero-cost no-ops. When enabled, they run on separate Tokio tasks that never contend with the audio hot path. Turn-level correlation (the feature requested in LiveKit #4054) is built in — every span carries `session_id` as a field.

### 5.6 Why Validated State Machine (Motivated by: LiveKit #3418)

LiveKit’s “agent goes silent while status shows speaking” (#3418) is a state machine desynchronization bug. The TTS pipeline has been cancelled, but the state machine still says “speaking.” The framework provides no mechanism to detect this inconsistency.

Our FSM uses `can_transition_to()` with exhaustive `matches!` macro validation. Invalid transitions are rejected at the call site. Phase observers (`tokio::sync::watch`) allow any component to subscribe to state changes and detect stuck states. If the FSM is in `ModelSpeaking` but no audio arrives within a configurable timeout, it transitions to `Active` and surfaces a warning span.

-----

## 6. Feature Parity Matrix vs Pipecat & LiveKit

Every ⚠️ or ❌ below for Pipecat/LiveKit is traceable to a specific GitHub issue. This is not marketing — it’s engineering due diligence.

|Feature Category     |Feature                          |Pipecat                                      |LiveKit                                     |gemini-live-rs                          |
|---------------------|---------------------------------|---------------------------------------------|--------------------------------------------|----------------------------------------|
|**Core Audio**       |PCM16 streaming                  |✅                                            |✅                                           |✅                                       |
|                     |Opus codec support               |✅ (via service)                              |✅ (WebRTC)                                  |✅ (feature flag)                        |
|                     |Sample rate conversion           |⚠️ (mulaw→PCM broken for Twilio→Gemini, #1823)|✅                                           |✅ (rubato)                              |
|                     |Jitter buffer                    |❌ (#3222 — explicitly confirmed missing)     |✅ (WebRTC built-in)                         |✅ (adaptive EWMA)                       |
|                     |Noise reduction                  |✅ (filter)                                   |✅ (BVC plugin)                              |🔮 (planned)                             |
|                     |Echo cancellation                |❌ (self-interrupt on speaker, #188)          |✅ (WebRTC)                                  |🔮 (client responsibility)               |
|**VAD**              |Silero VAD                       |✅                                            |✅                                           |❌ (not needed — server VAD + energy VAD)|
|                     |Energy-based VAD                 |❌                                            |❌                                           |✅ (with adaptive noise floor)           |
|                     |Server-side VAD relay            |⚠️ (params not exposed, #1606)                |⚠️ (spurious events cancel tools, #4441)     |✅ (first-class signal handling)         |
|                     |Adaptive noise floor             |❌                                            |❌                                           |✅                                       |
|                     |Pre-speech buffering             |❌                                            |⚠️ (leading audio dropped on barge-in, #3261)|✅                                       |
|**Turn Detection**   |Silence-based                    |✅                                            |✅                                           |✅                                       |
|                     |Semantic/ML-based                |✅ (Smart Turn)                               |⚠️ (text-only, no prosody — #3094)           |🔮 (pluggable, but Gemini VAD is primary)|
|                     |Server-side (Gemini VAD)         |⚠️ (not fully exposed)                        |⚠️ (conflicts with agent interruption, #4441)|✅ (authoritative)                       |
|                     |State-dependent sensitivity      |❌ (#2153)                                    |❌ (#3427 — can’t tune thinking vs speaking) |✅ (FSM-aware, per-phase config)         |
|                     |False-interruption resume        |❌                                            |⚠️ (regression in 1.3.3, #4039)              |✅ (server VAD handles natively)         |
|**Transport**        |WebSocket                        |✅                                            |❌ (WebRTC)                                  |✅                                       |
|                     |WebRTC                           |✅ (Daily/LK)                                 |✅ (best-in-class)                           |❌ (not needed for Gemini)               |
|                     |Reconnection                     |⚠️ (session resume not supported, #1674)      |⚠️ (auto-reconnect unreliable, #3145)        |✅ (resume handle + GoAway)              |
|                     |Backpressure                     |❌                                            |✅ (WebRTC)                                  |✅ (token bucket)                        |
|**Conversation**     |Barge-in / Interruption          |⚠️ (broken across transports, #2460, #3191)   |⚠️ (agent goes silent, #3418)                |✅ (atomic flush)                        |
|                     |Context after interruption       |❌ (partial speech lost, #2791, #1591)        |⚠️ (system messages stripped, #4497)         |✅ (Gemini manages context)              |
|                     |Transcription (input)            |✅ (via STT)                                  |✅ (via STT)                                 |✅ (native Gemini)                       |
|                     |Transcription (output)           |✅ (via text)                                 |✅ (via text)                                |✅ (native Gemini)                       |
|**Tool Use**         |Function calling                 |⚠️ (results silently dropped, #908)           |⚠️ (agent speaks protocol text, #2174)       |✅ (native wire format)                  |
|                     |Parallel tool execution          |⚠️                                            |✅                                           |✅                                       |
|                     |Tool call cancellation           |❌                                            |❌ (forced by spurious VAD, #4441)           |✅ (native Gemini)                       |
|                     |Async function calling           |❌                                            |❌ (#2367 — requested)                       |✅ (NON_BLOCKING behavior)               |
|                     |Code execution                   |❌                                            |❌                                           |✅ (native Gemini)                       |
|**Agent Integration**|ADK                              |❌                                            |❌                                           |✅                                       |
|                     |A2A Protocol                     |❌                                            |❌                                           |✅                                       |
|                     |MCP                              |❌                                            |❌                                           |🔮 (planned)                             |
|                     |LangChain/LangGraph              |✅ (Python native)                            |✅ (Python native)                           |🔮 (via FFI/HTTP)                        |
|**Multimodal**       |Video/image input                |✅                                            |✅                                           |✅ (JPEG frames)                         |
|                     |Screen sharing                   |❌                                            |✅ (WebRTC)                                  |🔮 (frame capture)                       |
|                     |Native audio reasoning           |❌                                            |⚠️ (model not in allowed list, #3747)        |✅ (Gemini 2.5 native audio)             |
|**Observability**    |Structured logging               |✅ (loguru)                                   |✅                                           |✅ (tracing crate)                       |
|                     |OpenTelemetry                    |❌                                            |❌ (most requested feature, #2260)           |✅ (full OTel)                           |
|                     |Prometheus metrics               |❌                                            |❌ (cloud-only dashboard)                    |✅                                       |
|                     |Turn-level correlation           |❌                                            |❌ (STT missing speech_id, #4054)            |✅ (session_id on all spans)             |
|                     |Observability without perf impact|N/A                                          |❌ (recording degrades voice, #4055)         |✅ (separate Tokio tasks)                |
|**State Management** |Explicit FSM                     |❌ (implicit pipeline position)               |⚠️ (desync possible, #3418)                  |✅ (validated transitions)               |
|                     |Phase observation                |❌                                            |❌                                           |✅ (watch channel)                       |
|                     |Session persistence              |❌                                            |✅ (cloud)                                   |✅ (resume handle)                       |
|**Lifecycle**        |Clean shutdown                   |❌ (deadlocks, leaks — #953, #1809, #3179)    |⚠️ (callbacks killed — #322)                 |✅ (task cancellation)                   |
|                     |Concurrent sessions              |❌ (ThreadPool deadlock, #1912)               |✅ (Go SFU)                                  |✅ (actor-per-session)                   |
|                     |Idle detection + recovery        |⚠️ (race condition with Gemini, #3149)        |⚠️ (empty response = silence, #4066)         |✅ (FSM timeout + health check)          |
|**Deployment**       |Server                           |✅                                            |✅                                           |✅                                       |
|                     |Edge / embedded                  |❌                                            |❌                                           |✅                                       |
|                     |WASM                             |❌                                            |❌                                           |🔮 (planned)                             |
|                     |Mobile (native)                  |❌                                            |✅ (client SDK)                              |🔮 (C FFI)                               |
|**Performance**      |GIL-free                         |❌                                            |❌ (agent)                                   |✅                                       |
|                     |Memory per session               |~50-100MB                                    |~30-60MB                                    |~2-5MB                                  |

**Legend**: ✅ Supported · ⚠️ Partial/broken (issue cited) · ❌ Not supported · 🔮 Planned

-----

## 7. Gemini Live API Capabilities Leveraged

`gemini-live-rs` is designed to exploit every Gemini Multimodal Live API capability:

|Gemini Feature                    |How We Use It                                                          |
|----------------------------------|-----------------------------------------------------------------------|
|Bidirectional WebSocket           |Direct WSS, split into independent send/receive tasks                  |
|Server-side VAD                   |Relay signals to session state + combine with client VAD               |
|Input/output transcription        |Surfaced as `SessionEvent::InputTranscription` / `OutputTranscription` |
|Native audio reasoning (2.5 Flash)|Pass-through — no intermediate STT/TTS needed                          |
|Function calling                  |Native dispatch through `FunctionRegistry`, `AdkBridge`, or `A2AClient`|
|Tool call cancellation            |`ToolCallCancelled` event + state cleanup                              |
|Code execution                    |Surfaced as `Part::ExecutableCode` / `Part::CodeExecutionResult`       |
|Session resume                    |Resume handle preserved across reconnections                           |
|GoAway signal                     |Graceful shutdown with time-left awareness                             |
|Activity signals                  |Client VAD → `activityStart`/`activityEnd` signals                     |
|Multi-model support               |`GeminiModel` enum covers Flash Live, 2.5 Flash Native Audio, custom   |

-----

## 8. Error Handling Philosophy

### 8.1 Error Categories

```rust
pub enum SessionError {
    // Transient — will be retried automatically
    WebSocket(String),        // Network glitch
    Timeout,                  // Handshake or setup timeout
    
    // Logic — programming error, should not occur in correct usage
    InvalidTransition { from, to },  // FSM violation
    NotConnected,                     // Sent command before connect
    
    // Fatal — session cannot continue
    SetupFailed(String),      // Server rejected configuration
    GoAway,                   // Server requested disconnect
    ChannelClosed,            // Internal channel dropped
}
```

### 8.2 Recovery Strategy

|Error Type               |Action                                                       |
|-------------------------|-------------------------------------------------------------|
|WS disconnect (transient)|Exponential backoff reconnection with session resume         |
|Setup failure            |Surface to application, do not retry (config likely wrong)   |
|Tool call timeout        |Cancel pending call, notify model of failure                 |
|Jitter buffer underrun   |Fill with silence, log warning, adjust buffer depth          |
|VAD false positive       |Hangover timer absorbs, no user-visible impact               |
|GoAway                   |Begin graceful shutdown, notify application of time remaining|

-----

## 9. Security Considerations

1. **API Key Handling**: The API key is held in `SessionConfig` and only transmitted in the WebSocket URL query parameter (as required by Gemini). Never logged, never serialized to disk.
1. **TLS**: All connections use WSS (WebSocket Secure). `tokio-tungstenite` with `native-tls` feature uses the system’s TLS implementation.
1. **No Key Embedding**: Unlike Pipecat’s server-only architecture (which requires API keys on the server), `gemini-live-rs` can use ephemeral tokens for client-side deployments, preventing key exposure.
1. **A2A Authentication**: Supports OAuth 2.0, API keys, and OpenID Connect per the A2A v0.3 specification.
1. **Audio Data**: Audio is base64-encoded in transit (Gemini’s requirement) over TLS. No audio is stored by the library — it flows through ring buffers and is overwritten.

-----

## 10. Future Roadmap

|Phase             |Features                                                                   |
|------------------|---------------------------------------------------------------------------|
|**v0.1** (Current)|Core protocol, transport, buffers, VAD, session FSM, function calling, OTel|
|**v0.2**          |ADK bridge, A2A client, MCP tool support, Opus codec                       |
|**v0.3**          |WASM target, C FFI for mobile, multi-session multiplexer                   |
|**v0.4**          |Video frame streaming, screen capture integration                          |
|**v0.5**          |Production hardening: chaos testing, fuzzing, formal verification of FSM   |
|**v1.0**          |Stable API, backward compatibility guarantees, comprehensive benchmarks    |

-----

*This document reflects the architecture as of March 2026. The Gemini Multimodal Live API is actively evolving; this library evolves with it.*

# gemini-live-rs — Implementation Guide

**From Design to Code: A Complete Implementation Reference**

Version 0.1 · March 2026

-----

## 1. Implementation Overview

This document provides the concrete implementation plan for `gemini-live-rs`. It maps every design component to specific Rust types, crates, and code patterns. Use this as your blueprint when writing the actual code.

### 1.1 Implementation Phases

```
Phase 1: Foundation (Weeks 1-3)
  ├── Protocol types + serde round-trip tests
  ├── SPSC ring buffer + benchmarks
  ├── WebSocket transport + reconnection
  └── Session FSM + event system

Phase 2: Audio Intelligence (Weeks 4-5)
  ├── Client-side VAD
  ├── Jitter buffer
  ├── Barge-in handling
  └── Flow control (token bucket)

Phase 3: Agent Integration (Weeks 6-8)
  ├── Function registry + dispatch
  ├── ADK bridge
  ├── A2A client
  └── MCP tool support

Phase 4: Observability (Week 9)
  ├── OTel tracing integration
  ├── Structured logging
  └── Prometheus metrics

Phase 5: Hardening (Weeks 10-12)
  ├── Fuzzing (cargo-fuzz on protocol parsing)
  ├── Property tests (proptest on ring buffer)
  ├── Integration tests with live Gemini API
  └── Benchmarks + optimization pass
```

-----

## 2. Crate Dependencies and Rationale

### 2.1 Core Dependencies

|Crate                 |Version   |Purpose             |Why This One                           |
|----------------------|----------|--------------------|---------------------------------------|
|`tokio`               |1.x (full)|Async runtime       |Industry standard, best ecosystem      |
|`tokio-tungstenite`   |0.24      |WebSocket client    |Async, supports WSS via native-tls     |
|`serde` + `serde_json`|1.x       |Serialization       |Zero-overhead for JSON wire format     |
|`base64`              |0.22      |Audio encoding      |Gemini requires base64 inline data     |
|`crossbeam`           |0.8       |Lock-free primitives|Battle-tested lock-free data structures|
|`parking_lot`         |0.12      |Faster mutexes      |2-3x faster than `std::sync::Mutex`    |
|`dashmap`             |6.x       |Concurrent hashmap  |For pending tool call tracking         |
|`thiserror`           |2.x       |Error types         |Ergonomic derive for error enums       |
|`uuid`                |1.x (v4)  |Session/turn IDs    |Standard UUID generation               |
|`bytes`               |1.x       |Byte buffers        |Zero-copy byte manipulation            |
|`arc-swap`            |1.x       |Atomic Arc swap     |Hot-swap configuration without locks   |

### 2.2 Audio Dependencies

|Crate     |Version|Purpose                                         |
|----------|-------|------------------------------------------------|
|`rubato`  |0.15   |Sample rate conversion (16kHz ↔ 24kHz ↔ 48kHz)  |
|`dasp`    |0.11   |DSP primitives (sample conversion, ring buffers)|
|`audiopus`|0.3    |Opus codec (optional, feature-gated)            |

### 2.3 Observability Dependencies (Feature-Gated)

|Crate                        |Feature Flag     |Purpose                            |
|-----------------------------|-----------------|-----------------------------------|
|`tracing`                    |`tracing-support`|Structured spans                   |
|`tracing-subscriber`         |`tracing-support`|Span export (stdout, OTLP)         |
|`metrics`                    |`metrics`        |Counter/gauge/histogram definitions|
|`metrics-exporter-prometheus`|`metrics`        |Prometheus endpoint                |

### 2.4 Agent Dependencies

|Crate           |Purpose                               |
|----------------|--------------------------------------|
|`reqwest` (json)|HTTP client for ADK bridge, A2A client|
|`http`          |HTTP types for A2A protocol           |

-----

## 3. Module Implementation Details

### 3.1 Protocol Layer

**File: `src/protocol/mod.rs`**

The protocol module is the most critical for correctness — a single field name mismatch means Gemini rejects our messages silently.

**Implementation strategy:**

1. Transcribe every message type from the [Gemini Live API reference](https://ai.google.dev/gemini-api/docs/live)
1. Write a round-trip test for each: `assert_eq!(from_str(to_string(msg)), msg)`
1. Test against captured real Gemini responses (save JSON from a working session)

**Critical serde patterns:**

```rust
// Gemini uses camelCase JSON keys
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerContentPayload {
    pub model_turn: Option<Content>,      // → "modelTurn"
    pub turn_complete: Option<bool>,       // → "turnComplete"
    pub interrupted: Option<bool>,         // → "interrupted"
}

// Parts are polymorphic — discriminated by field presence, not a "type" tag
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum Part {
    Text { text: String },
    InlineData { #[serde(rename = "inlineData")] inline_data: Blob },
    FunctionCall { #[serde(rename = "functionCall")] function_call: FunctionCall },
    // ...
}

// Server messages are also untagged — discriminated by top-level key
#[derive(Deserialize)]
#[serde(untagged)]
pub enum ServerMessage {
    SetupComplete(SetupCompleteMessage),    // has "setupComplete" key
    ServerContent(ServerContentMessage),     // has "serverContent" key
    ToolCall(ToolCallMessage),              // has "toolCall" key
    // ...
}
```

**⚠️ Gotcha**: `#[serde(untagged)]` tries variants in order. Put the most specific variants first, most general last. The `Unknown(serde_json::Value)` variant must be last.

**Setup message pre-serialization:**

```rust
impl SessionConfig {
    /// Pre-serialize the setup message. Called once at connection time.
    /// The resulting String is reused across reconnections.
    pub fn to_setup_json(&self) -> String {
        let msg = self.to_setup_message();
        serde_json::to_string(&msg).expect("setup serialization is infallible")
    }
}
```

**Test pattern:**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn round_trip_setup_message() {
        let config = SessionConfig::new("test-key")
            .model(GeminiModel::Gemini2_0FlashLive)
            .voice(Voice::Kore)
            .system_instruction("You are a helpful assistant.");
        
        let msg = config.to_setup_message();
        let json = serde_json::to_string(&msg).unwrap();
        
        // Verify key fields are present with correct casing
        assert!(json.contains("\"setup\""));
        assert!(json.contains("\"generationConfig\""));
        assert!(json.contains("\"voiceName\":\"Kore\""));
    }
    
    #[test]
    fn parse_real_server_content() {
        // Captured from a real Gemini session
        let json = r#"{
            "serverContent": {
                "modelTurn": {
                    "parts": [{"text": "Hello! How can I help?"}]
                },
                "turnComplete": true
            }
        }"#;
        
        let msg: ServerMessage = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, ServerMessage::ServerContent(_)));
    }
}
```

### 3.2 Ring Buffer Implementation

**File: `src/buffer/mod.rs`**

**Key implementation details:**

```rust
/// Cache-line alignment prevents false sharing.
/// On x86_64, a cache line is 64 bytes. We use 128 to be safe
/// across architectures (Apple M-series uses 128-byte lines).
#[repr(align(128))]
struct CachePad<T>(T);

pub struct SpscRing<T: Copy + Default> {
    buf: Box<[T]>,           // Heap-allocated, but never reallocated
    cap_mask: usize,          // capacity - 1, for bitwise AND (replaces modulo)
    head: CachePad<AtomicUsize>, // Producer writes here (separate cache line)
    tail: CachePad<AtomicUsize>, // Consumer reads here (separate cache line)
}
```

**Why `Box<[T]>` not `Vec<T>`**: A `Vec` has a length field that could be accidentally modified. A boxed slice has a fixed size — it’s a pointer + length, both immutable after creation.

**Why power-of-two capacity**: `index & cap_mask` replaces `index % capacity`. Bitwise AND is a single CPU cycle; modulo can be 20-40 cycles on x86.

**Memory ordering:**

```rust
// Producer (write path):
// 1. Load tail with Acquire (see consumer's latest progress)
// 2. Write data (plain stores — fine because we're the only writer)
// 3. Store head with Release (make our writes visible to consumer)

// Consumer (read path):
// 1. Load head with Acquire (see producer's latest progress)
// 2. Read data (plain loads — fine because we're the only reader)
// 3. Store tail with Release (make our progress visible to producer)
```

**Benchmark target:**

```rust
// benches/buffer_throughput.rs
fn bench_write_read(c: &mut Criterion) {
    let ring = SpscRing::<i16>::new(65536);
    let data = vec![42i16; 1600]; // 100ms @ 16kHz
    
    c.bench_function("spsc_write_1600_samples", |b| {
        b.iter(|| ring.write(black_box(&data)))
    });
    // Target: < 100ns per write (> 10M samples/sec)
}
```

### 3.3 Jitter Buffer Implementation

**File: `src/buffer/jitter.rs`**

**State machine:**

```rust
pub enum BufferState {
    Filling,    // Accumulating initial depth
    Playing,    // Normal playback
    Underrun,   // Generating silence
}

// Transitions:
// Filling + depth >= adaptive_min_depth → Playing
// Playing + pull exhausts data → Underrun
// Underrun + depth >= adaptive_min_depth → Playing
// Any state + flush() → Filling
```

**Adaptive depth calculation:**

```rust
fn adaptive_min_depth(&self) -> usize {
    // Convert jitter estimate (microseconds) to samples
    let jitter_samples = (self.jitter_estimate_us / 1_000_000.0
        * self.config.sample_rate as f64
        * self.config.target_jitter_multiple) as usize;
    
    // Never go below configured minimum
    jitter_samples.max(self.config.min_depth_samples)
}
```

The jitter estimate uses an Exponential Weighted Moving Average (EWMA) of inter-arrival time variance. This is the same algorithm used in TCP’s RTT estimation (RFC 6298), adapted for audio packets.

**Flush for barge-in:**

```rust
pub fn flush(&mut self) {
    self.queue.clear();            // Drop all buffered audio
    self.state = BufferState::Filling;  // Must re-accumulate before playback
    self.last_arrival = None;      // Reset timing
    self.arrival_intervals.clear(); // Reset jitter estimate
}
```

This is called the instant client-side VAD detects user speech during model playback. The result is immediate silence — no waiting for the server’s interrupt signal.

### 3.4 VAD Implementation

**File: `src/vad/mod.rs`**

**Energy computation (optimized):**

```rust
fn compute_energy_db(samples: &[i16]) -> f64 {
    if samples.is_empty() { return -96.0; }
    
    // Sum of squares — this is the hot path
    let sum_sq: f64 = samples.iter()
        .map(|&s| {
            let f = s as f64;
            f * f  // Avoid powi(2) — plain multiply is faster
        })
        .sum();
    
    let rms = (sum_sq / samples.len() as f64).sqrt();
    let db = 20.0 * (rms / 32767.0).log10();  // dBFS relative to i16::MAX
    db.max(-96.0)  // Floor at -96 dBFS (silence)
}
```

**Potential SIMD optimization for Phase 5:**

```rust
#[cfg(target_arch = "x86_64")]
fn compute_energy_db_simd(samples: &[i16]) -> f64 {
    // Use _mm256_madd_epi16 for vectorized sum-of-squares
    // 16 samples per iteration instead of 1
    // Expected speedup: ~8-12x
    todo!("SIMD optimization")
}
```

**Adaptive noise floor update:**

```rust
// Only update during confirmed silence (not during hangover)
if self.state == VadState::Silence {
    self.noise_frames += 1;
    // Alpha decreases over time: fast initial adaptation, slow drift
    let alpha = 0.01_f64.min(1.0 / self.noise_frames as f64);
    self.noise_floor_db = self.noise_floor_db * (1.0 - alpha) + energy_db * alpha;
}
```

This means the VAD automatically adapts to:

- Office background noise (~40-50 dBSPL)
- Coffee shop (~60 dBSPL)
- Factory floor (~70-80 dBSPL)

### 3.5 Transport Implementation

**File: `src/transport/connection.rs`**

**Connection sequence:**

```rust
pub async fn connect(
    config: SessionConfig,
    transport_config: TransportConfig,
) -> Result<SessionHandle, SessionError> {
    // 1. Create shared state
    let state = SessionState::new();
    
    // 2. Create channels
    let (command_tx, command_rx) = mpsc::channel(transport_config.send_queue_depth);
    let (event_tx, _) = broadcast::channel(transport_config.event_channel_capacity);
    let (phase_tx, phase_rx) = watch::channel(SessionPhase::Disconnected);
    
    // 3. Build handle (cheaply cloneable)
    let handle = SessionHandle { command_tx, event_tx, state, phase_watch: phase_rx };
    
    // 4. Spawn connection loop (owns command_rx, manages WS lifecycle)
    tokio::spawn(connection_loop(config, transport_config, state, command_rx, event_tx, phase_tx));
    
    Ok(handle)
}
```

**Full-duplex split pattern:**

```rust
let (ws_write, ws_read) = ws_stream.split();

// Receive task — dedicated to reading from WebSocket
let recv_handle = tokio::spawn(async move {
    while let Some(msg) = ws_read.next().await {
        match msg {
            Ok(Message::Text(text)) => handle_server_message(&text, &state, &event_tx),
            Ok(Message::Binary(data)) => { /* raw audio */ },
            Ok(Message::Close(_)) => break,
            Err(e) => { event_tx.send(SessionEvent::Error(e.to_string())); break; },
            _ => {},
        }
    }
});

// Send task — dedicated to writing to WebSocket
loop {
    tokio::select! {
        cmd = command_rx.recv() => { /* serialize and send */ },
        _ = recv_handle.is_finished() => break,  // recv died, so should we
    }
}
```

**Server message dispatch (the parsing hot path):**

```rust
fn handle_server_message(text: &str, state: &SessionState, event_tx: &broadcast::Sender<SessionEvent>) {
    // Fast-path: check for top-level key presence before full deserialization
    // This avoids serde(untagged)'s try-all-variants overhead
    
    if text.contains("\"serverContent\"") {
        if let Ok(msg) = serde_json::from_str::<ServerContentMessage>(text) {
            // Handle model turn, transcriptions, interruptions, turn complete
        }
    } else if text.contains("\"toolCall\"") {
        if let Ok(msg) = serde_json::from_str::<ToolCallMessage>(text) {
            // Dispatch tool calls
        }
    } else if text.contains("\"toolCallCancellation\"") {
        // ...
    } else if text.contains("\"goAway\"") {
        // ...
    }
    // Unknown messages are logged at DEBUG level and ignored (forward compatibility)
}
```

**⚠️ Why string-contains before serde**: `serde(untagged)` tries every variant in order until one succeeds. For N variants, this is O(N) parse attempts. String-contains is O(1) and routes to the correct parser directly. At 30+ messages/second, this matters.

### 3.6 Session State Machine

**File: `src/session/state.rs`**

**Transition validation:**

```rust
impl SessionPhase {
    pub fn can_transition_to(&self, to: &SessionPhase) -> bool {
        matches!((self, to),
            // Connection lifecycle
            (Disconnected, Connecting)
            | (Connecting, SetupSent)
            | (SetupSent, Active)
            
            // Conversation flow
            | (Active, UserSpeaking)
            | (Active, ModelSpeaking)
            | (Active, ToolCallPending)
            
            // Barge-in
            | (ModelSpeaking, Interrupted)
            | (Interrupted, Active)
            
            // Tool flow
            | (ToolCallPending, ToolCallExecuting)
            | (ToolCallExecuting, Active)
            | (ToolCallExecuting, ModelSpeaking)
            
            // Universal force-disconnect
            | (_, Disconnected)
        )
    }
}
```

**Phase observation pattern:**

```rust
// Application code can await specific phases
let handle = connect(config, transport_config).await?;

// Wait for setup to complete
handle.wait_for_phase(SessionPhase::Active).await;

// Now safe to send audio/text
handle.send_text("Hello!").await?;
```

Implementation uses `tokio::sync::watch`:

```rust
pub async fn wait_for_phase(&self, target: SessionPhase) {
    let mut rx = self.phase_watch.clone();
    while *rx.borrow_and_update() != target {
        if rx.changed().await.is_err() { break; }
    }
}
```

### 3.7 Function Registry

**File: `src/agent/function_registry.rs`**

```rust
use std::future::Future;
use std::pin::Pin;
use std::collections::HashMap;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type FnHandler = Box<dyn Fn(serde_json::Value) -> BoxFuture<Result<serde_json::Value, String>> + Send + Sync>;

pub struct FunctionRegistry {
    handlers: HashMap<String, FnHandler>,
    declarations: Vec<FunctionDeclaration>,
}

impl FunctionRegistry {
    pub fn new() -> Self {
        Self { handlers: HashMap::new(), declarations: Vec::new() }
    }
    
    /// Register a function with its schema and async handler.
    pub fn register<F, Fut>(
        &mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Option<serde_json::Value>,
        handler: F,
    ) where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, String>> + Send + 'static,
    {
        let name = name.into();
        let description = description.into();
        
        self.declarations.push(FunctionDeclaration {
            name: name.clone(),
            description,
            parameters,
        });
        
        self.handlers.insert(name, Box::new(move |args| Box::pin(handler(args))));
    }
    
    /// Execute a single function call. Returns a FunctionResponse.
    pub async fn execute(&self, call: &FunctionCall) -> FunctionResponse {
        let result = if let Some(handler) = self.handlers.get(&call.name) {
            match handler(call.args.clone()).await {
                Ok(response) => response,
                Err(e) => serde_json::json!({ "error": e }),
            }
        } else {
            serde_json::json!({ "error": format!("Unknown function: {}", call.name) })
        };
        
        FunctionResponse {
            name: call.name.clone(),
            response: result,
            id: call.id.clone(),
        }
    }
    
    /// Execute multiple calls in parallel.
    pub async fn execute_all(&self, calls: &[FunctionCall]) -> Vec<FunctionResponse> {
        let futures: Vec<_> = calls.iter().map(|c| self.execute(c)).collect();
        futures::future::join_all(futures).await
    }
    
    /// Get tool declarations for the setup message.
    pub fn to_tool_declaration(&self) -> ToolDeclaration {
        ToolDeclaration {
            function_declarations: self.declarations.clone(),
        }
    }
}
```

**Usage pattern:**

```rust
let mut registry = FunctionRegistry::new();

registry.register(
    "get_weather",
    "Get current weather for a city",
    Some(serde_json::json!({
        "type": "object",
        "properties": {
            "city": { "type": "string", "description": "City name" }
        },
        "required": ["city"]
    })),
    |args| async move {
        let city = args["city"].as_str().ok_or("missing city")?;
        // Call weather API...
        Ok(serde_json::json!({ "temperature": 22, "condition": "sunny", "city": city }))
    },
);

// Add to session config
let config = SessionConfig::new(api_key)
    .add_tool(registry.to_tool_declaration());
```

### 3.8 ADK Bridge

**File: `src/agent/adk_bridge.rs`**

The ADK bridge translates between `gemini-live-rs` events and ADK’s `LiveRequestQueue` model.

```rust
pub struct AdkBridge {
    config: AdkConfig,
    client: reqwest::Client,
}

pub struct AdkConfig {
    /// ADK server endpoint (FastAPI backend)
    pub endpoint: url::Url,
    /// Agent name to invoke
    pub agent_name: String,
    /// Session ID for ADK state persistence
    pub session_id: Option<String>,
    /// Timeout for ADK requests
    pub timeout: Duration,
}

impl AdkBridge {
    pub fn new(config: AdkConfig) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(config.timeout)
                .build()
                .expect("HTTP client"),
            config,
        }
    }
    
    /// Dispatch a function call to ADK and return the response.
    pub async fn dispatch(&self, call: &FunctionCall) -> Result<FunctionResponse, AgentError> {
        // ADK expects LiveRequest format
        let request = serde_json::json!({
            "agent_name": self.config.agent_name,
            "session_id": self.config.session_id,
            "function_call": {
                "name": call.name,
                "args": call.args,
            }
        });
        
        let response = self.client
            .post(self.config.endpoint.join("/dispatch")?)
            .json(&request)
            .send()
            .await
            .map_err(|e| AgentError::AdkDispatch(e.to_string()))?;
        
        let result: serde_json::Value = response.json().await
            .map_err(|e| AgentError::AdkResponse(e.to_string()))?;
        
        Ok(FunctionResponse {
            name: call.name.clone(),
            response: result,
            id: call.id.clone(),
        })
    }
    
    /// Attach to a session — handles all tool calls via ADK automatically.
    pub fn attach(self, handle: &SessionHandle) -> tokio::task::JoinHandle<()> {
        let mut events = handle.subscribe();
        let command_tx = handle.command_tx.clone();
        
        tokio::spawn(async move {
            while let Ok(event) = events.recv().await {
                if let SessionEvent::ToolCall(calls) = event {
                    let mut responses = Vec::new();
                    for call in &calls {
                        match self.dispatch(call).await {
                            Ok(resp) => responses.push(resp),
                            Err(e) => {
                                responses.push(FunctionResponse {
                                    name: call.name.clone(),
                                    response: serde_json::json!({ "error": e.to_string() }),
                                    id: call.id.clone(),
                                });
                            }
                        }
                    }
                    let _ = command_tx.send(SessionCommand::SendToolResponse(responses)).await;
                }
            }
        })
    }
}
```

### 3.9 A2A Client

**File: `src/agent/a2a_client.rs`**

Implements the Agent2Agent protocol v0.3 for inter-agent communication:

```rust
pub struct A2AClient {
    config: A2AConfig,
    client: reqwest::Client,
    agent_card: Option<AgentCard>,
}

/// Agent Card as defined by the A2A specification.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub url: String,
    pub skills: Vec<AgentSkill>,
    pub supported_protocols: Vec<String>,
    pub authentication: Option<AuthScheme>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub input_modes: Vec<String>,
    pub output_modes: Vec<String>,
}

impl A2AClient {
    /// Discover a remote agent's capabilities.
    pub async fn discover(&mut self) -> Result<&AgentCard, AgentError> {
        let card_url = format!("{}/.well-known/agent.json", self.config.base_url);
        let card: AgentCard = self.client.get(&card_url)
            .send().await?
            .json().await?;
        self.agent_card = Some(card);
        Ok(self.agent_card.as_ref().unwrap())
    }
    
    /// Send a task to the remote agent (JSON-RPC 2.0).
    pub async fn send_task(&self, message: &str) -> Result<A2ATask, AgentError> {
        let card = self.agent_card.as_ref().ok_or(AgentError::NotDiscovered)?;
        
        let rpc_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": uuid::Uuid::new_v4().to_string(),
            "method": "message/send",
            "params": {
                "message": {
                    "role": "user",
                    "parts": [{ "text": message }],
                    "messageId": uuid::Uuid::new_v4().to_string()
                }
            }
        });
        
        let response = self.client.post(&card.url)
            .header("Content-Type", "application/a2a+json")
            .json(&rpc_request)
            .send().await?;
        
        let result: serde_json::Value = response.json().await?;
        // Parse task from result...
        Ok(A2ATask::from_json(&result)?)
    }
    
    /// Subscribe to task updates via SSE (for long-running tasks).
    pub async fn subscribe_task(&self, task_id: &str) -> Result<A2ATaskStream, AgentError> {
        let card = self.agent_card.as_ref().ok_or(AgentError::NotDiscovered)?;
        
        let rpc_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": uuid::Uuid::new_v4().to_string(),
            "method": "message/stream",
            "params": {
                "message": {
                    "role": "user",
                    "parts": [{ "text": format!("Continue task {}", task_id) }],
                    "messageId": uuid::Uuid::new_v4().to_string()
                }
            }
        });
        
        // Returns an SSE stream reader
        let response = self.client.post(&card.url)
            .header("Accept", "text/event-stream")
            .json(&rpc_request)
            .send().await?;
        
        Ok(A2ATaskStream::new(response))
    }
}
```

### 3.10 Telemetry Implementation

**File: `src/telemetry/mod.rs`**

**Modular initialization:**

```rust
pub struct TelemetryConfig {
    /// Enable OpenTelemetry tracing
    pub tracing_enabled: bool,
    /// OTLP endpoint for span export
    pub otlp_endpoint: Option<String>,
    /// Enable Prometheus metrics endpoint
    pub metrics_enabled: bool,
    /// Prometheus listen address
    pub metrics_addr: Option<SocketAddr>,
    /// Log level filter
    pub log_level: LogLevel,
    /// Log format (JSON for production, pretty for development)
    pub log_format: LogFormat,
}

impl TelemetryConfig {
    /// Initialize all telemetry subsystems based on configuration.
    pub fn init(&self) -> Result<TelemetryGuard, TelemetryError> {
        let mut guard = TelemetryGuard::default();
        
        #[cfg(feature = "tracing-support")]
        if self.tracing_enabled {
            // Set up tracing subscriber with OTLP exporter
            let subscriber = tracing_subscriber::fmt()
                .with_env_filter(self.log_level.to_filter())
                .json() // or .pretty() based on log_format
                .finish();
            tracing::subscriber::set_global_default(subscriber)?;
            guard.tracing = true;
        }
        
        #[cfg(feature = "metrics")]
        if self.metrics_enabled {
            if let Some(addr) = self.metrics_addr {
                let builder = metrics_exporter_prometheus::PrometheusBuilder::new();
                builder.with_http_listener(addr).install()?;
                guard.metrics = true;
            }
        }
        
        Ok(guard)
    }
}
```

**Span definitions (File: `src/telemetry/spans.rs`):**

```rust
#[cfg(feature = "tracing-support")]
use tracing::{instrument, info_span, Span};

/// Create a span for the entire session lifecycle.
pub fn session_span(session_id: &str) -> Span {
    info_span!("gemini.live.session", session_id = session_id)
}

/// Create a span for WebSocket connection.
pub fn connect_span(url: &str) -> Span {
    info_span!("gemini.live.connect", url = url)
}

/// Create a span for audio chunk transmission.
pub fn send_audio_span(chunk_size: usize, session_id: &str) -> Span {
    info_span!(
        "gemini.live.send_audio",
        chunk_size = chunk_size,
        session_id = session_id,
    )
}

/// Create a span for tool call execution.
pub fn tool_call_span(function_name: &str, session_id: &str) -> Span {
    info_span!(
        "gemini.live.tool_call",
        function_name = function_name,
        session_id = session_id,
    )
}
```

**Metric definitions (File: `src/telemetry/metrics.rs`):**

```rust
#[cfg(feature = "metrics")]
use metrics::{counter, gauge, histogram};

pub fn record_session_connected(session_id: &str) {
    counter!("gemini_live_connections_total").increment(1);
    gauge!("gemini_live_sessions_active").increment(1.0);
}

pub fn record_session_disconnected(session_id: &str) {
    gauge!("gemini_live_sessions_active").decrement(1.0);
}

pub fn record_audio_latency(latency_ms: f64) {
    histogram!("gemini_live_audio_latency_ms").record(latency_ms);
}

pub fn record_response_latency(latency_ms: f64) {
    histogram!("gemini_live_response_latency_ms").record(latency_ms);
}

pub fn record_jitter_depth(depth_ms: f64) {
    gauge!("gemini_live_jitter_buffer_depth_ms").set(depth_ms);
}

pub fn record_tool_call(function_name: &str, duration_ms: f64) {
    counter!("gemini_live_tool_calls_total", "function" => function_name.to_string()).increment(1);
    histogram!("gemini_live_tool_call_duration_ms", "function" => function_name.to_string()).record(duration_ms);
}

pub fn record_vad_event(event: &str) {
    counter!("gemini_live_vad_events_total", "event" => event.to_string()).increment(1);
}

pub fn record_reconnection() {
    counter!("gemini_live_reconnections_total").increment(1);
}
```

-----

## 4. Public API Surface

### 4.1 `lib.rs` — The Entry Point

```rust
//! # gemini-live-rs
//!
//! Ultra-low-latency Rust library for the Gemini Multimodal Live API.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use gemini_live_rs::prelude::*;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = SessionConfig::new("YOUR_API_KEY")
//!         .model(GeminiModel::Gemini2_5FlashPreview)
//!         .voice(Voice::Kore)
//!         .system_instruction("You are a helpful voice assistant.");
//!
//!     let session = connect(config, TransportConfig::default()).await?;
//!     session.wait_for_phase(SessionPhase::Active).await;
//!
//!     // Send text
//!     session.send_text("Hello, what can you help me with?").await?;
//!
//!     // Listen for events
//!     let mut events = session.subscribe();
//!     while let Ok(event) = events.recv().await {
//!         match event {
//!             SessionEvent::TextDelta(text) => print!("{}", text),
//!             SessionEvent::AudioData(samples) => { /* play audio */ },
//!             SessionEvent::ToolCall(calls) => { /* handle tools */ },
//!             SessionEvent::TurnComplete => println!("\n--- Turn complete ---"),
//!             SessionEvent::Disconnected(_) => break,
//!             _ => {},
//!         }
//!     }
//!     Ok(())
//! }
//! ```

pub mod protocol;
pub mod transport;
pub mod buffer;
pub mod vad;
pub mod session;
pub mod agent;
pub mod flow;
pub mod telemetry;

/// Convenient re-exports for common usage.
pub mod prelude {
    pub use crate::protocol::{
        SessionConfig, GeminiModel, Voice, AudioFormat, Modality,
        FunctionDeclaration, FunctionCall, FunctionResponse, ToolDeclaration,
        Content, Part,
    };
    pub use crate::transport::{connect, TransportConfig};
    pub use crate::session::{
        SessionHandle, SessionEvent, SessionCommand, SessionPhase, SessionError,
    };
    pub use crate::agent::FunctionRegistry;
    pub use crate::vad::{VoiceActivityDetector, VadConfig, VadEvent};
    pub use crate::buffer::{SpscRing, AudioJitterBuffer, JitterConfig};
    pub use crate::telemetry::TelemetryConfig;
}
```

### 4.2 Example: Simple Conversation

```rust
// examples/simple_conversation.rs
use gemini_live_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize telemetry
    TelemetryConfig {
        tracing_enabled: true,
        log_level: LogLevel::Info,
        log_format: LogFormat::Pretty,
        ..Default::default()
    }.init()?;

    let api_key = std::env::var("GEMINI_API_KEY")?;
    
    let config = SessionConfig::new(api_key)
        .model(GeminiModel::Gemini2_5FlashPreview)
        .voice(Voice::Aoede)
        .system_instruction("You are a friendly voice assistant. Keep responses concise.");

    let session = connect(config, TransportConfig::default()).await?;
    session.wait_for_phase(SessionPhase::Active).await;
    println!("Connected! Type messages or press Ctrl+C to quit.\n");

    // Event listener task
    let mut events = session.subscribe();
    let event_task = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                SessionEvent::TextDelta(t) => print!("{t}"),
                SessionEvent::TextComplete(t) => println!("\n[Complete]: {t}"),
                SessionEvent::InputTranscription(t) => println!("[You said]: {t}"),
                SessionEvent::TurnComplete => println!("\n---"),
                SessionEvent::Interrupted => println!("[Interrupted]"),
                SessionEvent::Disconnected(r) => {
                    println!("[Disconnected: {:?}]", r);
                    break;
                }
                _ => {}
            }
        }
    });

    // Simple text input loop
    let mut line = String::new();
    loop {
        line.clear();
        if std::io::stdin().read_line(&mut line)? == 0 { break; }
        let text = line.trim();
        if text.is_empty() { continue; }
        if text == "/quit" { break; }
        session.send_text(text).await?;
    }

    session.disconnect().await?;
    event_task.abort();
    Ok(())
}
```

### 4.3 Example: Function Calling Agent

```rust
// examples/function_calling_agent.rs
use gemini_live_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GEMINI_API_KEY")?;
    
    // Build function registry
    let mut registry = FunctionRegistry::new();
    
    registry.register(
        "get_stock_price",
        "Get the current stock price for a ticker symbol",
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "ticker": { "type": "string", "description": "Stock ticker (e.g., GOOGL)" }
            },
            "required": ["ticker"]
        })),
        |args| async move {
            let ticker = args["ticker"].as_str().ok_or("missing ticker")?;
            // In production: call a real stock API
            Ok(serde_json::json!({
                "ticker": ticker,
                "price": 178.52,
                "currency": "USD",
                "change": "+1.2%"
            }))
        },
    );
    
    registry.register(
        "place_order",
        "Place a buy or sell order for a stock",
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "ticker": { "type": "string" },
                "action": { "type": "string", "enum": ["buy", "sell"] },
                "quantity": { "type": "integer" }
            },
            "required": ["ticker", "action", "quantity"]
        })),
        |args| async move {
            let order_id = uuid::Uuid::new_v4();
            Ok(serde_json::json!({
                "order_id": order_id.to_string(),
                "status": "confirmed",
                "ticker": args["ticker"],
                "action": args["action"],
                "quantity": args["quantity"]
            }))
        },
    );

    let config = SessionConfig::new(api_key)
        .model(GeminiModel::Gemini2_0FlashLive)
        .voice(Voice::Puck)
        .system_instruction("You are a stock trading assistant. Use the available tools to look up prices and place orders. Always confirm with the user before placing orders.")
        .add_tool(registry.to_tool_declaration());

    let session = connect(config, TransportConfig::default()).await?;
    session.wait_for_phase(SessionPhase::Active).await;

    // Auto-dispatch tool calls through the registry
    let mut events = session.subscribe();
    let cmd_tx = session.command_tx.clone();
    
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                SessionEvent::ToolCall(calls) => {
                    let responses = registry.execute_all(&calls).await;
                    let _ = cmd_tx.send(SessionCommand::SendToolResponse(responses)).await;
                }
                SessionEvent::TextDelta(t) => print!("{t}"),
                SessionEvent::TurnComplete => println!("\n---"),
                _ => {}
            }
        }
    });

    // Send initial prompt
    session.send_text("What's the current price of GOOGL?").await?;
    
    // Keep alive
    tokio::signal::ctrl_c().await?;
    session.disconnect().await?;
    Ok(())
}
```

### 4.4 Example: A2A Multi-Agent Dispatch

```rust
// examples/a2a_dispatch.rs
use gemini_live_rs::prelude::*;
use gemini_live_rs::agent::a2a::{A2AClient, A2AConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GEMINI_API_KEY")?;

    // Discover remote specialist agents
    let mut weather_agent = A2AClient::new(A2AConfig {
        base_url: "https://weather-agent.example.com".parse()?,
        ..Default::default()
    });
    let weather_card = weather_agent.discover().await?;
    println!("Discovered agent: {} - {}", weather_card.name, weather_card.description);

    let mut calendar_agent = A2AClient::new(A2AConfig {
        base_url: "https://calendar-agent.example.com".parse()?,
        ..Default::default()
    });
    let calendar_card = calendar_agent.discover().await?;

    // Build registry that delegates to remote agents via A2A
    let mut registry = FunctionRegistry::new();
    
    registry.register(
        "check_weather",
        "Check weather at a location (delegates to specialist weather agent)",
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "location": { "type": "string" }
            },
            "required": ["location"]
        })),
        move |args| {
            let agent = weather_agent.clone();
            async move {
                let location = args["location"].as_str().ok_or("missing location")?;
                let task = agent.send_task(&format!("What's the weather in {}?", location)).await
                    .map_err(|e| e.to_string())?;
                Ok(task.result)
            }
        },
    );

    registry.register(
        "schedule_meeting",
        "Schedule a meeting (delegates to specialist calendar agent)",
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "time": { "type": "string" },
                "attendees": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["title", "time"]
        })),
        move |args| {
            let agent = calendar_agent.clone();
            async move {
                let task = agent.send_task(&serde_json::to_string(&args)?).await
                    .map_err(|e| e.to_string())?;
                Ok(task.result)
            }
        },
    );

    let config = SessionConfig::new(api_key)
        .voice(Voice::Kore)
        .system_instruction(
            "You are a personal assistant that can check weather and schedule meetings. \
             Use the available tools to help the user."
        )
        .add_tool(registry.to_tool_declaration());

    let session = connect(config, TransportConfig::default()).await?;
    // ... (same event loop pattern as above)
    
    Ok(())
}
```

-----

## 5. Testing Strategy

### 5.1 Unit Tests

|Module         |Test Coverage Target|Key Tests                                                            |
|---------------|--------------------|---------------------------------------------------------------------|
|`protocol`     |100%                |Serde round-trips for every message type; parse real Gemini responses|
|`buffer`       |95%                 |Write/read, wraparound, overflow, concurrent access (proptest)       |
|`vad`          |90%                 |Silence → speech → silence with synthetic audio; noise adaptation    |
|`session/state`|100%                |All valid transitions pass; all invalid transitions fail             |
|`agent`        |85%                 |Function dispatch, parallel execution, error handling                |

### 5.2 Property-Based Tests (proptest)

```rust
// Ring buffer: data written is data read, in order, for any input
proptest! {
    #[test]
    fn ring_preserves_data(data in prop::collection::vec(any::<i16>(), 0..10000)) {
        let ring = SpscRing::<i16>::new(16384);
        let written = ring.write(&data);
        let mut out = vec![0i16; written];
        let read = ring.read(&mut out);
        prop_assert_eq!(read, written);
        prop_assert_eq!(&out[..read], &data[..written]);
    }
}
```

### 5.3 Integration Tests

```rust
// Requires GEMINI_API_KEY environment variable
#[tokio::test]
#[ignore] // Run with: cargo test -- --ignored
async fn live_text_conversation() {
    let api_key = std::env::var("GEMINI_API_KEY").unwrap();
    let config = SessionConfig::new(api_key)
        .text_only()
        .system_instruction("Reply with exactly one word.");
    
    let session = connect(config, TransportConfig::default()).await.unwrap();
    session.wait_for_phase(SessionPhase::Active).await;
    session.send_text("Say hello").await.unwrap();
    
    let mut events = session.subscribe();
    let mut got_text = false;
    let timeout = tokio::time::sleep(Duration::from_secs(10));
    tokio::pin!(timeout);
    
    loop {
        tokio::select! {
            event = events.recv() => {
                if let Ok(SessionEvent::TextComplete(text)) = event {
                    assert!(!text.is_empty());
                    got_text = true;
                    break;
                }
            }
            _ = &mut timeout => panic!("Timed out waiting for response"),
        }
    }
    assert!(got_text);
    session.disconnect().await.unwrap();
}
```

### 5.4 Benchmarks

```rust
// benches/buffer_throughput.rs
use criterion::{criterion_group, criterion_main, Criterion, black_box};
use gemini_live_rs::buffer::SpscRing;

fn bench_spsc(c: &mut Criterion) {
    let ring = SpscRing::<i16>::new(65536);
    let data = vec![42i16; 1600]; // 100ms @ 16kHz
    
    c.bench_function("spsc_write_100ms", |b| {
        b.iter(|| {
            ring.write(black_box(&data));
        })
    });
    
    c.bench_function("spsc_read_100ms", |b| {
        ring.write(&data);
        let mut out = vec![0i16; 1600];
        b.iter(|| {
            ring.write(&data);
            ring.read(black_box(&mut out));
        })
    });
}

criterion_group!(benches, bench_spsc);
criterion_main!(benches);
```

### 5.5 Fuzzing

```rust
// fuzz/fuzz_targets/parse_server_message.rs
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        // Should never panic, even on garbage input
        let _ = serde_json::from_str::<gemini_live_rs::protocol::ServerMessage>(text);
    }
});
```

-----

## 6. Performance Targets

|Metric                        |Target                   |Measurement                  |
|------------------------------|-------------------------|-----------------------------|
|Ring buffer write latency     |< 100ns per 1600 samples |Criterion benchmark          |
|Ring buffer throughput        |> 10M samples/sec        |Criterion benchmark          |
|VAD processing time           |< 500μs per 30ms frame   |Criterion benchmark          |
|WS message serialization      |< 50μs per audio chunk   |Criterion benchmark          |
|Memory per session            |< 5MB                    |RSS measurement under load   |
|Sessions per core             |> 500 concurrent         |Load test with mock WS server|
|Audio capture → WS send       |< 5ms end-to-end         |Instrumented timing          |
|End-of-speech → first response|< 300ms (network + model)|OTel span measurement        |
|Barge-in → silence            |< 10ms (local flush)     |Instrumented timing          |

-----

## 7. Dependencies Quick Reference

```toml
# Minimal (core only):
gemini-live-rs = "0.1"

# With all features:
gemini-live-rs = { version = "0.1", features = ["opus", "vad", "agent-adk", "agent-a2a", "tracing-support", "metrics"] }

# Runtime requirement:
tokio = { version = "1", features = ["full"] }
```

-----

## 7.5 Implementation Priorities — Driven by Real Production Failures

This section maps specific classes of production failures observed in Pipecat and LiveKit to the components that prevent them in our implementation. Every implementation priority has a “why” grounded in real user pain.

### Priority 1: Jitter Buffer (Prevents: Pipecat #3222, #2551)

Pipecat has no jitter buffer. Users report chunks arriving 600ms+ apart, causing choppy audio on SIP/Twilio calls while server recordings are clean. This is the most straightforward audio quality issue a framework can solve.

**Implementation requirement**: Adaptive jitter buffer on the receive path with:

- EWMA-based depth estimation (RFC 6298 algorithm)
- Instant `flush()` on barge-in (zero-latency transition from playing to silence)
- Configurable min/max depth for different network profiles
- Underrun handling with silence fill + metric recording

### Priority 2: Atomic Barge-in (Prevents: Pipecat #2460/#3191/#1661, LiveKit #3418)

Both frameworks have multi-step interruption handling that creates race conditions. Pipecat propagates `CancelFrame` through the pipeline; LiveKit’s agent state desynchronizes from TTS state.

**Implementation requirement**: Single atomic operation:

1. Jitter buffer flush (instant silence) — happens locally, no network round-trip
1. Send `activityStart` to Gemini — informs server of user speech
1. FSM transition `ModelSpeaking → Interrupted` — validated before executing
1. Server confirms with `interrupted=true` in `serverContent` — secondary confirmation

The critical insight: step 1 happens before step 2’s round-trip completes, eliminating the >200ms barge-in delay both frameworks suffer from.

### Priority 3: Clean Session Lifecycle (Prevents: Pipecat #953/#1809/#3179/#1338/#1912)

Pipecat’s pipeline shutdown is the single largest source of production instability — five separate issues documenting deadlocks, memory leaks, stuck frames, and infinite loops.

**Implementation requirement**: Actor-per-session with Tokio `CancellationToken`:

- `SessionHandle::close()` → cancels all child tasks → memory reclaimed by `Drop`
- No frame propagation chain that can block
- No shared mutable state between sessions
- No global audio mixer that can loop infinitely
- `AbortOnDrop` wrapper ensures tasks never leak, even on panic

### Priority 4: First-Class Gemini Live Protocol (Prevents: Pipecat #908/#1606/#1674, LiveKit #4441/#4497/#2174/#4414)

Both frameworks wrap Gemini in abstractions designed for other providers. This causes: function calling results dropped (missing `name` field), VAD params unexposed, session resume unsupported, spurious VAD cancelling tools, system messages stripped, protocol text spoken aloud, and model names hard-coded.

**Implementation requirement**: One-to-one Rust type mapping to the wire protocol:

- Every `clientContent`, `realtimeInput`, `toolResponse`, `setupMessage` is a Rust struct with `#[serde(rename_all = "camelCase")]`
- All Gemini Live config options are fields on `SessionConfig` — nothing hidden
- `SessionResumption` handle preserved across reconnections
- `GoAway` signal triggers graceful shutdown with time-left awareness
- Async function calling with `NON_BLOCKING` behavior (LiveKit #2367)

### Priority 5: Validated State Machine (Prevents: LiveKit #3418/#3427/#4039)

LiveKit’s agent goes silent while showing “speaking” because state and audio pipeline desynchronize. Turn detection can’t be tuned independently for thinking vs speaking. False-interruption resume regresses between versions.

**Implementation requirement**: FSM with `can_transition_to()` validation:

- `matches!` macro ensures only valid transitions execute
- Phase observers via `tokio::sync::watch` detect stuck states
- Per-phase interruption sensitivity (configurable thresholds for `Thinking` vs `Speaking`)
- Server VAD as authoritative turn detection — eliminates the entire class of client-side turn detection problems

### Priority 6: Built-in Observability (Prevents: LiveKit #2260/#4054/#4055, Pipecat implicit)

LiveKit’s most-upvoted issue is native OTel. When they add recording, voice quality degrades 2x. STT metrics can’t be correlated to turns.

**Implementation requirement**: Three pillars, all feature-gated for zero-cost when disabled:

- OTel spans with `session_id` on every span (solves #4054 correlation)
- Structured logging with per-phase verbosity policy
- Prometheus metrics on separate Tokio tasks (never contends with audio path, solves #4055)

### Priority 7: Reconnection with Session Resume (Prevents: LiveKit #3145/#2341, Pipecat #1674)

LiveKit’s auto-reconnect fails silently in production after 30-minute session expiry. Pipecat doesn’t support session resume at all.

**Implementation requirement**:

- `SessionResumption` handle stored after initial setup
- Exponential backoff with jitter on reconnection
- GoAway signal awareness — initiate reconnection before server forces disconnect
- Resume handle sent in setup message — server restores context without re-sending history

-----

## 8. Migration Guide from Pipecat / LiveKit

### 8.1 From Pipecat

**What you’re escaping** (with issue references):

- Pipeline shutdown deadlocks (#953, #1809, #3179, #1912)
- Interruption handling broken across FastAPI WS, Twilio, Daily transports (#2460, #3191, #1661)
- Context lost after interruptions — bot repeats itself (#2791, #1591)
- Function calling results silently dropped with Gemini Live (#908)
- No jitter buffer — audio quality degrades on real networks (#3222, #2551)
- Gemini-specific: VAD config, session resume, context compression all inaccessible (#1606, #1674)

**Code migration:**

```python
# Pipecat (Python) — cascaded pipeline that fights Gemini's unified model
pipeline = Pipeline([
    transport.input(),       # These stages don't exist in Gemini Live —
    stt,                     # the model does STT, reasoning, and TTS
    context_aggregator.user(),  # Context aggregation is manual, error-prone (#147)
    llm,
    tts,
    transport.output(),
    audiobuffer,             # AudioBuffer blocks shutdown (#2609)
    context_aggregator.assistant(),  # Assistant messages silently dropped (#1591)
])
```

```rust
// gemini-live-rs (Rust) — no pipeline needed
// Gemini Live API IS the pipeline (speech-to-speech)
let config = SessionConfig::new(api_key)
    .model(GeminiModel::Gemini2_5FlashPreview)  // Native audio model
    .voice(Voice::Kore)
    .vad_config(VadConfig {                      // Full access to all params (#1606)
        start_sensitivity: Sensitivity::High,
        end_sensitivity: Sensitivity::High,
        silence_duration_ms: 300,
    })
    .session_resumption(true);                   // First-class support (#1674)

let session = connect(config, TransportConfig::default()).await?;
// Send audio, receive audio — Gemini handles STT, reasoning, and TTS internally
// Interruptions are atomic. Context is managed by the model. Shutdown is instant.
```

**What you lose**: Pipecat’s multi-provider flexibility. If you need Deepgram STT + OpenAI LLM + Cartesia TTS as separate components, Pipecat’s pipeline model is correct for that use case. We are the right choice specifically when your LLM is Gemini Live.

### 8.2 From LiveKit

**What you’re escaping** (with issue references):

- Agent becomes silent but status shows ‘speaking’ (#3418)
- Spurious Vertex AI VAD cancels running tools with no opt-out (#4441)
- Turn detection can’t be tuned for thinking vs speaking states (#3427)
- `update_chat_ctx()` silently strips system messages (#4497)
- Agent speaks literal protocol text “tools_output” (#2174)
- No native OTel — most requested feature at 57+ upvotes (#2260)
- Recording/observability degrades voice quality (#4055)
- Auto-reconnect unreliable in production (#3145)

**Code migration:**

```python
# LiveKit (Python) — SFU + agent abstraction that conflicts with Gemini's session model
session = AgentSession(
    vad=silero.VAD.load(),           # Client VAD → but server VAD overrides + conflicts (#4441)
    stt=deepgram.STT(),              # Separate STT — redundant with Gemini's native audio
    llm=google.realtime.RealtimeModel(  # Wrapped in abstraction that strips system msgs (#4497)
        model="gemini-live-2.5-flash",
    ),
    tts=cartesia.TTS(),              # Separate TTS — redundant with Gemini's native output
    turn_detection=MultilingualModel(),  # Text-only, no prosody (#3094)
    resume_false_interruption=True,      # Broken since 1.3.3 (#4039)
)
agent.start(ctx.room, participant)
```

```rust
// gemini-live-rs (Rust) — no separate VAD/STT/LLM/TTS
let config = SessionConfig::new(api_key)
    .model(GeminiModel::Gemini2_5FlashPreview)
    .voice(Voice::Aoede)
    .system_instruction("You are a helpful assistant.")
    .tool_config(ToolConfig::auto())    // No "tools_output" spoken (#2174)
    .enable_otel(true);                 // Built-in, not an afterthought (#2260)

let session = connect(config, TransportConfig::default()).await?;
// Server VAD is authoritative — no conflict with client interruption model
// System instructions are sent as setup message — never stripped
// Observability runs on separate tasks — never degrades audio (#4055)
```

**What you lose**: LiveKit’s multi-participant SFU, WebRTC transport, client SDKs (iOS/Android/Web/Flutter), and SIP gateway integration. If your use case involves conferencing, phone bridging, or multi-user rooms, LiveKit’s infrastructure is genuinely best-in-class. We are the right choice for 1:1 voice AI interactions that need to talk directly to Gemini with minimal latency and maximum reliability.

**Key mental model shift**: You’re not orchestrating a pipeline of independent services. You’re not routing through an SFU. You’re having a conversation with a single, unified multimodal model. The library manages the WebSocket connection, audio buffering, and event dispatching — the model does the rest.

-----

*This implementation guide will be updated as development progresses. File issues or PRs for corrections and improvements.*