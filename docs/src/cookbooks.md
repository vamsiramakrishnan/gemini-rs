# Cookbook Examples

The `cookbooks/` directory contains runnable examples organized by complexity.
Each demonstrates specific SDK features at the layer you need.

## Getting Started

```bash
# 1. Configure credentials
cp .env.example .env
# Edit .env: set GEMINI_API_KEY (Google AI)
# or GOOGLE_CLOUD_PROJECT + GOOGLE_CLOUD_LOCATION (Vertex AI)

# 2. Run a standalone cookbook
cargo run -p cookbook-text-chat       # http://127.0.0.1:3001
cargo run -p cookbook-voice-chat      # http://127.0.0.1:3002
cargo run -p cookbook-tool-calling    # http://127.0.0.1:3003
cargo run -p cookbook-transcription   # http://127.0.0.1:3004

# 3. Run the multi-app UI (all apps + devtools panel)
cargo run -p cookbook-ui              # http://127.0.0.1:3000
```

## Standalone Cookbooks

These run independently with their own Axum server and minimal UI.

### text-chat (L0 Wire)

Minimal text-only Gemini Live session. Connects via WebSocket, sends text, receives
streaming deltas. No microphone required.

- **Port:** 3001
- **Layer:** L0 (`rs_genai::prelude::*`)
- **Features:** Text I/O, streaming text deltas, turn lifecycle

```rust,ignore
// Core pattern: connect, send text, receive events
let session = rs_genai::quick_connect(api_key, model).await?;
session.send_text("Hello").await?;
let mut events = session.subscribe();
while let Ok(event) = events.recv().await {
    if let SessionEvent::TextDelta(ref t) = event { print!("{t}"); }
    if let SessionEvent::TurnComplete = event { break; }
}
```

### voice-chat (L0 Wire)

Native audio voice chat with bidirectional audio streaming. Demonstrates voice
selection, VAD events, and real-time transcription.

- **Port:** 3002
- **Layer:** L0 (`rs_genai::prelude::*`)
- **Model:** `GeminiLive2_5FlashNativeAudio`
- **Features:** Bidirectional audio, input/output transcription, VAD events
- **Voices:** Puck, Charon, Kore, Fenrir, Aoede

### tool-calling (L1 Runtime)

Function calling with `TypedTool` and auto-generated JSON Schema from Rust structs.
Shows `ToolDispatcher` routing tool calls by function name.

- **Port:** 3003
- **Layer:** L1 (`rs_adk::tool::{ToolDispatcher, TypedTool}`)
- **Tools:** `get_weather(city)`, `calculate(expression)`

```rust,ignore
#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    /// The city to get weather for
    city: String,
}

let tool = TypedTool::new::<WeatherArgs>(
    "get_weather", "Get current weather",
    |args: WeatherArgs| async move {
        Ok(json!({"temp": 22, "city": args.city}))
    },
);

let mut dispatcher = ToolDispatcher::new();
dispatcher.register(tool);
```

### transcription (L0 Wire)

Comprehensive showcase of every Gemini Live API configuration property. The most
complete reference for wire-level options.

- **Port:** 3004
- **Layer:** L0 (`rs_genai::prelude::*`)
- **Features:** Input/output transcription, activity handling, turn coverage, server VAD,
  context window compression, session resumption, affective dialog

### agents (L1/L2 Runtime + Fluent)

CLI-based examples demonstrating text agent combinators and typed tool dispatch.

- **Binaries:** `weather-agent`, `research-pipeline`
- **Features:** Agent combinators (`>>`, `|`, `/`), copy-on-write builders,
  state transforms (`S::pick()`, `S::rename()`)

```rust,ignore
// Sequential pipeline: research then summarize
let pipeline = AgentBuilder::new("research").instruction("Research the topic")
    >> AgentBuilder::new("summarize").instruction("Summarize findings");

// Parallel fan-out
let parallel = AgentBuilder::new("a") | AgentBuilder::new("b");

// Fallback chain
let robust = AgentBuilder::new("primary") / AgentBuilder::new("fallback");
```

---

## Multi-App UI (`cookbook-ui`)

The UI cookbook bundles all apps into a single Axum server at `http://localhost:3000`
with a shared devtools panel showing real-time state, timeline, transcript, and telemetry.

### Crawl (Beginner)

#### text-chat

Minimal text-only session — no microphone needed.

- **SDK Features:** `Live::builder().text_only()`, text streaming
- **Try:** "What are three interesting facts about octopuses?"

#### voice-chat

Native audio chat with real-time transcription.

- **SDK Features:** `Modality::Audio`, voice selection, input/output transcription
- **Try:** "Hello! Tell me a joke."

#### tool-calling

Three demo tools: weather, time, calculator.

- **SDK Features:** `FunctionDeclaration`, `on_tool_call`, `NonBlocking` behavior,
  `WhenIdle` scheduling
- **Tools:** `get_weather(city)`, `get_time(timezone)`, `calculate(expression)`
- **Try:** "What's the weather in San Francisco?" / "Calculate 15 * 7 + 23"

### Walk (Intermediate)

#### all-config

Configuration playground — every Gemini Live option exposed via JSON config.

- **SDK Features:** Dynamic tool creation, modality switching, temperature,
  Google Search, code execution, context compression, session resumption
- **Try:** `{"modality": "text", "temperature": 1.5}`

#### guardrails

Policy monitoring with real-time corrective injection.

- **SDK Features:** `RegexExtractor`, `.watch()` state reactions,
  `.instruction_amendment()`, `.on_turn_boundary()`
- **Policies:** PII detection (SSN, credit cards), off-topic detection,
  negative sentiment monitoring
- **Try:** "My SSN is 123-45-6789" (PII) / "Did you see the football game?" (off-topic)

#### playbook

6-phase customer support state machine with regex-based state extraction.

- **SDK Features:** `.phase()` chains, `.transition_with()` guards, `.greeting()`,
  `.with_context()`, `RegexExtractor`, `.watch()`
- **Phases:** greet → identify → investigate → explain → resolve → close
- **Try:** "Hi, my name is Alex and I need help with my order."

### Run (Advanced)

#### support-assistant

Multi-agent handoff between billing and technical support.

- **SDK Features:** 10-phase dual state machine, `.computed()` derived state,
  `.watch()` escalation, cross-agent transitions, telemetry
- **Phases:** Billing (5 phases) + Technical (5 phases) with handoff on
  `issue_type == "technical"`
- **Try:** "I'm having trouble with my internet connection."

#### call-screening

Incoming call screening with sentiment analysis and smart routing.

- **SDK Features:** Phase machine, `NonBlocking` tool calling, `WhenIdle` scheduling
- **Tools:** `check_contact_list`, `check_calendar`, `take_message`,
  `transfer_call`, `block_caller`
- **Try:** "Hi, I'm John from Acme Corp, I need to speak to the manager."

#### clinic

HIPAA-aware telehealth appointment scheduling with clinical triage.

- **SDK Features:** Phase machine, 8 tools with `NonBlocking` behavior,
  patient intake, department routing
- **Tools:** `verify_patient`, `check_availability`, `book_appointment`,
  `get_doctors`, `check_insurance`, `get_patient_history`, `cancel_appointment`,
  `send_reminder`
- **Try:** "I need to schedule an appointment. I've been having headaches."

#### restaurant

Restaurant reservation assistant with menu context and special requests.

- **SDK Features:** Phase machine, 6 tools with `NonBlocking` behavior,
  dietary and occasion tracking
- **Tools:** `check_availability`, `make_reservation`, `get_menu`,
  `check_dietary_options`, `modify_reservation`, `cancel_reservation`
- **Try:** "I'd like a reservation for 4 this Saturday at 7pm. It's a birthday."

#### debt-collection

FDCPA-compliant debt collection with compliance gates and payment negotiation.

- **SDK Features:** `StateKey<T>` typed state, compliance watchers,
  identity verification, cease-and-desist handling
- **Try:** "Hello, who's calling?" / "I can't afford to pay the full amount."

---

## Platform Support

All cookbooks work with both **Google AI** (API key) and **Vertex AI** (project/location).

| Feature | Google AI | Vertex AI |
|---------|-----------|-----------|
| Async tool calling (`NonBlocking`) | Supported | Stripped automatically |
| Response scheduling (`WhenIdle`/`Silent`) | Supported | Stripped automatically |
| WebSocket frames | Text | Binary (handled automatically) |

The SDK detects your authentication method and strips unsupported wire fields
transparently — no code changes needed across platforms.
