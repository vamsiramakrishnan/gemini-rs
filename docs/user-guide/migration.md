# Migration Guide: L0 -> L1 -> L2

This guide shows the same voice agent implemented at all three layers,
so you can see what each layer adds and decide where to build.

## Why Migrate?

Each layer removes a category of boilerplate:

| What you write | L0 (gemini-live) | L1 (gemini-adk) | L2 (gemini-adk-fluent) |
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

## L0: Wire Protocol

At L0, you work directly with `SessionHandle`, `SessionEvent`, and
`SessionCommand`. You write your own event loop, dispatch tools manually,
and manage all state yourself.

Here is a weather assistant with one tool:

```rust,ignore
use gemini_live::prelude::*;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Build session config with tool declaration
    let config = SessionConfig::from_endpoint(
        ApiEndpoint::google_ai(std::env::var("GEMINI_API_KEY")?)
    )
        .model(GeminiModel::Gemini2_0FlashLive)
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
            }]),
            ..Default::default()
        });

    // 2. Connect
    let handle = ConnectBuilder::new(config).build().await?;
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
                    });
                }
                // Manual response send
                handle.send_tool_response(responses).await?;
            }
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
use gemini_adk::{SimpleTool, ToolDispatcher, LiveSessionBuilder};
use gemini_live::prelude::*;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Create tool dispatcher
    let mut dispatcher = ToolDispatcher::new();
    dispatcher.register(SimpleTool::new(
        "get_weather",
        "Get current weather for a city",
        |args| async move {
            let city = args["city"].as_str().unwrap_or("unknown");
            Ok(json!({ "city": city, "temp_c": 22, "condition": "sunny" }))
        },
    ));

    // 2. Build session config
    let config = SessionConfig::from_endpoint(
        ApiEndpoint::google_ai(std::env::var("GEMINI_API_KEY")?)
    )
        .model(GeminiModel::Gemini2_0FlashLive)
        .system_instruction("You are a weather assistant. Use get_weather for queries.");

    // 3. Build callbacks
    let mut callbacks = gemini_adk::EventCallbacks::default();
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
use gemini_adk_fluent::prelude::*;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let handle = Live::builder()
        .model(GeminiModel::Gemini2_0FlashLive)
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
    .model(GeminiModel::Gemini2_0FlashLive)
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
| WebSocket connection | `ConnectBuilder::new(config).build()` | `LiveSessionBuilder::new(config).connect()` | `Live::builder().connect_*()` |
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
| Transcription toggle | `config.enable_input_transcription()` | Same | `.transcription(true, true)` |
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
    .build()
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

1. Replace `SessionConfig::from_endpoint(...)` with `Live::builder().model().instruction()`
2. Replace manual `Tool` declarations with `.tools(dispatcher)` or `.with_tools(T::simple(...))`
3. Replace the `while let Some(event) = recv_event(...)` loop with callbacks
4. Replace `match SessionEvent::AudioData` with `.on_audio()`
5. Replace `match SessionEvent::TextDelta` with `.on_text()`
6. Replace manual `send_tool_response()` with `ToolDispatcher` auto-dispatch
7. Replace `ConnectBuilder::new(config).build()` with `.connect_google_ai()` or `.connect_vertex()`
8. Replace manual phase tracking with `.phase("name").instruction().transition().done()`
9. Replace manual state HashMaps with `.extract_turns::<T>()` and `handle.state()`
10. Remove the `tokio::select!` loop -- the three-lane processor handles it
