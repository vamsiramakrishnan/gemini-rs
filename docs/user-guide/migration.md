# Migration Guide

## 1.x → 2.0

The 2.0 release tightens the L0 (`gemini-genai-rs`) surface. Nothing changes
in how a session behaves; what changes is how a few things are named and
constructed. Everything below is mechanical.

| Area | 1.x | 2.0 |
|------|-----|-----|
| Model type | `GeminiModel` enum (`Gemini2_0FlashLive`, `GeminiLive2_5FlashNativeAudio`, `Gemini2_0Flash`, `Custom(s)`) | `ModelId` string newtype: `ModelId::new("…")`, `"…".into()`, `ModelId::from_static("…")`; constants `ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO` (Vertex GA), `ModelId::FLASH_2_5_NATIVE_AUDIO_LATEST` (Google AI alias), `ModelId::FLASH_LATEST` (text). `GeminiModel::Custom(s)` → `ModelId::new(s)` |
| Choosing a model | `.model(GeminiModel::…)` was effectively required | `SessionConfig.model` is `Option<ModelId>`; leave it unset and connect resolves `ModelId::live_default(vertex)` (honours `GEMINI_LIVE_MODEL`, then `GEMINI_MODEL`; prefixes bare names with `models/`). Text agents read `GEMINI_TEXT_MODEL`, then `GEMINI_MODEL`. `Live::builder().model(..)` and `AgentBuilder::model(..)` take a `ModelId` |
| Connecting | `connect(config, TransportConfig::default())` | `connect(config)` |
| Connecting with options | `connect_with(config, tc, transport, codec)` / `ConnectBuilder::new(config).build()` | `ConnectBuilder::new(config).transport_config(tc).transport(t).codec(c).connect().await` (`connect_with` is crate-private) |
| Shortcuts | `quick_connect(key, model)`, `quick_connect_vertex(..)` | `connect(SessionConfig::new(key)).await` |
| REST client → Live | `Client::live(model)` returning a `LiveSessionBuilder` | `Client::live(Option<ModelId>)` returns a `ConnectBuilder`: `client.live(None).connect().await` (tune with `.configure(..)`) |
| Vertex credentials | `String` access token only | `AccessToken` — `Static(String)` or `Dynamic(closure)`; `SessionConfig::from_vertex(..)`, `ApiEndpoint::vertex(..)` and `Live::builder().connect_vertex(..)` accept `Into<AccessToken>` (a `&str`/`String` still works); `ApiEndpoint::vertex_refreshing(project, location, \|\| token())` and `Client::from_vertex_refreshable(..)` refresh on every (re)connect |
| Token access | `SessionConfig::bearer_token() -> Option<&str>` | `-> Option<String>`, read on every connection attempt (`None` on Google AI); `ApiEndpoint`'s `Debug` output redacts secrets |
| Error event | `SessionEvent::Error(String)` | `SessionEvent::Error(SessionError)` — print with `{e}` or match `Codec(..)`, `WebSocket(..)`, `Timeout { phase, .. }`, `SetupFailed(..)`; `SessionError` gained `Codec(CodecError)` |
| GoAway | `SessionEvent::GoAway(Option<String>)`, `GoAwayPayload.time_left: Option<String>` | `SessionEvent::GoAway(Option<Duration>)`, `GoAwayPayload.time_left: Option<Duration>` |
| Audio / video payloads | `SessionCommand::SendAudio(Vec<u8>)` / `SendVideo(Vec<u8>)`; `SessionWriter::send_audio(Vec<u8>)` | `Bytes` throughout: `SessionCommand::SendAudio(Bytes)`, `SessionWriter::send_audio(Bytes)`, `SessionHandle::send_audio(impl Into<Bytes>)`; L1 `LiveHandle`/`AgentSession::send_audio`/`send_video` take `impl Into<Bytes>` (a `Vec<u8>` still works), `InputEvent::Audio` carries `Bytes` |
| SPSC ring | `SpscRing::new(cap)` returned one shared, `Sync` object with `write(&self)` / `read(&self)`; capacity rounded up to a power of two | `SpscRing::channel(cap)` returns `(SpscProducer, SpscConsumer)` — each `Send`, neither `Sync`, so one-producer/one-consumer is enforced by the type system (backed by `rtrb`); exact capacity; `is_abandoned()` on both halves. No `unsafe` remains in the workspace |
| Transcription setters | `enable_input_transcription()`, `enable_output_transcription()` | `input_transcription(true)`, `output_transcription(true)` |
| Thoughts | `include_thoughts()` | `include_thoughts(true)` (L2 `Live::builder().include_thoughts()` is unchanged) |
| Resumption setters | `session_resumption(None)`, `session_resumption(Some(h))` | `session_resumption()`, `resume_from(h)` |
| Audio format setters | `input_audio(format, rate)`, `output_audio(format, rate)`, fields `output_audio_format`, `input_sample_rate`, `output_sample_rate` | Removed — `input_audio_format(fmt)` remains; rates are fixed by the API (16 kHz in, 24 kHz out) |
| Capability checks | `supports_async_tools()` | `supports_async_tools()` plus new `supports_thinking()`; `Voice` implements `Display` |
| `SessionHandle` fields | public `command_tx`, `state` | private; `SessionHandle::resume_handle()` added; `event_sender()` is for runtimes only |
| Prelude: wire envelopes | `SetupPayload`, `RealtimeInputPayload`, `ServerMessageWrapper`, `MediaChunk`, `ActivityStart`/`ActivityEnd`, `GoAwayPayload`, `TranscriptionPayload`, … in `prelude::*` | `gemini_genai_rs::protocol::messages::*` (`ServerMessage` stays in the prelude) |
| Prelude: REST & tooling | `Client`, `File`/`FileSource`/`FileState`, `TaskType`, `Candidate`, `ModelInfo`, `BatchJob`, `TelemetryConfig`, `FileWireRecorder`, `MemoryWireRecorder`, `WireRecorder`, `WireEntry`, `WireDirection`, `read_wire_log`, `ReplayTransport`, `ReplayControl` in `prelude::*` | `gemini_genai_rs::Client`, the REST modules, `gemini_genai_rs::telemetry::TelemetryConfig`, `gemini_genai_rs::transport::…` for recording/replay |
| Prelude additions | — | `AccessToken`, `ModelId`, `TungsteniteError`, `VadState`, `BufferState` |
| Removed types | `Platform` enum, `ToolDeclaration` alias | Use `ApiEndpoint` (host/version) and `FunctionDeclaration` |
| Error types | `TungsteniteError::WebSocket(tungstenite::Error)`; REST errors `Auth(String)` | `TungsteniteError::WebSocket` boxes its source (no `tungstenite::Error` in the public API); `GenerateError`, `TokensError`, … carry `Auth(AuthError)`; `FilesError` gained `Decode(String)` |
| L1 flow names | `flow::ToolPolicy` (the set of tools a flow reasons about; collided with `tool::ToolPolicy`), `CompiledFlow::tool_policy()`; `flow::Mode` (deprecated) / root `FlowMode`; `flow::run(agent, mode)` / root `run_on_enter`; root `render_ground` | `flow::ToolSurface`, `CompiledFlow::tool_surface()`; one name `Enforcement` (root, `flow`, L2 prelude); `flow::on_enter(agent, mode)` (root `on_enter`); `render_ground` lives in `flow` only. Root and L2 prelude `ToolPolicy` now means `tool::ToolPolicy` (timeout/cache/confirm) |
| Orchestration names | `orchestration::Mode`, `orchestration::call` (root/L2 aliases `AgentMode`, `call_agent`) | Defined as `orchestration::AgentMode`, `orchestration::call_agent` — same names everywhere |
| "Why is it blocked?" | `LiveHandle::why_blocked()`, `FlowMonitor::why_blocked(&state)` (aliases) | `LiveHandle::explain()`, `FlowMonitor::explain(&state)` |
| Blocking vs concurrent | `live::callbacks::CallbackMode` (callbacks) and `live::reactor::EffectMode` (effects) | One `live::ExecutionMode { Blocking, Concurrent }` used by both |
| Closure aliases | `live::phase::StateGuard` / `workflow::GuardFn`; `live::phase::PhaseHook`; `live::BoxFuture`; private `orchestration::FetchFn` / `extract::FieldFetchFn` / `workflow::FunctionFn` | `gemini_adk_rs::StatePredicate`; `live::SessionHook` (also used by temporal patterns); `gemini_adk_rs::BoxFuture` (re-exported from `live`); `gemini_adk_rs::AsyncSourceFn<In = State>` (`AsyncSourceFn<Value>` for extraction-kit field resolvers) |
| Phase history record | `live::phase::PhaseTransition` | `live::TransitionRecord` (`Transition` remains the declared edge) |
| Wire session phase callback | `EventCallbacks::on_phase` / `PhaseCallback`; L2 `Live::on_phase(..)` | `on_session_phase` / `SessionPhaseCallback`; L2 `Live::on_session_phase(..)` (it is the transport phase, not the `PhaseMachine`) |
| Registry verbs | `WatcherRegistry::add`, `TemporalRegistry::add` | `register` (matching `ComputedRegistry::register`) |
| Toolset | `Toolset::get_tools()` | `Toolset::tools()` |
| Builder verbs | `Runner::{with_middleware, with_plugin, with_state}`; `LiveSessionBuilder::with_state`; L2 `Live::with_state` | `Runner::{middleware, plugin, state}`; `LiveSessionBuilder::state`; L2 `Live::state` |
| Text runner | `text_runner::InMemoryRunner` | `text_runner::TextRunner` |
| Removed modules/types | `gemini_adk_rs::callback` (`BeforeToolCallback`, `AfterToolCallback`, `BeforeToolResult`, `ToolCallResult` — unreferenced); public `agents::generated` (transpiler shadow types) | Deleted / crate-private. Use `Middleware` hooks (`before_tool`/`after_tool`) instead of the callback aliases |
| State reads | `State::get<T>` returns `None` for a present-but-mistyped value | `get`/`get_key` unchanged (lenient); new `State::try_get<T>` / `try_get_key` return `Result<Option<T>, StateError>` with `StateError::WrongType { key, source }`. `ReadOnlyPrefixedState` (the type of `state.derived()`) is exported from the crate root |
| Configuration errors | `ComputedRegistry::register` panicked on a dependency cycle; `Flow::validate` → `Result<(), Vec<String>>`, `FlowBuilder::build` → `Result<Flow, Vec<String>>`, `PhaseMachine::validate` / `ComputedRegistry::validate` → `Result<(), String>` | `ComputedRegistry::register` → `Result<(), ConfigError>` (never panics; a rejected registration leaves the registry unchanged — L2 `Live::computed(..)` defers the error to `connect`); all three `validate`s and `FlowBuilder::build` return `error::ConfigError { issues: Vec<String> }` (`Display` joins with `"; "`; `From<ConfigError> for AgentError`) |
| Session persistence errors | `SessionPersistence::{save, load, delete}` → `Result<_, Box<dyn Error + Send + Sync>>` | `Result<_, PersistenceError>` (`Io`, `Serde`, `NotFound`, `Backend(String)`) |
| Combinator middleware | `with_middleware_chain` on `LoopTextAgent`, `FallbackTextAgent`, `RouteTextAgent` only | Also on `Sequential`, `Parallel`, `Race`, `Timeout`, `MapOver`, `Dispatch`, `Join` text agents (`AgentStarted`/`AgentCompleted`, `LoopIteration`, `Timeout` `on_event`s); `TapTextAgent` documents why it has none |

Toolchain: the workspace is Rust edition 2024 with MSRV 1.93. `gemini-genai-rs`
default features are `["live", "tls-native"]` (`tls-rustls` is the alternative;
`vad`, `vad-wavekat`, `tracing-subscriber`, and the REST features are opt-in;
there is no `opus` feature). `gemini-adk-rs` no longer gates tracing behind
`tracing-support`. The OTel endpoint is configured via
`TelemetryConfig.otel_endpoint` / `TelemetrySetup::with_otlp(..)`, not an
environment variable.

## L0 -> L1 -> L2

This guide shows the same voice agent implemented at all three layers,
so you can see what each layer adds and decide where to build.

## Why Migrate?

Each layer removes a category of boilerplate:

| What you write | L0 (gemini-genai-rs) | L1 (gemini-adk-rs) | L2 (gemini-adk-fluent-rs) |
|----------------|:---:|:---:|:---:|
| WebSocket connection | Manual | Manual | One line |
| Event loop (`select!`) | Manual | Automatic | Automatic |
| Tool dispatch + response | Manual | Automatic | Automatic |
| State management | None | Built-in | Built-in |
| Phase transitions | Manual | PhaseMachine | `.phase()` builder |
| Turn extraction | None | TurnExtractor | `.extract_turns::<T>()` |
| Telemetry | None | SessionTelemetry | Auto-collected |
| Instruction updates | Manual | instruction_template | `.instruction_template()` |

The tradeoff is control. L0 gives you total control over every message. L2
handles the common patterns automatically but gives you less room to
customize the event processing loop itself.

## The L2 prelude: kernel + submodule map

`gemini_adk_fluent_rs::prelude` is a **kernel**, not an everything-glob. It
re-exports the ~40 types a typical application touches; everything else lives in
a focused, discoverable submodule. Start with the prelude and reach for a
submodule when the compiler says a name isn't found.

**In the kernel `prelude`:**

- Builders & composition: `AgentBuilder`/`Agent`, the `S·C·T·P·M·A·E·G·Ctx`
  algebra, operators (`>> | * /`) and patterns (`until`, `review_loop`,
  `fan_out_merge`, `supervised`), `Live`.
- State: `State`, `StateKey`.
- Flow (core): `Flow`, `Guard`, `FlowMonitor`, `Enforcement`, `Verdict`.
- Tools (core): `SimpleTool`, `TypedTool`, `ToolFunction`, `ToolDispatcher`,
  `ToolPolicy` (the per-tool timeout/cache/confirm policy), `#[tool]`,
  `Extract`, `Frame`.
- LLM (core): `BaseLlm`, `GeminiLlm`.
- Errors: `AgentError`, `AgentResult`, `ConfigError`, `ToolError`.
- Callback contexts: `CallbackContext`, `ToolContext`.
- Common Live types: `LiveHandle`, `EventCallbacks`, `SteeringMode`,
  `ContextDelivery`, `RepairConfig`, `SessionPersistence`, `PersistenceError`, `FsPersistence`,
  `MemoryPersistence`, `TurnExtractor`, `ExtractionTrigger`, `LlmExtractor`,
  `SoftTurnDetector`, `TranscriptBuffer`, `TranscriptTurn`.
- Text-agent combinators (`LlmTextAgent`, `SequentialTextAgent`, …).
- Build-time validation: `check_contracts`, `ContractViolation`, `diagnose`,
  `infer_data_flow`, `AgentHarness`, `DataFlowEdge`.
- The L0 wire prelude (`ModelId`, `AccessToken`, `Voice`, `Content`, `Part`, `Role`, …).

**Moved to submodules** (import the named module):

| Symbol(s) | Home |
|-----------|------|
| Full Live control plane: `LiveEvent`, `RuntimeContract`, `FieldPromotion`, `DeferredWriter`, `PendingContext`, `NeedsFulfillment`, `RepairAction`, `SessionSnapshot`, `LiveSessionBuilder`, `ExecutionMode`, `ToolExecutionMode`, the `*Contract` types, … | `gemini_adk_fluent_rs::live` |
| Text-agent runtime internals | `gemini_adk_fluent_rs::text` |
| `Toolset`, `StaticToolset`, `ConfirmationProvider`, `Recognizer`, `RecordExtractor`, `FrameSpec`, `SlotSpec`, … | `gemini_adk_fluent_rs::tools` |
| `SlotEvidence`, prefix-scope helpers | `gemini_adk_fluent_rs::state` |
| `CompiledFlow`, `StepAction`, `Violation`, `FlowExplanation`, `ToolSurface`, `on_enter`, `render_ground`, … | `gemini_adk_fluent_rs::flow` |
| `AgentTrait` (L1 `Agent` trait), `call_agent`, `AgentMode`, `provenance`, `Resolver`, `agent_session::*` | `gemini_adk_fluent_rs::agents` |
| `LlmRequest`, `LlmResponse`, `GeminiLlmParams`, `LlmRegistry` | `gemini_adk_fluent_rs::llm` |
| `Conversation`, `ConversationSpec`, `CompiledConversation`, `FlowStack`, … | `gemini_adk_fluent_rs::conversation` |
| `A2AServer`, `RemoteAgent`, `SkillDeclaration` | `gemini_adk_fluent_rs::a2a` |
| `Scenario`, `Sim`, `SimStep` | `gemini_adk_fluent_rs::simulation` |
| `Motif`, `CommitPolicy`, `Policy` | `gemini_adk_fluent_rs::{motifs, policy}` |
| Raw L0 wire types | `gemini_adk_fluent_rs::wire` |

> The L1 `Agent` *trait* is exposed as `AgentTrait` (in both `prelude` and
> `agents`) to avoid colliding with the L2 `Agent` builder alias.

## 0.8 feature changes (slim defaults)

`gemini-genai-rs` default features contracted to `["live", "tls-native"]`:

- **ML VAD is opt-in.** The `wavekat` VAD model is no longer compiled by
  default. Enable `vad-wavekat` (available as a passthrough feature on
  `gemini-adk-rs` and `gemini-adk-fluent-rs` too). The lightweight energy VAD
  (`vad`) is still enabled by `gemini-adk-rs`.
- **TLS backend is selectable.** `tls-native` (default) or `tls-rustls`; both
  the WebSocket transport and the optional REST client follow the choice. To go
  rustls: `default-features = false, features = ["live", "tls-rustls"]`.
- **Tracing facade vs subscriber.** The `tracing` facade is always compiled
  (spans/events are no-ops without a subscriber). `TelemetryConfig::init`'s
  console-logging machinery now sits behind the `tracing-subscriber` feature.
  The old `tracing-support` feature (on `gemini-genai-rs` and `gemini-adk-rs`)
  is a no-op kept only so existing manifests resolve — tracing is unconditional.
- **No more `tokio/full`.** The published crates declare only the tokio
  features they use; applications control their own tokio feature set.

## L0: Wire Protocol

At L0, you work directly with `SessionHandle`, `SessionEvent`, and
`SessionCommand`. You write your own event loop, dispatch tools manually,
and manage all state yourself.

Here is a weather assistant with one tool:

```rust,ignore
use gemini_genai_rs::prelude::*;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Build session config with tool declaration
    // (no `.model(..)`: connect resolves the platform's default Live model)
    let config = SessionConfig::from_endpoint(
        ApiEndpoint::google_ai(std::env::var("GEMINI_API_KEY")?)
    )
        .system_instruction("You are a weather assistant. Use get_weather for queries.")
        .add_tool(Tool {
            function_declarations: Some(vec![FunctionDeclaration {
                name: "get_weather".into(),
                description: "Get current weather for a city".into(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "city": { "type": "string", "description": "City name" }
                    },
                    "required": ["city"]
                })),
                behavior: None,
            }]),
            ..Default::default()
        });

    // 2. Connect
    let handle = connect(config).await?;
    handle.wait_for_phase(SessionPhase::Active).await;

    // 3. Subscribe to events
    let mut events = handle.subscribe();

    // 4. Send a question
    handle.send_text("What's the weather in Tokyo?").await?;

    // 5. Manual event loop
    while let Some(event) = recv_event(&mut events).await {
        match event {
            SessionEvent::TextDelta(text) => {
                print!("{text}");
            }
            SessionEvent::TurnComplete => {
                println!();
            }
            SessionEvent::ToolCall(calls) => {
                // Manual tool dispatch
                let mut responses = Vec::new();
                for call in calls {
                    let result = match call.name.as_str() {
                        "get_weather" => {
                            let city = call.args.get("city")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            json!({ "city": city, "temp_c": 22, "condition": "sunny" })
                        }
                        _ => json!({ "error": "unknown tool" }),
                    };
                    responses.push(FunctionResponse {
                        name: call.name.clone(),
                        id: call.id.clone(),
                        response: result,
                        scheduling: None,
                    });
                }
                // Manual response send
                handle.send_tool_response(responses).await?;
            }
            SessionEvent::Error(e) => eprintln!("session error: {e}"),
            SessionEvent::Disconnected(_) => break,
            _ => {}
        }
    }

    Ok(())
}
```

**Lines of code**: ~70
**What you manage**: Event loop, tool dispatch, tool response serialization,
phase waiting, all state.

## L1: Agent Runtime

At L1, `LiveSessionBuilder` handles the event loop, tool dispatch, and
state. You register callbacks and a `ToolDispatcher` instead of writing
a `match` over every event variant.

Same weather assistant:

```rust,ignore
use gemini_adk_rs::{SimpleTool, ToolDispatcher, LiveSessionBuilder};
use gemini_genai_rs::prelude::*;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Create tool dispatcher
    let mut dispatcher = ToolDispatcher::new();
    dispatcher.register(SimpleTool::new(
        "get_weather",
        "Get current weather for a city",
        None, // JSON Schema for parameters (None = no declared schema)
        |args| async move {
            let city = args["city"].as_str().unwrap_or("unknown");
            Ok(json!({ "city": city, "temp_c": 22, "condition": "sunny" }))
        },
    ));

    // 2. Build session config
    let config = SessionConfig::from_endpoint(
        ApiEndpoint::google_ai(std::env::var("GEMINI_API_KEY")?)
    )
        .system_instruction("You are a weather assistant. Use get_weather for queries.");

    // 3. Build callbacks
    let mut callbacks = gemini_adk_rs::EventCallbacks::default();
    callbacks.on_text = Some(Box::new(|t| print!("{t}")));
    callbacks.on_turn_complete = Some(std::sync::Arc::new(|| {
        Box::pin(async { println!() })
    }));

    // 4. Build and connect
    let handle = LiveSessionBuilder::new(config)
        .dispatcher(dispatcher)
        .callbacks(callbacks)
        .connect()
        .await?;

    // 5. Send a question (tools are auto-dispatched)
    handle.send_text("What's the weather in Tokyo?").await?;
    handle.done().await?;

    Ok(())
}
```

**Lines of code**: ~40
**What changed**: No event loop. No manual tool dispatch. No manual
`send_tool_response`. The `ToolDispatcher` handles tool calls automatically:
it matches the function name, deserializes args, calls your function, and
sends the response back to the model.

You also get `State` (via `handle.state()`), `SessionTelemetry`
(via `handle.telemetry()`), and the full three-lane processor for free.

## L2: Fluent DX

At L2, `Live::builder()` wraps everything in a chainable API. The same
weather assistant:

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let handle = Live::builder()
        .instruction("You are a weather assistant. Use get_weather for queries.")
        .with_tools(
            T::simple("get_weather", "Get current weather for a city", |args| async move {
                let city = args["city"].as_str().unwrap_or("unknown");
                Ok(json!({ "city": city, "temp_c": 22, "condition": "sunny" }))
            })
        )
        .on_text(|t| print!("{t}"))
        .on_turn_complete(|| async { println!() })
        .connect_google_ai(std::env::var("GEMINI_API_KEY")?)
        .await?;

    handle.send_text("What's the weather in Tokyo?").await?;
    handle.done().await?;
    Ok(())
}
```

**Lines of code**: ~20
**What changed**: No `SessionConfig` construction. No `ToolDispatcher`
setup. No `EventCallbacks` struct. The builder infers everything:
- `.with_tools()` creates and configures the `ToolDispatcher`
- `.instruction()` sets the system instruction on the underlying `SessionConfig`
- `.connect_google_ai()` builds the endpoint and connects in one call

### L2 with Multiple Tools

Tools compose with the `|` operator:

```rust,ignore
let handle = Live::builder()
    .instruction("You are a helpful assistant with access to tools.")
    .with_tools(
        T::simple("get_weather", "Get weather", |args| async move {
            Ok(json!({ "temp_c": 22 }))
        })
        | T::simple("get_time", "Get current time", |_| async move {
            Ok(json!({ "time": "14:30" }))
        })
        | T::google_search()
    )
    .on_text(|t| print!("{t}"))
    .connect_google_ai(api_key)
    .await?;
```

## Feature Comparison Table

| Feature | L0 | L1 | L2 |
|---------|:--:|:--:|:--:|
| WebSocket connection | `connect(config)` (or `ConnectBuilder::new(config)….connect()`) | `LiveSessionBuilder::new(config).connect()` | `Live::builder().connect_*()` |
| Event loop | Manual `while let` + `match` | Automatic (three-lane processor) | Automatic |
| Audio callback | Manual `match SessionEvent::AudioData` | `callbacks.on_audio = Some(...)` | `.on_audio(\|data\| ...)` |
| Tool dispatch | Manual match + response send | `ToolDispatcher` auto-dispatch | `.tools()` or `.with_tools()` |
| Tool declaration | Manual `Tool` + `FunctionDeclaration` | Auto from `ToolFunction::parameters()` | Auto from `T::simple()` |
| State management | None (DIY) | `State` with prefixes | `State` with prefixes |
| Phase machine | None (DIY) | `PhaseMachine::new()` | `.phase("name").instruction().done()` |
| Watchers | None (DIY) | `WatcherRegistry` | `.watch("key").became_true().then()` |
| Turn extraction | None (DIY) | `TurnExtractor` trait | `.extract_turns::<T>(llm, prompt)` |
| Instruction template | `handle.update_instruction()` | `callbacks.instruction_template` | `.instruction_template(\|state\| ...)` |
| Greeting | `handle.send_text()` after connect | `builder.greeting("...")` | `.greeting("...")` |
| Telemetry | None | `SessionTelemetry` auto-collected | Auto-collected |
| Session signals | None | `SessionSignals` auto-collected | Auto-collected |
| Transcription toggle | `config.input_transcription(true)` | Same | `.transcription(true, true)` |
| Computed state | None | `ComputedRegistry` | `.computed("key", &["deps"], \|s\| ...)` |
| Temporal patterns | None | `TemporalRegistry` | `.when_sustained()` / `.when_rate()` |
| Text agent tools | None | `TextAgentTool` | `.agent_tool("name", "desc", agent)` |

## When to Stay at L0

L0 is the right choice when you need:

**Custom transport**: You want to route WebSocket frames through a proxy,
use a Unix socket, or implement a custom reconnection strategy.

```rust,ignore
let handle = ConnectBuilder::new(config)
    .transport(MyCustomTransport::new())
    .codec(MyCustomCodec::new())
    .connect()
    .await?;
```

**Non-standard event processing**: Your application needs to process events
in an order or pattern that does not fit the callback model (e.g., batching
audio chunks before processing, custom priority queuing).

**Embedding in a larger runtime**: You are building your own agent framework
and want wire-level access without the L1 runtime's task spawning.

**Minimal binary size**: L0 has fewer dependencies than L1/L2.

## When to Stay at L1

L1 is the right choice when you need:

**Programmatic callback registration**: You build callbacks dynamically
based on configuration or plugin systems, and the fluent builder syntax
gets in the way.

```rust,ignore
let mut callbacks = EventCallbacks::default();
if config.enable_logging {
    callbacks.on_text = Some(Box::new(|t| println!("{t}")));
}
if config.enable_audio {
    callbacks.on_audio = Some(Box::new(move |data| {
        audio_tx.send(data.clone()).ok();
    }));
}
```

**Custom PhaseMachine setup**: You need to build the phase machine
programmatically (e.g., phases loaded from a database at runtime).

**Direct registry access**: You want to add/configure `ComputedRegistry`,
`WatcherRegistry`, or `TemporalRegistry` objects directly rather than
through sub-builders.

## Mixing Layers

The layers are designed to compose. Common patterns:

**L0 config + L2 builder**: Build a `SessionConfig` at L0 and pass it to
the L2 builder. Useful when `build_session_config()` handles credential
detection for you:

```rust,ignore
let config = build_session_config(Some("gemini-2.0-flash-live"))?
    .voice(Voice::Kore)
    .response_modalities(vec![Modality::Audio])
    .system_instruction("You are a helpful assistant.");

let handle = Live::builder()
    .on_audio(|data| { /* play */ })
    .on_text(|t| print!("{t}"))
    .connect(config)
    .await?;
```

**L1 types in L2 callbacks**: The `on_tool_call` callback receives `State`
(an L1 type) that you can query and mutate:

```rust,ignore
let handle = Live::builder()
    .on_tool_call(|calls, state| async move {
        // Promote tool context to state
        state.set("last_tool", calls[0].name.clone());
        None // auto-dispatch
    })
    .connect_google_ai(api_key)
    .await?;
```

**L0 handle from L2**: Access the underlying `SessionHandle` for operations
not exposed on `LiveHandle`:

```rust,ignore
let live_handle = Live::builder()
    .connect_google_ai(api_key)
    .await?;

// Access raw L0 handle
let session = live_handle.session();
let events = session.subscribe();
let phase = session.phase();
```

## Migration Checklist

When migrating from L0 to L2:

1. Replace `SessionConfig::from_endpoint(...)` with `Live::builder().instruction()` (the model stays optional at every layer)
2. Replace manual `Tool` declarations with `.tools(dispatcher)` or `.with_tools(T::simple(...))`
3. Replace the `while let Some(event) = recv_event(...)` loop with callbacks
4. Replace `match SessionEvent::AudioData` with `.on_audio()`
5. Replace `match SessionEvent::TextDelta` with `.on_text()`
6. Replace manual `send_tool_response()` with `ToolDispatcher` auto-dispatch
7. Replace `connect(config)` / `ConnectBuilder::new(config).connect()` with `.connect_google_ai()` or `.connect_vertex()`
8. Replace manual phase tracking with `.phase("name").instruction().transition().done()`
9. Replace manual state HashMaps with `.extract_turns::<T>()` and `handle.state()`
10. Remove the `tokio::select!` loop -- the three-lane processor handles it

## See also

- [Architecture Overview](./architecture.md) — the three-crate stack explained, with a guide on choosing your layer
- [S.C.T.P.M.A Operator Algebra](./composition.md) — fluent composition operators available at L2
