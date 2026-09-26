# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Security

- **Flow Studio ran commands from a posted spec.** A live run on
  `gemini-adk-web-rs` applied the spec the browser sent, which listed `mcp`
  entries to launch as local commands and HTTP bindings to call. The server
  listened on `0.0.0.0` with no authentication, so anyone who could reach the
  port could run commands on the host or reach internal addresses. The
  server now:
  - runs posted specs through `SessionSpec::sandboxed`, which drops `mcp`
    entries and turns HTTP bindings into mocks unless the operator allows
    them (`FLOW_STUDIO_ALLOW_HTTP`, `FLOW_STUDIO_ALLOW_MCP`). An allowed
    HTTP binding is checked on normalized URLs, and again at call time after
    interpolation and on every redirect;
  - binds to `127.0.0.1` unless `ADK_WEB_ADDR` says otherwise.

### Fixed

- **A reconnect after GoAway started a new conversation.** The transport
  reconnected on its own but re-sent the original setup, without the
  resumption handle the server had issued, so the server started a fresh
  conversation. When the session has resumption enabled, a reconnect now
  presents the latest handle. The session-persistence guide said there was no
  automatic reconnect; it now describes what happens.
- `SessionSpec::to_cargo_toml` (the Studio's Code tab) pinned the SDK at
  0.8. It now pins the current version.

### Added

- `adk spec codegen` generates a Rust, Python or Go project around a session
  spec, with one typed stub per mock tool. Rust registers the stubs in
  process. Python and Go serve them as MCP tool servers (official SDKs),
  and the project's `agent.json` binds those tools to them. Each project
  has tests, and CI builds and tests all three for every gallery spec.
  `adk spec test`, `adk spec call` and `adk spec run` test, call and run a
  spec. `POST /api/flows/project` returns the generated files to the
  Studio. See [From a spec to a project](docs/user-guide/spec-projects.md).
- A spec's tools can be implemented in code or on an MCP server. A tool's
  `mcp` field calls the tool of the same name on that server, and
  `SpecResources::implement` supplies an in-process implementation. The
  spec's declaration, `set_state` and `save_response_as` apply either way.
- `SessionSpec` can carry a `conversation` (the conversation compiler's
  spec) in place of a `flow`, plus `scenarios` to run against it with
  `SessionSpec::run_scenarios`. One document now describes a voice agent
  end to end: model, tools, conversation and tests. Stage resolvers bind to
  declared tools by name. Flow Studio's test endpoint runs the scenarios.
  `Scenario` and `SimStep` derive `JsonSchema`, so the spec schema covers
  them.
- `SessionSpec::sandboxed` and `BindingAllowlist`, for applying a spec you
  did not write.

## [3.0.0] - 2026-09-25

### Highlights

- **Gemini 3.8 Live**, including the extended-thinking variant: custom
  transcription vocabulary, blocking tools, history seeding, Live Avatar video
  and transparent resumption (both Vertex AI), and setups shaped per model and
  platform (`LiveModelProfile`, `SessionConfig::ignored_settings()`). All of it
  was checked against the live Google AI endpoint.
- **Google AI sessions no longer die on `update_instruction` or
  `.proactive_audio()`**: both used to close the session with 1007.
- **Text agents on the golden path**: `agent.ask(..)`, `ask_as::<T>()`,
  `chat()`, `stream(..)`, `RunResult` with usage and tool calls, `MockLlm`
  for model-free tests, and `#[tool]` from doc comments.
- **Four wire encodings fixed** (VAD sensitivity, media resolution, voice
  activity, avatar media routing). Setups that set VAD sensitivity or media
  resolution were refused by the server (1007, confirmed live).
- **`gemini-adk`**, the one-crate facade over the fluent layer and memory, is
  published for the first time, with one `Error` type for application code.
- **Conversations are testable offline, end to end.** An injectable clock,
  record/replay tapes for model and resolver calls, `ScriptedServer` for
  Live sessions over an in-memory transport, and scenarios extracted from a
  recorded journal and replayed in CI (`adk flow replay`, `adk flow why`).
- **The conversation graph governs voice behavior.** Per-stage timing
  (reprompts, filler cues, floor holding, end-of-speech hold), verbatim
  stages checked against the transcript, corrections that reopen a
  confirmation, nested digressions, and escalation on repeated barge-ins or
  tool failures. Redaction and commit (idempotency and compensation)
  policies are enforced at runtime.
- **Raw SIP calls can register and encrypt**: `SipAgent::register` and SRTP
  (SDES) media. A published jitter benchmark shows the runtime adds no
  measurable mic-to-wire jitter up to 1,000 sessions.
- **Breaking**: this is a major release. The *Changed*, *Removed* and
  *Deprecated* sections below list what to update. Several public enums and
  structs are now `#[non_exhaustive]`, and the fluent prelude is a curated
  list instead of a glob of the wire prelude.

### Fixed

- **Instruction updates closed Google AI sessions.** `update_instruction`, and
  with it every phase transition in the default `InstructionUpdate` steering
  mode, sent a `system`-role client content turn, which the Google AI endpoint
  answers with close code 1007. This was measured live on Gemini 2.5, 3.1 and
  3.8 Live. On Google AI the update is now a user-role turn that says it
  replaces the instructions, sent with `turnComplete: false`. Gemini 3.1 and
  3.8 follow it; Gemini 2.5 accepts it without following it. Vertex AI keeps
  the documented `system` role
  (`SessionConfig::supports_system_role_updates()`).
- **`.proactive_audio()` broke every Google AI session.** Google AI's setup has
  no `proactivity` field, and a setup that carries one is refused with 1007
  (measured on 2.5 and 3.8). It is now left off on Google AI and reported by
  `ignored_settings()`.
- **Gemini 3.8 Live Avatar video would have played as audio.** Every
  `inlineData` part from the model was decoded and sent to `on_audio`,
  whatever its MIME type. Only `audio/*` parts go there now; anything else
  (`video/mp4`) arrives as `SessionEvent::Media` / `LiveEvent::Media` /
  `on_media`.
- **VAD sensitivity was sent with values the API does not have.**
  `Sensitivity::SensitivityHigh` went out as `SENSITIVITY_HIGH`; the fields
  take `START_SENSITIVITY_HIGH` / `END_SENSITIVITY_LOW` and so on. Both fields
  now use their own wire enums, and levels the API has no value for (`Medium`,
  `Automatic`) are sent as `*_UNSPECIFIED`, the server default. Specs that set
  `"start_sensitivity": "high"` were affected.
- **`MediaResolution` was sent as `LOW` / `MEDIUM` / `HIGH`.** The API's names
  are `MEDIA_RESOLUTION_LOW` and so on. The old spellings are still read.
- **Server voice-activity events failed to parse.** The API sends
  `ACTIVITY_START` / `ACTIVITY_END`; the parser expected `VOICE_ACTIVITY_*`,
  so a `voiceActivity` frame errored instead of emitting
  `VoiceActivityStart`. Both spellings are accepted now, unknown values read as
  `VoiceActivityType::Unspecified`, and `audio_offset` is exposed.
- `lastConsumedClientMessageIndex` is accepted as a JSON number as well as the
  proto-JSON string.
- **Model errors lost their kind on the text path.** `GeminiLlm` flattened
  every failure into `LlmError::RequestFailed(String)`, and `LlmTextAgent`
  flattened that again into `AgentError::Other("LLM error: …")`, so a caller
  could not tell a rate limit from a bad key without parsing text. `LlmError`
  now carries `Api { status, message }`, `Auth`, `Transport` and `Config`,
  with `status()`, `is_rate_limited()`, `is_auth()`, `is_retryable()` and
  `is_content_filtered()`; it reaches the caller as `AgentError::Llm`
  (`AgentError::as_llm()`).
- **A blocked prompt returned an empty answer.** `promptFeedback.blockReason`
  and a reply withheld for safety (`SAFETY`, `PROHIBITED_CONTENT`, …) are now
  `LlmError::ContentFiltered(reason)`. A reply truncated at `MAX_TOKENS` is
  still returned.
- **Text-path token usage reported zero output tokens.** `generateContent`
  names the count `candidatesTokenCount`, which `UsageMetadata` did not read.
  It does now, and thinking tokens, billed as output, count toward
  `completion_tokens`.
- `LlmResponse::finish_reason` from `GeminiLlm` uses the API's names
  (`"STOP"`, `"MAX_TOKENS"`), as mocks and recordings do, instead of Rust
  `Debug` names (`"Stop"`).
- **`adk create` wrote a project that did not build.** It pinned version
  0.5, depended on a `gemini-live` crate that does not exist, defaulted to the
  retired `gemini-2.0-flash`, and accepted names that are not package names.
  The scaffold's `src/main.rs` is now a real file compiled (and linted) in this
  repository as the CLI's `scaffold-agent` example: a streaming conversation
  built on `GeminiLlm::from_env()` and `chat()`. The generated `Cargo.toml`
  depends on the SDK release the CLI was built from, the default model is
  `gemini-flash-latest`, and `.env` names `GEMINI_API_KEY`. The `deploy`
  Dockerfile built with Rust 1.82, below the MSRV, and without the OpenSSL
  headers the default TLS backend needs; it uses `rust:1-slim` with
  `libssl-dev`, and `libssl3` at runtime.
- **`AgentBuilder::build` dropped most of its configuration.** Only the
  instruction, temperature, max tokens and function tools reached the agent;
  `model`, `top_p`, `top_k`, `stop_sequences`, `thinking`, `output_schema`,
  `output_key` and every built-in tool (`google_search`, `code_execution`,
  `url_context`) were accepted and ignored, so changing the model changed
  nothing. They are all sent now — `LlmRequest` gained `model`, `top_p`,
  `top_k`, `stop_sequences` and `thinking_budget`, `LlmTextAgent` the matching
  setters, and `GeminiLlm` maps every field (a test asserts each one reaches
  the wire body). Settings a text agent cannot honour — `voice`, audio
  modalities, a Live model, `sub_agent`/`transfer_to`/`stay`/`isolate` — are
  now a `ConfigError` naming what to use instead, not a silent no-op.
- **`conditional(..)` did not branch.** It always ran the true branch, and
  rebuilt both branches from their name and instruction only. It now compiles
  to a `RouteTextAgent` (new `Composable::Branch`) that runs exactly the branch
  the predicate chooses, with each branch's full configuration, and accepts
  any `Composable`.
- **`Live::dispatcher(..)` discarded tools registered before it.** They are
  merged in (new `ToolDispatcher::merge`); the supplied dispatcher wins a name
  clash.
- **`ToolDispatcher` declarations were cached forever and unordered.** The
  cache is now cleared on every registration, and declarations are ordered by
  name, so a setup message is identical from run to run.
- **A `T::confirm` tool ran unconfirmed when no confirmation provider was
  set.** `AgentBuilder::build` and `Live::connect` refuse that configuration,
  and `check_live` reports it (`LiveViolation::UnconfirmedTools`).
  `AgentBuilder::confirmation_provider` is new. A declined call now returns
  `ToolError::Declined(reason)`, so the model can tell the user why, instead
  of a bare `Cancelled`.
- **A model that called a tool on an agent with no tools got an empty turn
  back** and called again until the ten-round limit. Each call is now answered
  with `ToolError::NotFound`, which the model can act on.
- `GeminiLlm` no longer carries a `preprocess_request` stub that did nothing.
- **`#[tool]` sent a schema the API rejects.** It used raw
  `schemars::schema_for!`, so an `Option<String>` parameter declared
  `"type": ["string", "null"]` — refused outright, and on Live by closing the
  socket during setup — and a nested enum became a `$ref` the API ignores.
  `#[tool]`, `TypedTool` and `extract_turns*` now share one pipeline,
  `gemini_adk_rs::tool::wire_schema::<T>()`.
- **A tool result that was not a JSON object failed the request.** The API
  accepts only an object in `functionResponse.response`; a tool returning a
  number, string, array or `null` is now sent as `{"output": value}`, the key
  the API documents, on every path (text, Live, REST).
- **`#[tool]` dropped the function's other attributes**, including `#[cfg]`:
  a tool behind a disabled feature still compiled. `#[cfg]` now gates every
  generated item, doc comments and `#[deprecated]` stay on the constructor,
  and the rest (`#[allow]`, `#[tracing::instrument]`) stay on the body.
- `T::simple` was documented with arguments it never declared. It declares
  no parameters, and its documentation now says so and points at `#[tool]`
  and `T::typed`.
- **`Live::converse(&convo)` did not install a conversation's digressions or
  repair policies.** It attached the main flow and extractors only, while the
  simulator drove a `FlowStack` with everything the conversation declared, so
  a scenario could pass in Conversation CI and the same digression never fire
  on a call. The stack now lives in the runtime (`gemini_adk_rs::flow::FlowStack`)
  and is the only governance object the control plane drives: a bare `govern`
  is a stack with no digressions, and `converse` installs the overlays and
  repair policies on it. A control-plane test drives a digression through the
  real turn path, and a fluent test asserts the installed stack and the
  simulator agree turn by turn.

- **A digression that completed on its entry turn was never heard.** The stack
  applied its `Resume` policy without ever making it the active layer, so the
  control plane read the *main* flow for postures and `flow:overlay` — and the
  one-terminal-stage flow `Policy::safety_handoff` lowers to could fire without
  its hand-off instruction ever reaching the model. A digression now governs
  the turn on which it completes (that turn projects its terminal stage's
  instruction) and resumes at the next turn boundary.

- **A terminated conversation still governed with the main flow.**
  `Resume::Terminate` set a flag that made `on_turn` a no-op, but `current()`,
  `admits_tool()` and the posture accessors still delegated to the suspended
  main monitor, so a session that was not disconnected kept receiving main-flow
  steering and could call tools that flow admitted. A terminated stack is inert:
  no active steps, no postures or grounds, no `on_enter` actions, and every tool
  denied with the reason. The new `flow:terminated` state key (and
  `FlowStack::is_terminated`) tells the application to close the session; the
  runtime still never hangs up on its own.

- **A `reset(..)` on an escalated step was ineffective.** Repair bookkeeping ran
  before the monitor applied `Constraint::Reset`, so the escalate signal the
  lowered `escalate_to` edge reads stayed latched; the re-latch then completed
  the step again on that stale signal and routed straight back to the hand-off
  target. Resets are applied first now (`FlowMonitor::begin_turn`), and the
  steps they un-latch have their repair signals and counters cleared before the
  re-latch — as does every repair-tracked step on a `Resume::Restart`. A reset
  can also be gated on a tool (`reset(..).when(called_ok("start_over"))`), whose
  edge fires inside `on_tool_ok` rather than at a turn boundary; that path sheds
  the same signals (`FlowMonitor::begin_tool_ok`), and `FlowStack::observe_tool`
  now runs the conformance check itself and records through it, instead of
  delegating to a monitor that would bypass the shedding.

- **`--all-features` did not compile** after a lone `opentelemetry_sdk` 0.31 →
  0.32 bump split the OpenTelemetry family across two versions; the sdk is back
  on 0.31 with its exporters, the manifest says why they move together, and
  Dependabot now groups them so a partial bump cannot recur.
- `session/vertex_ai.rs` interpolated caller-supplied `session_id` and `user_id`
  into request URLs unencoded — a session id of `../../otherEngine` addressed
  a different reasoning engine, and a `&` in a user id appended a second query
  parameter. Both are percent-encoded now.
- `ci.yml` declared no `permissions`, so every job ran with the repository's
  default `GITHUB_TOKEN` scope; it is `contents: read`.

### Added

- **Injectable clock** (`gemini_adk_rs::clock`): `Clock`, `SystemClock`,
  `ManualClock`. Set it with `State::with_clock`,
  `LiveSessionBuilder::clock` or `Live::clock`. Temporal patterns, phase
  timing, session signals, the reactor, resolver caches and journal
  timestamps all read it. `replay_session` drives a `ManualClock` from the
  recording's timestamps, so a replay sees the time the recording saw.
- **Record/replay tapes** (`gemini_adk_rs::tape`): `TapedLlm::recording` /
  `replaying` and `taped_resolver` record model and resolver outputs to a
  `MemoryTape` or a JSONL `FileTape` and play them back. A request missing
  from the tape is an error, never a live call.
- **Journal cursors**: `State::try_mutations_since(cursor)` reports a gap
  when the ring dropped entries. Computed state recomputes only the
  variables whose dependencies changed since its last cursor. `read_journal`
  loads a JSONL journal sink.
- **Offline Live sessions**: `connect_with_transport` on `LiveSessionBuilder`
  and `Live`; `testing::ScriptedServer` (the model's side as a script:
  `says`, `hears`, `speaks`, `calls`, `interrupts`) and `ScriptedRun` (events,
  state, frames sent, tool responses). `ReplayTransport::with_frame_gate`.
- **Voice timing per stage**: `VoiceTiming` (`reprompt_after`, `filler_after`,
  `uninterruptible`, `end_of_speech`, `context_delivery`) on a conversation
  stage (`.timing(..)`, spec field `timing`) or `Live::stage_timing`. The
  active stage's timing is published under `session:voice_timing`. New events
  `LiveEvent::Reprompted` and `LiveEvent::FillerCue`.
- **Verbatim stages**: `.verbatim(text)` (spec field `verbatim`). A stage
  completes only when the model's output transcript matches the text
  (word-level similarity at least 0.9). `LiveEvent::VerbatimChecked` reports
  each check.
- **Statechart and repair**: digressions nest and resume layer by layer.
  Correcting a collected slot raises `correction:{slot}` and reopens the
  confirmation downstream. `RepairPolicy::escalate_after_interruptions` and
  `escalate_after_tool_failures`.
- **Policies enforced at runtime**: `Live::policy(..)`. `Policy::redact(keys)`
  masks the keys in the journal sink, snapshots and extraction events
  (`State::redact_keys`). `Policy::commit(tool)` wraps the tool in
  `tool::CommitGuard`: an idempotency key (a retried commit returns the first
  result) and a compensating tool on failure.
- **Tool context**: `tool::ToolContext` gives a tool the session state, its
  call id and a cancellation token. It is available through `ContextTool`,
  `T::contextual`, and a `ToolContext` parameter on a `#[tool]` function
  (left out of the schema). Live barge-in cancels the token of an inline
  tool call.
- **CI from recordings**: `Scenario::from_journal`, `SimStep::ToolFailed`,
  `Interrupt` and `ToolResult`, and `Sim::apply`. The runtime journals
  `flow:tool_call`, `flow:tool_denied` and `flow:tool_result`. CLI:
  `adk session scenario`, `adk flow replay` (a CLEAN/DIVERGED timeline) and
  `adk flow why`.
- `gemini_adk::{Error, Result}`: one error for application code, converting
  from every layer's error and keeping it as the source, with
  `is_retryable()` and `llm()`.
- `AgentBuilder::artifacts(A::..)` attaches artifact declarations, so
  `check_contracts` sees an artifact input no agent produces. An `S::` chain
  is a pipeline step: `a >> S::pick(..) >> b`.
- **SIP registration**: `SipAgent::register(SipAccount)` returns a
  `SipRegistration` that refreshes the binding, retries after a failure,
  reports `RegistrationState`, and removes the binding on `unregister` or
  drop.
- **SRTP** (`telephony::srtp`, feature `sip`): RFC 3711 with
  `AES_CM_128_HMAC_SHA1_80` / `_32`, SDES keying (`CryptoAttribute`),
  rollover tracking and a replay window. It is checked against the RFC 3711
  and libsrtp test vectors. `SipAgent` answers `RTP/SAVP` offers with SRTP
  (`sdp::secure_audio_answer`).
- `session-bench --jitter-secs`: mic-to-wire latency and jitter under N
  concurrent sessions, with results published in the capacity guide.
- Schema snapshots for `#[tool]`, `#[derive(Extract)]` and `#[derive(Frame)]`
  (`crates/gemini-adk-macros-rs/tests/snapshots`, regenerate with
  `UPDATE_SNAPSHOTS=1`).
- The Python binding is versioned 3.0.0 and built and smoke-tested in CI.
- **Gemini 3.8 Live Extended Thinking** — `ModelId::LIVE_3_8_EXTENDED_THINKING`
  with its own `LiveModelProfile`, `ThinkingLevel` / `ThinkingConfig::thinking_level`
  and `.thinking_level(..)` (L0 and L2); the model refuses a setup without a
  level. A question that needs thought gets a spoken holding line first; the
  answer arrives unprompted as the next turn, and
  `SessionEvent::InteractionStatus("IN_PROGRESS")` (from the new
  `serverContent.interactionStatus`) marks the wait.
- **Gemini 3.8 Live support** — `ModelId::LIVE_3_8` (`gemini-3.8-live`, GA
  2026-09-24 on Vertex AI and Google AI; the wire behavior below was verified
  live against Google AI). See the new *Gemini 3.8 Live* guide.
  - `LiveModelProfile`: what a Live model accepts in its setup, keyed on the
    model name. For 3.8 the setup leaves off `thinkingConfig` (unsupported)
    and `enableAffectiveDialog` / `proactivity` (always on; its guide says not
    to send them), and keeps tool `behavior` and response `scheduling` on
    Vertex AI, which earlier Vertex Live models reject. Unknown models get
    every field passed through.
  - `SessionConfig::ignored_settings()` lists every configured setting left
    off the wire; connect logs it once as a warning.
  - Live Avatar: `AvatarConfig::prebuilt("Ben")` / `AvatarConfig::custom(image,
    "png")` via `.avatar(..)` (L0 and L2), which also sets the `VIDEO` response
    modality; `Modality::Video`; video chunks as `InlineMedia` on
    `SessionEvent::Media`, `LiveEvent::Media`, `EventCallbacks::on_media`
    and `Live::on_media`, suppressed after barge-in like audio, with their own
    `DeliveryConfig::media` policy.
  - `AudioTranscriptionConfig` with `language_codes` and `custom_vocabulary`;
    `input_transcription_config` / `output_transcription_config` /
    `custom_vocabulary` on `SessionConfig` and `Live`.
    `InputAudioTranscription` and `OutputAudioTranscription` are now aliases
    of it.
  - `ReplicatedVoiceConfig` and `.replicated_voice(..)` for a voice replicated
    from a sample.
  - `.transparent_resumption()` (`sessionResumption.transparent`),
    `.history_in_client_content()` / `initial_history_in_client_content(..)`
    (`historyConfig`), and `.explicit_vad_signal()` (Vertex AI only; left off
    the wire on Google AI).
- **`gemini-adk`, one crate to depend on** (`crates/gemini-adk`, library
  `gemini_adk`). It re-exports the fluent crate with the same feature names,
  adds a `memory` feature for `gemini_adk::memory`, and `#[tool]` resolves its
  runtime through it. It is `publish = false` until its crates.io name is
  confirmed.
- **Every workspace member now declares the MSRV** (`rust-version = "1.93"`,
  inherited); none did, so crates.io showed no minimum and an older toolchain
  failed with an unrelated error.
- **OpenTelemetry GenAI spans on the text path.** Each run is an
  `invoke_agent {agent}` span, each model call a `chat {model}` span carrying
  `gen_ai.request.*` settings, `gen_ai.usage.input_tokens`/`output_tokens`,
  `gen_ai.response.finish_reasons` and `error.type`, and each tool call an
  `execute_tool {tool}` span — the GenAI semantic conventions, so a backend
  reads latency by model, token cost and tool timelines without
  configuration. Message content is not recorded. Model calls also record
  the `gemini_llm_*` metrics (calls, duration, tokens) under the `metrics`
  feature; they were defined but never recorded.
- **Streaming text agents.** `agent.stream(prompt)` and
  `TextAgent::run_stream(request, state)` yield `RunEvent`s — `TextDelta` as
  the model writes, `ToolCall` and `ToolResult` around each tool, and
  `Finished(RunResult)` last — across tool rounds; `Chat::send_stream` adds
  the turn to the history when it finishes. An agent with middleware emits
  each model turn only after `after_model` has seen it, so a redacting guard
  cannot be bypassed through the deltas. Underneath: `BaseLlm::generate_stream`
  (default: one chunk; `GeminiLlm` streams for real, `MockLlm` word by word),
  `LlmResponse::append` to fold chunks, and at L0
  `Client::stream_generate_content_with` over `streamGenerateContent?alt=sse`,
  `HttpClient::post_sse` and a chunk-boundary-safe `SseDecoder`.
- **`agent.ask(..)`, `agent.ask_as::<T>(..)` and `agent.chat()`** on every
  `TextAgent`. `ask` sends one prompt and returns the reply, with no `State`
  and no magic `"input"` key. `ask_as` sends `T`'s JSON Schema as the response
  schema and deserializes the reply; if it does not parse, the model is shown
  the error and asked once more, and a second failure is
  `AgentError::InvalidOutput`. `chat()` returns a `Chat` that carries the
  conversation's history and state across `send` calls and adds up usage.
- **`TextAgent::run_with(RunRequest, &State) -> RunResult`**, the primitive
  the three are built on. A `RunRequest` carries the new turn (text or media),
  the history and an optional response schema; a `RunResult` reports the
  reply, the turns to append to the history, token usage, every tool call
  (`ToolCallRecord`) and the number of model calls. `LlmTextAgent` implements
  it natively; every other agent gets it through `run`.
- `AgentBuilder::output::<T>()`, and `RunResult::parse::<T>()` to read the
  reply back.
- **`GeminiLlm::from_env()`** (and `try_new(params)`): the same configuration
  as `new`, checked before any request — a missing API key, a Vertex AI setup
  without a project, or a Live model on the text API fails at once with the
  variable to set, instead of on the first request.
- `AgentError::State` (so `state.set(..)?` works in a function returning
  `AgentError`) and `AgentError::InvalidOutput`.
- **`MockLlm`, a public test model** (`gemini_adk_rs::llm::MockLlm`, also in
  the fluent `testing` module). `MockLlm::text` repeats one reply,
  `MockLlm::script` replies in order and fails loudly once spent, and
  `MockLlm::from_fn` computes each reply from the request. Every request is
  recorded (`requests`, `last_request`, `call_count`), so a test can assert on
  what an agent sent, not only on what it returned. Clones share the script
  and the recording. The runtime's own text-agent tests now use it instead of
  four hand-written mocks.
- `LlmResponse::{from_text, tool_call, tool_calls, with_usage}` and
  `TokenUsage::new`, with `TokenUsage` now `Copy + Default + PartialEq` and
  summable (`+`, `+=`).
- **`#[tool]` reads the function's documentation.** The doc comment's prose is
  the description (`#[tool("...")]` still overrides it), and a rustdoc
  `# Arguments` section describes each parameter in the schema; documenting a
  parameter that does not exist is a compile error. The return type can be
  any `Serialize` type, a `Result` with any error type (`io::Result<T>`,
  `anyhow::Result<T>`, ...), or nothing. A borrowed parameter (`&str`) is a
  compile error that names the owned type to use.
- `ToolError::from_error`, which keeps a `ToolError` and turns any other
  error into `ExecutionFailed` with its message.
- `T::timeout`, `T::cached` and `T::confirm` accept any tool, so a `#[tool]`
  function's value wraps directly: `T::timeout(search(), secs)`.
- `T::typed::<A>(name, description, closure)`: a closure tool whose arguments
  are a `JsonSchema` type, for tools that capture a client or pool.
- `gemini_adk_rs::tool::wire_schema::<T>()`, public.
- `BaseLlm` is implemented for `Arc<L>` and `Box<L>`, and `TextAgent` for
  `Arc<A>` and `Box<A>`. A built `Arc<dyn TextAgent>` satisfies any
  `impl TextAgent` parameter, and any model satisfies `impl BaseLlm`.
- `gemini_adk_rs::flow::{FlowStack, Overlay, Resume, RepairPolicy,
  SharedFlowStack}`, `FlowMonitor::into_stack`/`restart`,
  `LiveSessionBuilder::flow_stack`, and the `flow:overlay` state key naming
  the active digression. The fluent `conversation::{FlowStack, Resume,
  RepairPolicy}` paths are re-exports of the runtime types.
- `flow::TERMINATED_STATE_KEY` (`flow:terminated`), published at every turn
  boundary, and `FlowStack::is_terminated()` — how an application learns that a
  `Resume::Terminate` digression ended the conversation and it should hang up.
- `FlowMonitor::begin_turn` and `FlowMonitor::begin_tool_ok` (the turn/tool
  count and reset edges, split out of `on_turn`/`on_tool_ok` so a caller holding
  evidence outside the marking can shed it before the re-latch), and
  `FlowMonitor::closing_steps`/`closing_postures`/
  `closing_grounds` (a completed flow's terminal steps — its last word, which a
  terminal step never being *active* otherwise hides).
- `Sim::postures()` and `Sim::is_terminated()`, so a scenario can assert what a
  digression actually says, not merely that it fired.
- `FlowMonitor::record_violation`, for a deviation the caller sees and the
  monitor cannot. A terminated `FlowStack` uses it so that `Observe` still
  records a tool used after the conversation ended — the denial is the stack's,
  not the flow's — without advancing a flow that has finished.
- `Live::digressions()` and `Live::repair_policies()` introspection, and
  `CompiledConversation::repair_policies()`.
- `Conversation::instruction(..)` as the name for a stage's model guidance;
  `say(..)` remains as an alias. Spec documents accept `"instruction"` as
  well as `"say"`.
- **`example-session-bench`, a concurrent-session capacity harness.** Holds
  `N` Live sessions open in one process, each the real L1 control plane over
  a scripted L0 transport that answers every turn after a fixed delay, and
  reports resident memory (baseline, connected, peak, after disconnect, per
  session), `connect` time, `send_text` → first text and → `TurnComplete`
  percentiles, and a cross-check of the runtime's own telemetry against the
  turns it drove. `just bench-sessions 100`; JSON output; a four-session
  smoke test runs under `cargo test --workspace`. The guide is
  `docs/user-guide/capacity.md`. No credentials, no network.
- `Conversation`, `ConversationSpec`, `CompiledConversation` and `Sim` are in
  the fluent prelude. The authoring model and the model-free simulator were
  the one headline feature that needed a second import line.
- `gemini-adk-fluent-rs` feature bundles: `voice` (`voice-io`, `denoise`,
  `dsp`, `vad-wavekat`) and `full` (`voice` plus `sip`, `http-tools`,
  `templates`, `otel-otlp`, with the default TLS backend).
- `TungsteniteError::NoTlsBackend` and `transport::HAS_TLS_BACKEND`: a build
  with neither `tls-native` nor `tls-rustls` still compiles, but a `wss://`
  dial now fails before any socket is opened, with an error that names the
  feature to enable, instead of a handshake error from inside the WebSocket
  stack.

### Removed

- The root `benches/` directory. Its two files were never part of any
  package (the workspace root has no `[package]`) and called a ring-buffer
  API that no longer exists; the live benchmark is
  `crates/gemini-genai-rs/benches/audio_pipeline.rs`.

### Deprecated

- `gemini_adk_server_rs::{FlowAppSpec, MockToolSpec}`: use `SessionSpec` and
  `ToolSpec`. The document format is unchanged.
- `Live::agent_tool_arc`: `agent_tool` accepts an `Arc<dyn TextAgent>` now.
- Aliases that duplicated a name: `AgentBuilder::instruct` (use `instruction`),
  `AgentBuilder::describe` (`description`), `AgentBuilder::no_peers`
  (`isolate`), `Pipeline::sub_agent` (`step`), `FanOut::sub_agent` (`branch`)
  and `T::fn_tool` (`T::simple`, `T::typed` or `#[tool]`).

### Changed

- **Composition operators have one meaning each** (breaking): `>>` is
  "then" and `+` is "together". `C` context policies chain with `>>` (was
  `+`), `M` middleware with `>>` (was `|`), and `T` tools, `G` guards and
  `E` criteria combine with `+` (was `|`). See the migration guide.
- The fluent prelude's `ToolContext` is the tool context
  (`gemini_adk_rs::tool::ToolContext`). The callback context stays at
  `gemini_adk_rs::context::ToolContext`.
- `Motif::disclosure` and verbatim stages are uninterruptible by default.
- `Live::converse` keeps the stage timings and policies set on the builder
  (`stage_timing`, `policy`) in either call order. It used to replace them.
- `RepairPolicy` has two more fields, `SimStep` three more variants, and
  `SipError` three more variants. `sdp::AudioOffer` has `secure` and `crypto`.
- Computed state is written only when its value changes, so a watcher on a
  computed key fires only on a real change.
- `wire_schema` collapses `Option<T>` for a struct or enum `T`
  (`anyOf: [T, {type: null}]`) to `T`, like the nullable primitives already
  were. `#[tool]` schemas no longer carry the hidden args struct's name as
  their `title`.

- On Google AI, `explicitVadSignal`, `sessionResumption.transparent` and
  `avatarConfig.avatarName` / `customizedAvatar` are left off the setup and
  reported by `ignored_settings()`: that endpoint refuses each with 1007.
  Live Avatar is a Vertex AI feature; Google AI's `gemini-3.8-live` refuses the
  `VIDEO` modality. `LiveModelProfile` gained `thinking_level_required`.
- **Wire types gained Gemini 3.8 Live fields** (source-breaking for struct
  literals): `InputAudioTranscription {}` / `OutputAudioTranscription {}` are
  now aliases of the `#[non_exhaustive]` `AudioTranscriptionConfig` — write
  `::default()`; `VoiceConfig` has `replicated_voice_config`;
  `SessionResumptionConfig` has `transparent` (both derive `Default`);
  `SessionConfig` has `avatar_config`, `history_config` and
  `explicit_vad_signal`; `DeliveryConfig` has `media`; `EventCallbacks` has
  `on_media`. `Modality` gained `Video`, and `VoiceActivityType` is
  `#[non_exhaustive]` with an `Unspecified` variant.
- `SessionConfig::supports_async_tools()` is now also true for Gemini 3.8 Live
  on Vertex AI, and `supports_thinking()` is false for Gemini 3.8 Live on
  either platform (true for the extended-thinking variant on both).
- The workspace dependency on `gemini-adk-fluent-rs` has default features off,
  like L0 and L1, so the `gemini-adk` facade decides them; workspace members
  that inherit it say `default-features = true`.
- **Documentation leads with the golden path.** The README's text-agent
  section is a ladder — ask, a streamed conversation, a typed answer, a tool,
  a model-free test — whose every program is a compiled quickstart target
  checked against the README; the fluent crate README, the text-agent, tools,
  tool-policy and best-practice guides and the agent reference use
  `GeminiLlm::from_env`, `ask`/`chat`/`stream`, `#[tool]` and `MockLlm`, and no
  longer show `T::simple` with arguments it never declares.
- `LlmError` and `AgentError` are `#[non_exhaustive]`, and
  `LlmError::ContentFiltered` carries the provider's reason.
- **The fluent prelude no longer glob-imports the L0 prelude.** It named
  about 200 wire, transport, buffer and turn-detection types in every
  application's namespace. It now carries the L0 types an application names —
  content (`Content`, `Part`, `Role`, `FunctionCall`, …), models and voices
  (`ModelId`, `Voice`, `Modality`), the arguments of `Live` builder methods
  (`ActivityHandling`, `AutomaticActivityDetection`, `FunctionCallingBehavior`,
  …) and safety settings — plus `futures_util::StreamExt` for consuming
  `agent.stream(..)`. Everything else is one import away in
  `gemini_adk_fluent_rs::wire`.
- `ToolError`, `Composable` and `LiveViolation` are `#[non_exhaustive]`; a
  `match` on them needs a `_` arm. `ToolError` gained `Declined`, and
  `Composable` gained `Branch`.
- `LlmRequest` has new fields; build it with `..Default::default()` or
  `LlmRequest::from_text`. `GenerationConfig` gained `stop_sequences`.
- `LlmTextAgent::new` and `AgentBuilder::build` take `impl BaseLlm + 'static`
  instead of `Arc<dyn BaseLlm>`. Existing calls that pass an `Arc` still
  compile; a bare `GeminiLlm` or `MockLlm` no longer needs wrapping.
- `Resume::Restart` is documented for what it does: it restarts the main
  flow's monitor against the existing state, not the business task.
- The Governed Flows guide has a "Digressions and repair: the flow stack"
  section; the glossary defines flow stack, digression, stage/step/phase and
  instruction.

- **The documentation website is now Astro + Starlight** (`apps/docs`),
  replacing mdBook. Content is not authored in the app: every page is synced
  from `docs/` at build time, `docs/src/SUMMARY.md` remains the single source
  of structure (its headings are the sidebar groups), and the rustdoc API
  reference is merged into `/api` on deploy as before. The theme is the
  Modernist Functionalism system shared with `anvil`. Every mdBook-era
  `.html` URL redirects to the page's new route, so nothing that linked to
  the book breaks. `just docs-site` serves it locally (Node 22).
- **docs.rs now documents every feature it can build.** No crate had
  `[package.metadata.docs.rs]`, so docs.rs showed default features only — no
  `vad`, `otel`, `vertex-ai-sessions`, `dsp`, `denoise`, `sip`, and no badge to
  say they existed. All five library crates declare it, with `doc_cfg` badges;
  the fluent crate lists every feature except `voice-io` (`cpal` needs ALSA
  headers the docs.rs builder lacks) and a test keeps that list in step with
  `[features]`. The deployed API reference is built with `--all-features` too —
  it had installed ALSA for that purpose and then not passed the flag.
- Flow Studio: **Open** (⌘O / Ctrl+O, or drop a `.json` on the canvas) is the
  inverse of Download that was missing; **Delete** removes the selected step,
  **Esc** deselects / leaves preview / closes diagnostics, **⌘↵** validates,
  **⌘S** downloads. Nothing fires while a field has focus.
- `just` alone lists recipes instead of running `setup` (a full workspace
  build); `just run-studio` starts the web UI and prints the Studio URL.
- READMEs for the `audiohook`, `sip-agent`, `telephony`, `quickstart` and
  `voice-spec-demo` examples; an orientation page for `gemini-adk-rs` on
  docs.rs; the book's API reference page lists all five crates and says when
  to read docs.rs versus the site.
- The README and the book landing said **v1.0** for a day after 2.0.0 shipped;
  a drift test now holds them to the released major.minor.


### Features

- feat(devex): the small things a first look hits — docs.rs, Studio keys, READMEs
### Documentation

- docs: overhaul guides and shared agent instructions (#62)
- docs: clarify product outcomes, first-use paths, and boundaries (#61)
- docs: de-Claude gemini-rs README (#60)
- docs(site): Astro + Starlight website synced from docs/, replacing mdBook (#58)
### Other

- Support Gemini 3.8 Live and fix four wire encodings
- Flow stack in the runtime, plus a developer-experience overhaul: ask/chat/stream, typed output, #[tool], honest configuration (#69)
- Update version from 1.0 to 2.0 in README

## [2.0.0] - 2026-09-03

### Highlights

2.0 is the SDK-surface cut: the crates expose one name per concept, one
shape per idea, and defaults that work out of the box. Every rename, removal
and shape change is tabulated old → new in
[`docs/user-guide/migration.md`](docs/user-guide/migration.md).

- **Models.** `ModelId` (a string newtype with rolling-alias constants) replaces
  the `GeminiModel` enum; leave the model unset and connect resolves a
  platform default (`GEMINI_LIVE_MODEL`, then `GEMINI_MODEL`; text agents read
  `GEMINI_TEXT_MODEL` first, so one shared variable can no longer 404 every
  `generateContent` call).
- **Wire (L0).** One connect path (`connect(config)` / `ConnectBuilder`);
  typed `SessionEvent::Error(SessionError)` and `GoAway(Option<Duration>)`;
  `Bytes` audio both ways; `AccessToken` so Vertex reconnects carry a fresh
  credential; the REST key travels in a header, not the URL; an `rtrb`-backed
  SPSC ring split into `Send`-but-not-`Sync` halves; no `unsafe` left.
- **Runtime (L1).** One name per concept (`ToolSurface`, `Enforcement`,
  `ExecutionMode`, `StatePredicate`, `TextRunner`, `on_session_phase`, …);
  `ConfigError` instead of panics; `State::try_get` tells wrong-type from
  absent; typed `PersistenceError`; lossless session-event replay.
- **Fluent (L2).** `gemini-llm` is a default feature; `tools(..)` / `tool(f)`
  on both `Live` and `AgentBuilder`; `build` / `compile` return `Result`;
  one bool-setter rule (`transcription()`, `session_resume()`,
  `no_tool_advisory()`, `no_context_compression()`); `#[tool]` works from
  the prelude with a single dependency.
- **Live sessions.** Per-turn response latency as a distribution
  (`telemetry().latency()`: p50/p90/p99 and a histogram); one `turn` tracing
  span per turn across VAD, tools and the turn boundary; context window
  compression on by default so long calls keep going.
- **Examples and docs.** The flagship examples are L2 programs; the README
  quickstart is compiled in CI; edition 2024, `forbid(unsafe_code)` in every
  crate.


### Features

- feat(live): per-turn response latency, one span per turn, compression on by default (#42)
- feat(examples): redteam-call — a governed collections agent against an adversarial Live caller (#40)
- feat: salvage the unmerged tail of the roadmap branch — Conversation CI, resolver registry, Python wheel, run_stream (#34)
### Bug Fixes

- fix(live): publish the latency count after its sample, not before (#43)
- fix: post-merge Codex findings on #37 — release blocker, replay fidelity, quickstart promise (#39)
- fix: post-merge Codex findings + devex overhaul — a first-run path that actually works (#37)
- fix(cli): vendor web assets inside the crate so the published package compiles (#36)
- fix(release): make gemini-adk-macros-rs publishable and add it to the publish chain (#35)
### Chores

- chore(release): v1.0.0 (#33)
### Other

- SDK surface 2.0: foundations, ModelId, typed events, one connect path, refreshing credentials (#41)
- Update CodeQL workflow name
- Add comment for Dependabot configuration

## [1.0.0] - 2026-08-30

### Bug Fixes

- fix(ci): install ALSA headers before the all-features semver check (#32)
### Chores

- chore(release): v0.9.0 (#31)

## [0.9.0] - 2026-08-30

### Features

- feat(dsp): engineer-grade mic chain + decision-level effectiveness bench (AEC, WOLA STFT, AGC, limiter, sinc resampler) (#30)
- feat: audio hardening stack + Flow Studio mechanisms, bug-bash fixes, ADK parity features (#29)
- feat(voice): production hardening — redaction, DTMF over SIP, latency filler, warm handoff, mic chain, third connector (#28)
- feat(flow): Flow Studio + SessionSpec — author whole governed sessions as JSON, with a drag-and-drop editor (#25)
- feat(memory): split the extraction models; Flash Lite for transcripts (#24)
- feat(memory): contextual memory engine for Gemini Live sessions (#21)
### Bug Fixes

- fix: typed turns reach extractors; TypedTool declarations the API accepts; Live e2e coverage (#22)
### Documentation

- docs: straitjacket-register overhaul — README, Flow Studio visuals, new chapters, TTS-driven governed-call demo (#26)
- docs(site): world-class GH Pages brand refresh (phase 1) (#27)

_Nothing yet._

## [0.8.0] - 2026-06-12

### Added

- **`LiveHandle::stream()`** — semantic events as a `futures::Stream`. The new
  `LiveEventStream` wraps the `events()` broadcast receiver: lagged (missed)
  events are skipped and the stream continues, and the stream ends when the
  session's event channel closes. Composes with all `futures`/`tokio-stream`
  combinators (`while let Some(ev) = stream.next().await { … }`). Exported from
  `gemini_adk_rs::live` and `gemini_adk_fluent_rs::live` (not the kernel
  prelude). Also new: `LiveEvent::ToolCancelled { ids }`,
  `LiveHandle::resume_handle()`, and the L2 `session_resume_from(handle)`
  builder setter (see Fixed below).

- **Wire recording (`RecordingCodec`).** Any `Codec` can be wrapped to record
  every wire byte in both directions — monotonic sequence, direction, and
  epoch-millis timestamp per `WireEntry`, delivered synchronously to a
  `WireRecorder`. Built-in backends: `FileWireRecorder` (JSONL, base64
  payloads, periodic + on-drop flush) and `MemoryWireRecorder`. Install via
  `SessionConfig::record_wire(..)` / `ConnectBuilder::record_wire(..)` at L0,
  or `Live::builder().record_wire(path)` / `.wire_recorder(..)` at L2.
- **Durable `JournalSink` for state mutations.** The in-memory mutation
  journal stays a bounded ring (1024 entries, still serving `evidence()`);
  `State::set_journal_sink(..)` / `with_journal_sink(..)` additionally stream
  every mutation to a sync sink — `FileJournalSink` (JSONL, buffered, periodic
  + on-drop flush) or `MemoryJournalSink`. `StateMutation` is now serde
  round-trippable (`timestamp_ms` epoch millis).
- **Replay harness — any session replayable through the real control plane.**
  `gemini_genai_rs::transport::replay::ReplayTransport` replays a recorded
  wire log's inbound frames (gated until `ReplayControl::release()`, drained
  signal, outbound frames collected for comparison);
  `gemini_adk_rs::live::replay::{replay_session, attach_session}` drive the
  log through the REAL three-lane processor — phases, extractors, watchers,
  and tool dispatch all run for real. A closed-loop integration test
  (`crates/gemini-adk-rs/tests/replay_closed_loop.rs`) records a scripted
  session (text exchange, dispatched tool call + response, turn completes)
  and asserts the replay reproduces per-lane `LiveEvent` sequences, final
  state, journal per-key values, and byte-identical setup/tool-response
  frames.
- **`adk session replay <wire-log> [--journal <journal-log>]`.** Offline
  replay through the L1 processor with default callbacks: turn-by-turn
  summary (events, tool calls, final state keys) and, with `--journal`, a
  CLEAN/DRIFT diff of the recorded journal against the replayed final state
  (non-zero exit on drift). Replay only re-processes recorded frames — no LLM
  or tool re-execution. See `docs/user-guide/record-replay.md`.

### Fixed

- **Background tools are cancelled on disconnect.** The `BackgroundToolTracker`
  is now carried through `SessionRuntime` into `LiveHandle`, and
  `LiveHandle::disconnect()` cancels every tracked background tool task
  (cooperative token + task abort) before closing the L0 session. Previously the
  tracker was only reachable from the control lane, so orphaned tool tasks kept
  running after disconnect and could post stale `ToolCompleted` events to a dead
  (or new) control lane.

- **Fast/control/telemetry lanes are shut down on disconnect.**
  `LiveHandle::disconnect()` now grace-awaits the fast and control lanes
  (250 ms each) and aborts whatever is still stuck, and cancels the telemetry
  lane's `CancellationToken`. The event router also exits after routing the
  terminal `Disconnected` event (closing the lane channels so the lanes can
  drain and shut down gracefully). Previously the lane `JoinHandle`s were
  detached on construction — a lane blocked in a slow tool ran forever.

- **`FsPersistence::save` is atomic (tmp + rename).** Snapshots are written to
  a sibling `<session_id>.json.tmp` and renamed over the destination —
  `rename(2)` is atomic on the same filesystem, so a crash mid-write or a
  concurrent `load` can no longer observe a torn half-written snapshot.

- **Barge-in beats slow inline tools.** Inline tool dispatch in the control
  lane now races the tool future against a barge-in `CancellationToken` that
  the event router cancels the moment an `Interrupted` event arrives (the
  control lane re-arms it after processing the interruption). Previously an
  interruption queued behind the blocking dispatch and waited for the tool to
  finish. On cancellation the tool future is dropped at its current await
  point (tools must be drop-safe), **no** `FunctionResponse` is sent for the
  cancelled call, the governed-flow `ToolGate` is not advanced, and the new
  `LiveEvent::ToolCancelled { ids }` is emitted (also emitted for server-sent
  `ToolCallCancelled` events).

- **Graceful drain on control-lane exit + manual GoAway resume surface.** When
  the control lane shuts down it now (1) best-effort flushes any deferred
  context still queued in `PendingContext` (previously silently dropped on
  disconnect) and (2) runs a final persistence snapshot **synchronously** — the
  per-turn save is spawn-and-forget and could lose the last turn when the
  process exited right after disconnect. New `LiveHandle::resume_handle()`
  exposes the latest server-issued session-resumption handle, and the L2
  builder gained `session_resume_from(handle)`, so callers can manually resume
  after a `GoAway` (no auto-reconnect). Documented in
  `docs/user-guide/session-persistence.md`.

- **Control channel depth raised 64 → 512.** Control events are routed with a
  lossless `send().await`, so a full control queue blocks the shared event
  router — which then stops forwarding audio frames and causes playback
  glitches. Transcript-chunk accumulation flows through this channel, so 64
  slots could realistically fill behind a slow control-lane consumer.

### Changed

- **Turn lifecycle decomposed into named, tested stages (#4).** The live hot-path
  `handle_turn_complete` god-function was lifted, one behavior-preserving block at
  a time, into named async stage helpers (`run_turn_extractors`,
  `evaluate_phase_transition`, `project_tool_advisory`, `evaluate_repair`,
  `project_steering_context`, `govern_flow`, `deliver_instruction_and_context`). A
  deterministic harness drives the real `handle_turn_complete` through a recording
  `SessionWriter`, turning the documented ordering "scars" (single-send + dedup,
  batched/deferred context, turn reset) and each stage's effect into asserted
  invariants. No behavior change. See
  `docs/plans/2026-06-07-turn-tool-pipeline-rfc.md`.
- **Background tools advance the governed flow (#7).** Tool completions now pass
  through a single `ToolGate::observe_completion(call_id, …)` — idempotent per
  `call_id` — for both inline and background tools. Background tools (which run
  detached and can't reach the synchronous `FlowMonitor`) post a
  `ControlEvent::ToolCompleted` back to the control lane, which routes them through
  the same gate. This closes the prior fracture where background tools could only
  be gated indirectly on delivered state: `done(called_ok(..))` now works for
  background tools too. A `before_tool` veto posts no completion, so vetoed tools
  never advance the flow — matching the inline path.
- **CI: feature-boundary checks.** Added a job that builds the workspace with
  `--no-default-features` and `--all-features` (both verified green), so the
  feature-heavy SDK can't regress at the extremes. Also: `await_holding_lock` is
  now enforced (removed from the workspace lint allow-list).
- **Clippy smoke alarms back on.** The four remaining workspace-wide lint
  allows (`type_complexity`, `too_many_arguments`, `field_reassign_with_default`,
  `new_ret_no_self`) are removed. Boxed callback shapes got named public type
  aliases (`AudioCallback`, `TranscriptCallback`, `ToolCallCallback`,
  `PhaseHook`, `StateGuard`, …), test sites use struct literals, and the few
  deliberate exceptions (control-plane plumbing functions, builder entry
  points) carry targeted `#[allow(lint, reason = "…")]` at the site.
- **CI/release ratchet.** New `cargo hack check --each-feature` job (every
  feature of the published crates compiles in isolation), a `cargo deny` job
  (RustSec advisories, permissive-license allow-list, source whitelist — config
  in `deny.toml`), `cargo semver-checks` in the release validate job (declared
  bump must cover the real API delta), and crates are now published **with**
  tarball verification — the `--no-verify` escape hatch is gone (dependencies
  are already live on crates.io when each crate publishes).
- **Proc-macro hygiene.** The `#[tool]` macro now routes its generated code
  through `gemini_adk_rs::__macros` (re-exporting `serde`/`schemars`/`async_trait`/
  `serde_json`) and sets `#[serde(crate = ..)]`, so downstream crates no longer
  need those upstream crates as direct dependencies under those exact names.

### Changed (breaking)

- **`#[non_exhaustive]` on the wire-facing enums.** `SessionEvent`,
  `LiveEvent`, `GeminiModel`, and `Voice` will all grow as Google ships new
  models and server events; marking them non-exhaustive makes those additions
  semver-compatible instead of breaking releases. Downstream `match`es need a
  wildcard arm; the event router surfaces unknown wire events at debug level
  instead of silently dropping them.
- **Feature diet: slim defaults, selectable TLS, targeted tokio.**
  `gemini-genai-rs` default features are now `["live", "tls-native"]` — the ML
  VAD model (`vad-wavekat`) and the tracing *subscriber* are no longer pulled by
  default. The TLS backend is selectable (`tls-native` default, `tls-rustls`
  opt-in; `reqwest` follows the same choice), and all three published crates
  depend on targeted `tokio` features instead of `tokio/full` (tests keep `full`
  via dev-dependencies). The `tracing` facade is now an unconditional (tiny)
  dependency — transport spans/events always compile and are no-ops without a
  subscriber — and the new `tracing-subscriber` feature gates the fmt/EnvFilter
  machinery behind `TelemetryConfig::init`. `tracing-support` is now a
  deprecated no-op feature kept one release for manifest compatibility.
  `gemini-adk-rs` explicitly requires `gemini-genai-rs/vad` (it always used it),
  and L1/L2 grew `vad-wavekat`/`tls-rustls` passthrough features so applications
  don't need a direct lower-layer dependency to opt in.
- **`reqwest` is now optional; the REST modules are feature-gated.** The default
  `gemini-adk-rs` build no longer compiles `reqwest`. A new `http` feature pulls it,
  and the REST-backed areas now actually gate their modules (fixing "feature
  declared but not wired"): `vertex-ai-code-executor`, `vertex-ai-sessions`,
  `vertex-ai-rag` (new — RAG retrieval tool + memory service), `mcp-http` (the SSE
  transport; stdio MCP still works without it), and `gcs-artifacts` each enable
  `http`. `VertexAiCodeExecutor`, `VertexAiRag*`, and the MCP HTTP path are behind
  their features; enable the feature (or `--all-features`) to use them.
- **Reactor: dead effect nouns removed.** Dropped `EffectPolicy::dedupe_key` and
  `cancel_scope` (never set or read by any rule) and `LiveEffect::TransitionPhase`
  (never produced; executor no-op'd it) — per the "make it real or delete it"
  principle, they were deleted rather than left as aspirational fields. Concurrent
  effect failures are now **supervised**: an error surfaces as `LiveEvent::Error`
  instead of being silently discarded.
- **`State` writes are now fallible.** `State::set`, `set_committed`, `set_key`,
  `modify`, and `PrefixedState::set` return `Result<_, StateError>` instead of
  panicking via `expect` on non-serializable input — a public SDK write no longer
  aborts the host process. Call sites must handle the `Result`.
- **`flow::Mode` renamed to `flow::Enforcement`** (`Enforce`/`Observe`) to remove
  the collision with `orchestration::Mode` (`Call`/`Dispatch`/`Background`). A
  deprecated `flow::Mode` alias is kept for one release; the `FlowMode` prelude
  alias now points at `Enforcement`.

### Fixed

- **Server: sessions are created under the advertised id.** `POST /run` and
  `POST /run_sse` accepted a client-chosen `session_id` (and advertised it in
  responses/SSE events) but `SessionStore::create` generated its own UUID — so
  every `state()` read and `append_event` under the advertised id was a silent
  no-op and the returned session could not be fetched or continued. Both
  handlers now use the new idempotent `SessionStore::get_or_create(id, ..)`.
  Regression-tested.
- **Single-pass server-message parsing.** `ServerMessage::parse` now
  deserializes each frame once into a key-discriminated raw struct instead of
  up to seven `contains()` scans over the frame followed by a targeted
  re-parse. Behavior pinned by the golden-wire fixtures (including the
  `toolCallCancellation`-vs-`toolCall` substring trap, which is now structural).
- **`State::modify` is now atomic.** It performs the read-modify-write under a
  per-key map lock (`DashMap::entry`) instead of a racy `get`→`f`→`set`, so
  concurrent increments no longer lose updates.
- **Delta rollback is now correct.** Delta tracking uses tombstones
  (`DeltaOp::Put`/`Delete`): `remove()` and `clear_prefix()` no longer mutate the
  committed store, so `rollback()` reliably restores the base state after removals
  and prefix clears, and `commit()` applies removals.
- **`Flow` `Before` constraint is now enforced.** `before(a, b)` gates step
  eligibility (`b` cannot start until `a` is done); previously it was validated but
  never consulted at runtime.
- **Custom guards inside `Guard::all`/`any` are no longer silently dropped.** A
  nested `Guard::custom` is preserved as a runtime closure (making the combinator
  non-serializable) instead of being lowered to `Pred::Always`, which had silently
  deleted composed safety guards.
- **Metadata truth.** Crate READMEs and the main README license section corrected
  to MIT (matching `LICENSE`); install snippets bumped to `0.7`; documented MSRV
  aligned with CI (`rust-version = "1.93"`, README badge `1.93+`).

### Added

- **Producer-side audio send pacing.** `SessionConfig::audio_pacing(BackpressureConfig)`
  installs a shared token bucket on `SessionHandle::send_audio`: callers pushing
  audio faster than the sustained rate wait at the producer instead of
  overflowing the send queue (the previously-orphaned `TokenBucket` now earns
  its keep). Off by default; receives are never stalled by pacing.
- **Golden-wire protocol tests.** Checked-in JSON fixtures pin the wire format
  in both directions: client→server messages (setup, realtime audio, client
  content, tool responses) are serialized and diffed against blessed fixtures
  (`GOLDEN_BLESS=1` to re-bless intentional changes), and server→client frames
  (serverContent with audio/text/thought parts, transcriptions, toolCall,
  toolCallCancellation, goAway, sessionResumptionUpdate, setupComplete) are
  hand-written contract fixtures asserted to parse correctly. Platform deltas
  are pinned explicitly: Vertex AI's qualified model URI and its stripping of
  `behavior`/`thinkingConfig`/`scheduling`, plus the
  `GeminiModel::Custom`/`Voice::Custom` forward-compatibility escape hatches.
- **Server: real SSE streaming + debug/eval polish.** `POST /run_sse` now
  streams real execution milestones (`started`, `agent_started/completed`,
  `tool_call_started/completed/failed`, final `response`) instead of returning a
  hardcoded fake string; granularity note: `BaseLlm` has no token-level
  streaming API, so the endpoint streams real lifecycle events rather than
  fabricated token chunks. Also: `GET /debug/traces` (list recorded traces),
  HTTP 400 on malformed artifact versions (was `unwrap_or(0)`), and
  `limit`/`offset` pagination on `GET /eval/results`. Covered by integration
  tests with a mock LLM.
- **`adk flow` devtools.** A CLI command group over a serializable
  `ConversationSpec`: `adk flow inspect <spec.json>` (stages/tools/digressions/
  policies/redaction summary), `adk flow graph <spec.json>` (Mermaid diagram), and
  `adk flow simulate <spec.json> <scenario.json>` (run a model-free scenario, PASS/
  FAIL). Closes the draft → inspect → simulate authoring loop with no live API.
- **`conversation-from-script` skill.** A Claude Code skill
  (`.claude/skills/conversation-from-script/`) that drafts a serializable
  `ConversationSpec` + simulation `Scenario` tests from a call-center script/SOP —
  an authoring assistant (the model drafts; the deterministic control plane
  governs). Its example spec/scenario JSON are validated by an integration test so
  the guidance can't drift from what the compiler accepts.
- **Policy aspects.** Reusable, cross-cutting governance attached to a whole
  conversation via `Conversation::policy(..)`: `Policy::safety_handoff([intents])`
  (lowers to a `safety` digression that terminates on `intent:{name}`),
  `Policy::redact([keys])` (recorded for the runtime's logging; surfaced via
  `CompiledConversation::redacted_fields()`), and `Policy::commit(tool)
  .idempotency_key(..).compensate_with(..)` (commit governance metadata). All
  serializable and round-trip through JSON.
- **Typed graph macro.** `voice_flow! { mod booking { steps: [..]; tools: [..];
  slots: [..]; } }` generates a module of compile-time-checked `&str` name
  constants, so flow code references `booking::collect` etc. — a typo'd name is a
  build error, not a silently never-matching guard. (Full declarative DSL body is a
  follow-up; this is the name-checking core.)
- **Repair flows first-class.** A serializable per-stage `RepairPolicy`
  (`reprompt_after`/`escalate_after`/`escalate_to`) via `Conversation::repair(..)`.
  The runtime raises `repair:{stage}:reprompt` once a stage has been active too long
  without completing and `repair:{stage}:escalate` after the escalate threshold;
  when `escalate_to` is set, escalation also completes the stage and routes there
  (deterministic "give up and hand off"). Signals clear when the stage leaves.
- **Motif stdlib.** `Motif` factories for high-confidence flow fragments —
  `collect_frame::<F>` / `confirm_then_commit` / `identity_verification` /
  `disclosure` / `say` / `handoff` (→ `StageSpec`) and `faq_digression`
  (→ `OverlaySpec`) — composed via new `Conversation::add_stage` / `add_overlay`.
  Motifs lower through the validated IR (a mis-built commit motif fails `compile()`
  like a hand-written one).
- **Model-free simulation harness.** A deterministic `Sim` drives a compiled
  conversation with no live API: a fake user speaks (`sim.user(text)` runs the
  conversation's recognizers to fill slots, respecting validators), slots can be
  set directly, tools succeed on demand or after a latency (`schedule_tool`), and
  the `FlowStack` advances turn by turn. Introspect with `active`/`allowed`/
  `denied`/`slot`/`is_complete`/`explain`. A serializable `Scenario` (`SimStep`s:
  `user`/`set`/`tool_ok`/`turn`/`expect_*`) runs as a data-driven test (YAML/JSON)
  and reports the failing step. `Extract::field_state_keys()` exposes the
  field→state-key mapping for promotion.
- **Hierarchical digressions / statecharts above the DAG.** Conversations can now
  declare **overlays** — named sub-flows triggered by a guard that suspend the main
  flow, run, and resume: `Conversation::overlay(name).trigger(g).stage(..).resume(..)
  .done_overlay()`. A serializable `OverlaySpec` (round-trips through JSON) lowers to
  its own validated `CompiledFlow`. A new runtime `FlowStack` (`CompiledConversation::
  stack(mode)`) drives the main flow plus at most one active digression with
  push-on-trigger / resume-on-completion (`Resume::Previous`/`Restart`/`Terminate`);
  tool admission, postures, and `explain()` delegate to the active layer.
  `FlowMonitor::eval(guard, state)` exposes guard evaluation for triggers.
- **`Live::converse(&conversation)`** — one-liner that governs a Live session with
  a compiled conversation's flow and registers the extractors that fill its
  frames' slots (`converse_observe` for observe mode).
- **Slot validation.** A serializable `SlotValidator` (`Range`/`NonEmpty`/`Regex`/
  `OneOf`) on slots; `#[slot(min=…, max=…, non_empty)]` in the derive. A recognized
  value failing its validator is rejected (the slot stays unfilled). Extract gains
  `ExtractBuilder::validate(predicate)` to attach a post-recognition check.
- **Resolver-filled slots.** `Conversation::resolve_slot(name, args, ttl, fetch)`
  fills a slot from an async fetch/agent (bound from `State`), lowering to an
  Extract resolver field. The closure stays builder-only, so `ConversationSpec`
  remains serializable.
- **Typed frames & slots.** A `Frame` trait + `FrameSpec`/`SlotSpec`/`ConfirmPolicy`/
  `SlotRecognizer` and a `#[derive(Frame)]` macro: declare a struct with
  `#[slot(prompt=…, reprompt=…, confirm=…, state=…, pii)]` and `#[recognize(…)]`
  fields and get the slot definition (keys, prompts, confirmation policy, PII
  flags, recognizers). `FrameSpec::to_extract()` lowers recognizer-bearing slots
  to an `Extract` record. `Conversation::collect_frame::<F>()` collects a frame's
  slots in a stage (drives the `captured` completion) **and** lowers its
  extractor — `CompiledConversation::extractors()` exposes the extractors that
  fill the slots from the transcript each turn.
- **Recognizer confidence reaches state.** Deterministic extraction now records
  `state_meta:{key}` = `{source: "extraction", confidence}` when a recognizer
  matches, so `State::evidence()` surfaces real per-slot confidence.
- **Slot evidence.** `State::evidence(key) -> SlotEvidence` aggregates a slot's
  current value, provenance (`state_meta:{key}.source`), confidence, and the most
  recent journal write — the basis for principled confirmations ("I heard 6,
  right?") and stale/low-confidence repair. `StateMutationOrigin` is now serde.
- **Conversation compiler (Phase 1 MVP).** A serializable `ConversationSpec` and
  a fluent `Conversation` builder (sugar over the spec) that **compile down to a
  governed `CompiledFlow`** via `Conversation::compile() -> CompiledConversation`.
  Authors describe stages that `say`/`ground`, `collect` slots, `commit` tools
  behind confirmation, and advance via `next(to, when)`; the compiler lowers these
  to Flow steps, gates, postures, grounding, tool whitelists, and commit
  constraints. The spec round-trips through JSON (YAML/hot-reload follows for free)
  and `CompiledConversation::monitor()` yields a ready `FlowMonitor`.
- **`Flow::compile() -> Result<CompiledFlow, FlowErrors>`** — the validated flow
  IR the conversation compiler targets. On top of `validate()` it reports
  unreachable steps and effectively-unguarded commit tools (`FlowError`), and
  precomputes a `ToolPolicy` (the tool universe). `FlowMonitor::compiled` /
  `try_new` construct from it; `new` remains for in-process trusted flows.
- **`FlowMonitor::explain()` / `why_blocked()`** returning a serializable
  `FlowExplanation` (active steps, allowed/blocked tools with reasons, unmet
  requirements) — the deterministic answer to "why did the assistant ask that?".
- **Conversation-compiler RFC** (`docs/plans/2026-06-06-conversation-compiler-rfc.md`)
  — the plan to author voice behavior (slots/confirm/repair/digress/commit) and
  compile it down to Flow + Extract + Resolver + Reactor; locks
  serializable-spec-first and the one-control-structure rule.
- `State` property test (rollback always restores base) and regression tests for
  atomic `modify`, rollback-after-remove, and rollback-after-clear-prefix; `Flow`
  regression tests for `Before` enforcement and custom-guard preservation.
- **`ROADMAP.md`** — milestone-based plan for post-0.7.0 work, reframed around
  hardening the primitives into contracts.
- **Eval REST endpoint wired to `gemini_adk_rs::evaluation`** — `POST /eval/run` now loads an `EvalSet` (inline JSON or file path), maps criteria → deterministic evaluators (`response_match`/`exact_match`/`tool_trajectory`[`_any_order`], with optional `name=threshold`), scores each case (pre-recorded actuals, or live agent runs when actuals are absent), and aggregates a real `EvalResultSummary`. Results are stored on `ServerState` and served from `GET /eval/results`.

## [0.7.0] - 2026-05-31

### Added

- **Governed Agents** — three lenses over a shared State+result core (`{name}:result` + `state_meta:` provenance), composing multiplicatively:
  - **Flow** — governed conversation/tool DAG (`Flow`/`Step`/`Guard`, closed serializable predicate atoms). `FlowMonitor` keeps a token-replay `Marking`, gates tool calls (`once`/`never…until`/allow-deny), projects active-step postures as steering, surfaces unmet `require`s as repair, and exposes `verdict`/`violations`/`to_mermaid()`. Wired into `Live` via `.govern()` / `.observe()`.
  - **`Effect::ground(template)`** — serializable, `State`-interpolated fact line (`{key}`, `{key?yes:no}`) projected while a step is active (anti-hallucination), via `render_ground`.
  - **Flow `on_enter(run(agent, mode))`** — a step runs an agent on activation (fire-once); result → `{step}:result`, completing a downstream step via `Guard::resolved`.
  - **Extract** — deterministic recognizers (`integer`/`integer_near`/`money`/`regex`/`one_of`/`fuzzy`/`yes_no`/`datetime`) fill `State` on the CPU; **`#[derive(Extract)]`** builds a record from struct fields.
  - **Async resolver field sources** — `Extract::field_resolve(name, args, ttl, fetch)` binds args from `State`, caches by `(field, args)` for a TTL, runs concurrently with recognizers; `TurnExtractor::extract_with_state` threads `State` through the pipeline.
  - **`Extract::on_complete(agent, mode)`** — dispatch a downstream agent when a record lands fields.
  - **Orchestration** — `Mode` (`Call`/`Dispatch`/`Background`) + **`Resolver`** (`agent`/`fetch`/`llm`): a named async value source whose inputs come from `State`; `resolve`/`dispatch`. **Provenance** recorded under `state_meta:{name}:result` and readable via `provenance(state, key)`.
- **Cookbook** re-centered on the higher-order capabilities; capstones `39_booking` and `40_screening` combine all three lenses (run with no credentials).
- **mdbook** — new *Agent Orchestration* chapter; updated *Extraction* and *Governed Flows* chapters; RFCs in `docs/plans/`.

## [0.6.0] - 2026-03-19

### Bug Fixes

- fix: drop --all-targets from release validation (avoids openssl-sys bench dep)
- fix: release script publish dry-run tolerance for first-time crate publishes
- fix: release script _section crash under set -e with empty changelog sections
### Refactors

- refactor: rename crates with -rs suffix for crates.io namespace clarity
### Style

- style: cargo fmt --all

## [0.5.0] - 2026-03-18

### Added
- **Namespace parity** (~70 new methods across all composition namespaces):
  - Guards (`G::`): `rate_limit`, `toxicity`, `grounded`, `hallucination`, `llm_judge`
  - Tools (`T::`): `agent`, `mcp`, `a2a`, `mock`, `openapi`, `search`, `schema`, `transform`
  - Middleware (`M::`): `fallback_model`, `cache`, `dedup`, `metrics`, agent/model hooks
  - Prompt (`P::`): `reorder`, `only`, `without`, `compress`, `adapt`, `scaffolded`, `versioned`
  - Context (`C::`): `summarize`, `relevant`, `extract`, `distill`, `priority`, `fit`, `project`
  - State (`S::`): `log`, `unflatten`, `zip`, `group_by`, `history`, `validate`, `branch`
  - Eval (`E::`): `from_file`, `persona`
  - Artifacts (`A::`): `publish`, `save`, `load`, `list`, `delete`, `version`, JSON/text ops
- **30 cookbook examples** — progressive Crawl (01–10), Walk (11–20), Run (21–30) learning path:
  - Crawl: `simple_agent`, `agent_with_tools`, `callbacks`, `sequential_pipeline`, `parallel_fanout`, `loop_agent`, `state_transforms`, `prompt_composition`, `tool_composition`, `guards`
  - Walk: `route_branching`, `fallback_chain`, `review_loop`, `map_over`, `middleware_stack`, `context_engineering`, `evaluation_suite`, `artifacts`, `agent_tool`, `supervised`
  - Run: `full_algebra`, `contract_testing`, `deep_research`, `customer_support`, `code_review`, `dispatch_join`, `race_timeout`, `a2a_remote`, `live_voice`, `production_pipeline`
- **Web UI redesign**: Design system (80+ CSS tokens, Inter + JetBrains Mono), dark/light mode, animated landing page, architecture diagram, cookbook browser, operator algebra showcase, glassmorphism navigation
- **Cookbook browser panel** in DevTools UI
- **`gemini-adk-cli-rs` manifest fields**: `description`, `license`, `keywords`, `categories`, `repository` for crates.io compliance

### Changed
- All crate versions bumped from `0.4.0` → `0.5.0`
- Internal dependency versions updated (`gemini-genai-rs` and `gemini-adk-rs` constraints in downstream crates)
- Cookbook-to-example renaming across docs, configs, and source files
- Release workflow: publish steps now check crates.io API before uploading, skip if version already exists

### Fixed
- `cargo fmt` violations across cookbook examples and compose modules
- `gemini-adk-cli-rs` crates.io manifest verification failure (missing required fields)

## [0.4.0] - 2026-03-18

### Added
- **Workspace restructure**: Organized examples under `examples/` and interactive web UI under `apps/gemini-adk-web-rs/` to match upstream ADK convention
- **`gemini-adk-api-rs`**: Standalone REST API server for headless agent deployments
- **`gemini-adk-server-rs`**: Shared server library (agent loading, REST handlers, session management) used by both `gemini-adk-web-rs` and `gemini-adk-api-rs`
- **`gemini-adk-cli-rs`**: Full CLI tool with `create`, `run`, `web`, `eval`, `deploy`, and `api_server` subcommands
- **Evaluation framework** (`gemini-adk-rs`):
  - `EvalsetParser` — TOML-based eval set configuration
  - `HallucinationEvaluator` — detect hallucinated content in agent output
  - `RubricEvaluator` — score agent responses against grading rubrics
  - `SafetyEvaluator` — check agent output for safety policy violations
  - `UserSimulatorEvaluator` — simulate multi-turn user interactions
  - `TrajectoryMatchType` — exact, in-order, and any-order tool call sequence matching
  - `TestConfig` — test case configuration and execution
- **Session backends** (`gemini-adk-rs`): Postgres and Vertex AI session persistence
- **Agent configuration** (`gemini-adk-rs`): `AgentConfig` with full serialization support
- **Middleware module** (`gemini-adk-rs`): Middleware trait and composition pipeline
- **Telemetry** (`gemini-adk-rs`): Structured logging, metrics collection, span management, and setup utilities
- **Context module** (`gemini-adk-rs`): `InvocationContext` for agent execution context
- **Run configuration** (`gemini-adk-rs`): `RunConfig` for agent run parameters
- **Config-driven construction** (`gemini-adk-fluent-rs`): `AgentBuilder::from_config()` and `AgentBuilder::config()`
- Documentation: Comprehensive READMEs for `gemini-adk-web-rs`, `gemini-adk-api-rs`, and `gemini-adk-cli-rs`
- DevTools UI: Artifact panel, eval panel, event inspector panel, and trace panel

### Changed
- Workspace layout: standalone examples in `examples/`, web UI in `apps/gemini-adk-web-rs/`
- `gemini-adk-web-rs` now depends on `gemini-adk-server-rs` instead of inlining server logic
- All crate versions bumped from `0.1.0` → `0.4.0`

### Fixed
- `clippy::derivable_impls` on `TrajectoryMatchType` — replaced manual impl with `#[derive(Default)]`
- `clippy::print_literal` in `gemini-adk-cli-rs` eval output formatting
- Dead code warnings across workspace
- `cargo fmt` violations

## [0.1.0] - 2026-03-03

### Added
- Initial release of three-crate workspace
- **gemini-genai-rs** (L0): Wire protocol, WebSocket transport, `Codec`/`Transport`/`AuthProvider` traits, `SessionWriter`/`SessionReader`, structured errors, `Role` enum, `Content`/`Part` builders
- **gemini-adk-rs** (L1): Agent runtime with three-lane processor (fast/control/telemetry), `State` with prefix scoping (`session:`, `derived:`, `turn:`, `app:`, `user:`), `PhaseMachine` for conversation flow control, `ToolDispatcher` with `SimpleTool`/`TypedTool`, `ComputedRegistry` for derived state, `WatcherRegistry` for state change watchers, `TemporalRegistry` for temporal pattern detection, `SessionSignals` with atomic counters, `SessionTelemetry`, `BackgroundToolTracker`
- **gemini-adk-fluent-rs** (L2): Fluent builder API, S-C-T-P-M-A operator algebra for agent composition, `Middleware` trait and `MiddlewareChain`, pre-built patterns and contract validation
- ADK Web UI framework: multi-app Axum WebSocket tester with devtools panel
- Standalone examples: `text-chat`, `voice-chat`, `tool-calling`, `transcription`
- Agents examples: `weather-agent` and `research-pipeline` demos
- Support for both Google AI (API key) and Vertex AI (OAuth token) authentication
- Voice Activity Detection (VAD) with configurable settings
- Audio buffer management for bidirectional streaming
- `ConnectBuilder` for ergonomic session construction with generic `Transport` and `Codec`
