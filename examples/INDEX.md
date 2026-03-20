# Examples

Runnable examples demonstrating gemini-rs features, organized by difficulty.

## Getting Started

```bash
cp .env.example .env   # then fill in your credentials
```

### Environment variables

All examples read from a shared `.env` at the workspace root via [`dotenvy`](https://docs.rs/dotenvy). Pick **one** auth method:

| Variable | Required | Description |
|----------|----------|-------------|
| `GEMINI_API_KEY` | **Option A** | Google AI API key ([get one](https://aistudio.google.com/apikey)). Fastest path. |
| `GOOGLE_GENAI_USE_VERTEXAI` | **Option B** | Set to `TRUE` to use Vertex AI instead of Google AI. |
| `GOOGLE_CLOUD_PROJECT` | with Vertex | GCP project ID (e.g. `my-project-123`). |
| `GOOGLE_CLOUD_LOCATION` | with Vertex | Region for the Live endpoint. Defaults to `us-central1`. |
| `GOOGLE_ACCESS_TOKEN` | optional | Explicit OAuth2 token. Falls back to `gcloud auth print-access-token`. |
| `GEMINI_MODEL` | optional | Override the default model (see below). |

> **Tip:** The ADK Web UI also accepts `GOOGLE_GENAI_API_KEY` as an alias for `GEMINI_API_KEY`.

#### Model string format

Always use the full `models/` prefix — the SDK normalizes it per-platform:

```bash
# Works on both Google AI and Vertex AI:
GEMINI_MODEL=models/gemini-2.5-flash-native-audio-preview-12-2025

# Google AI  → sent as-is in the setup message
# Vertex AI  → stripped to: projects/{project}/locations/{loc}/publishers/google/models/gemini-2.5-flash-native-audio-preview-12-2025
```

Omit `GEMINI_MODEL` to use the SDK default (`models/gemini-genai-2.5-flash-native-audio`).

#### Two-model architecture

Examples use **two separate models** that share the same auth credentials:

| Role | Model | Protocol | Configured via |
|------|-------|----------|----------------|
| **Live session** | `gemini-genai-2.5-flash-native-audio` | WebSocket | `GEMINI_MODEL` env var |
| **Text LLM** | `gemini-3.1-flash-lite-preview` | REST | `GeminiLlmParams` in code |

The text LLM powers extractors, agent-as-tool pipelines, and background analysis in advanced examples. It reads the **same auth env vars** — no extra configuration needed.

```rust
// Text LLM inherits auth from env, model set explicitly in code
let llm: Arc<dyn BaseLlm> = Arc::new(GeminiLlm::new(GeminiLlmParams {
    model: Some("gemini-3.1-flash-lite-preview".into()),
    ..Default::default()  // reads GEMINI_API_KEY / GOOGLE_CLOUD_PROJECT from env
}));
```

**Google AI** — single API key covers both. No location concept, no extra setup.

**Vertex AI** — the text LLM may need a different `location` than the Live session. The native audio model is region-locked to `us-central1`, but `gemini-3.1-flash-lite-preview` is available at the `global` endpoint. Examples handle this by passing `location: Some("global".into())` in `GeminiLlmParams`.

### Standalone examples

```bash
cargo run -p example-text-chat       # http://127.0.0.1:3001
cargo run -p example-voice-chat      # http://127.0.0.1:3002
cargo run -p example-tool-calling    # http://127.0.0.1:3003
cargo run -p example-transcription   # http://127.0.0.1:3004
```

### Multi-app Web UI

```bash
cargo run -p gemini-adk-web-rs                 # http://127.0.0.1:3000
```

All apps listed below are available in the multi-app UI with a shared devtools panel showing state, transcript, and telemetry.

---

## Standalone Examples

### text-chat (L0 Wire)

Minimal text-only Gemini Live session. Connects via WebSocket, sends text, receives streaming deltas. No microphone required.

- **Port:** 3001
- **Layer:** L0 (`gemini_genai_rs::prelude::*`)
- **Features:** Text I/O, streaming text deltas, turn lifecycle

### voice-chat (L0 Wire)

Native audio voice chat with bidirectional audio streaming. Demonstrates voice selection, VAD events, and real-time transcription.

- **Port:** 3002
- **Layer:** L0 (`gemini_genai_rs::prelude::*`)
- **Model:** `GeminiLive2_5FlashNativeAudio`
- **Features:** Bidirectional audio, input/output transcription, VAD events
- **Voices:** Puck, Charon, Kore, Fenrir, Aoede

### tool-calling (L1 Runtime)

Function calling with `TypedTool` and auto-generated JSON Schema from Rust structs. Shows `ToolDispatcher` routing tool calls by function name.

- **Port:** 3003
- **Layer:** L1 (`gemini_adk_rs::tool::{ToolDispatcher, TypedTool}`)
- **Features:** TypedTool with `JsonSchema` derive, ToolDispatcher, SessionEvent::ToolCall handling
- **Tools:** `get_weather(city)`, `calculate(expression)`

### transcription (L0 Wire)

Comprehensive showcase of every Gemini Live API configuration property. The most complete reference for wire-level options.

- **Port:** 3004
- **Layer:** L0 (`gemini_genai_rs::prelude::*`)
- **Features:** Input/output transcription, activity handling (`StartOfActivityInterrupts`), turn coverage, server VAD with automatic sensitivity, context window compression (2048 tokens), session resumption, affective dialog

### agents (L1/L2 Runtime + Fluent)

CLI-based examples demonstrating text agent combinators and typed tool dispatch.

- **Layer:** L1/L2 (`gemini_adk_rs::tool::*`, `gemini_adk_fluent_rs::prelude::*`)
- **Binaries:** `weather-agent` (TypedTool dispatch), `research-pipeline` (agent composition)
- **Features:** Agent combinators (`>>`, `|`, `/`), copy-on-write builder templates, `S::pick()` / `S::rename()` state transforms, `review_loop()` pattern

---

## Multi-App UI Index

### Crawl (Beginner)

#### text-chat

Minimal text-only Gemini Live session.

- **SDK Features:** `Live::builder().text_only()`, system instruction, text streaming
- **Tips:** Text-only mode — no microphone needed. Watch the streaming text deltas arrive in real time.
- **Try:** "What are three interesting facts about octopuses?" / "Explain quantum computing in simple terms"

#### voice-chat

Native audio voice chat with Gemini Live.

- **SDK Features:** `Modality::Audio`, voice selection, input/output transcription
- **Tips:** Click the microphone button to start speaking. Transcriptions appear below each message.
- **Try:** "Hello! Tell me a joke." / "What's the weather like on Mars?"

#### tool-calling

Function calling with three demo tools.

- **SDK Features:** `FunctionDeclaration`, `on_tool_call` callback, `FunctionCallingBehavior::NonBlocking`, `FunctionResponseScheduling::WhenIdle`
- **Tools:** `get_weather(city)`, `get_time(timezone)`, `calculate(expression)`
- **Tips:** Watch the devtools State tab to see tool call arguments and results.
- **Try:** "What's the weather in San Francisco?" / "What time is it in Tokyo?" / "Calculate 15 * 7 + 23"

### Walk (Intermediate)

#### all-config

Configuration playground — every Gemini Live option exposed via JSON config.

- **SDK Features:** Dynamic tool creation, modality switching (text/audio/both), temperature control, Google Search (`.with_google_search()`), code execution (`.with_code_execution()`), context window compression, session resumption
- **Tips:** Send JSON as the system instruction to configure any option. Supports text-only, audio-only, and both output modalities.
- **Try:** `{"modality": "text", "temperature": 1.5}` / Enable Google Search and ask it to search the web

#### guardrails

Policy monitoring with real-time corrective injection for live conversations.

- **SDK Features:** `RegexExtractor` for pattern-based violation detection, `.watch()` for state-driven reactions, `.instruction_amendment()` for dynamic instruction modification, `.on_turn_boundary()` for telemetry
- **Policies Detected:**
  - PII: SSN patterns (`XXX-XX-XXXX`), credit card numbers (`XXXX-XXXX-XXXX-XXXX`)
  - Off-topic: sports, movies, politics, recipes keywords
  - Negative sentiment: angry, frustrated, terrible, awful, etc.
- **Tips:** Try triggering a violation — the system injects corrective instructions in real time.
- **Try:** "My SSN is 123-45-6789" (PII) / "Did you see the football game?" (off-topic) / "This is terrible service!" (sentiment)

#### playbook

6-phase customer support state machine with regex-based state extraction.

- **SDK Features:** `.phase()` chains with `.transition_with()` guards, `.greeting()` for model-first speech, `.with_context()` for state-driven instruction injection, `RegexExtractor`, `.watch()` state reactions, `.on_turn_boundary()`
- **Phases:** greet → identify → investigate → explain → resolve → close
- **Tips:** The agent follows a structured support flow. Watch the devtools for phase transitions and evaluation scores.
- **Try:** "Hi, my name is Alex and I need help with my order." / "My order #12345 arrived damaged." / "I'd like a refund please."

### Run (Advanced)

#### support-assistant

Multi-agent handoff between billing and technical support with dual state machines.

- **SDK Features:** 10-phase dual state machine (5 billing + 5 technical), `.computed()` for derived state (`active_agent`), `.watch()` for escalation detection, cross-agent transitions, priority-ordered guards, telemetry snapshot polling
- **Phases:** Billing (greet → identify → investigate → resolve → close) + Technical (greet → identify → troubleshoot → resolve → close). Handoff triggers when `issue_type == "technical"`.
- **Tips:** Starts with billing — describe a technical issue to trigger handoff to technical support.
- **Try:** "I'm having trouble with my internet connection." / "I was overcharged $50 on my last bill."

#### call-screening

Intelligent incoming call screening with sentiment analysis and smart routing.

- **SDK Features:** Phase machine, `NonBlocking` tool calling, `WhenIdle` scheduling, sentiment-based routing
- **Tools:** `check_contact_list(name)`, `check_calendar(date)`, `take_message(caller, message)`, `transfer_call(extension)`, `block_caller(reason)`
- **State Keys:** `caller_name`, `caller_org`, `call_purpose`, `urgency`, `is_known_contact`, `caller_sentiment`
- **Try:** "Hi, I'm John from Acme Corp, I need to speak to the manager about our contract."

#### clinic

HIPAA-aware telehealth appointment scheduling with clinical triage.

- **SDK Features:** Phase machine, 8 tools with `NonBlocking` behavior, patient intake workflow, department routing
- **Tools:** `verify_patient(name, dob)`, `check_availability(department, date)`, `book_appointment(patient_id, department, doctor, date, time)`, `get_doctors(department)`, `check_insurance(provider, member_id)`, `get_patient_history(patient_id)`, `cancel_appointment(appointment_id)`, `send_reminder(patient_id, appointment_id)`
- **State Keys:** `patient_name`, `patient_id`, `symptoms`, `department`, `doctor_name`, `appointment_date/time`, `is_new_patient`, `insurance_provider`, `clinical_urgency`
- **Try:** "I need to schedule an appointment. I've been having headaches for the past week."

#### restaurant

Restaurant reservation assistant with menu context and special requests.

- **SDK Features:** Phase machine, 6 tools with `NonBlocking` behavior, occasion and dietary tracking
- **Tools:** `check_availability(date, time, party_size)`, `make_reservation(guest_name, date, time, party_size, phone)`, `get_menu(category)`, `check_dietary_options(dietary_need)`, `modify_reservation(reservation_id, changes)`, `cancel_reservation(reservation_id)`
- **State Keys:** `guest_name`, `party_size`, `preferred_date/time`, `phone`, `dietary_needs`, `special_occasion`, `reservation_id`
- **Try:** "I'd like to make a reservation for 4 people this Saturday at 7pm. It's a birthday dinner."

#### debt-collection

FDCPA-compliant debt collection with compliance gates, identity verification, and payment negotiation.

- **SDK Features:** `StateKey<T>` typed state access, compliance watchers, identity verification flow, cease-and-desist handling, payment processing
- **State Keys:** `identity_verified`, `disclosure_given`, `cease_desist`, `payment_processed`, `willingness`
- **Try:** "Hello, who's calling?" / "I can't afford to pay the full amount right now."

---

## Platform Support

All examples work with both **Google AI** (API key) and **Vertex AI** (project/location).

| Feature | Google AI | Vertex AI |
|---------|-----------|-----------|
| Async tool calling (`NonBlocking`) | Supported | Stripped automatically |
| Response scheduling (`WhenIdle`/`Silent`) | Supported | Stripped automatically |
| Thinking (`thinkingConfig`) | Supported | Stripped automatically |
| Audio model | `GeminiLive2_5FlashNativeAudio` | `GeminiLive2_5FlashNativeAudio` |
| Text model | `Gemini2_0FlashLive` | `Gemini2_0FlashLive` |
| WebSocket frames | Text | Binary (handled automatically) |

The SDK detects your authentication method and strips unsupported wire fields transparently — no code changes needed across platforms.
