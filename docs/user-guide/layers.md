# The Layer Contract (L0 · L1 · L2)

Each layer exposes a `primitives` module for the types intended to be used by
the next layer. Contract tests detect changes to those exports. The `prelude`
is a convenience import; specialized APIs also live in named submodules.

| Layer | Responsibility |
|---|---|
| L0 | Authentication, protocol frames, transport and wire events |
| L1 | Session state, callbacks, tool dispatch and conversation control |
| L2 | Builders and composition APIs for applications |

```rust,ignore
use gemini_genai_rs::primitives::*;
use gemini_adk_rs::primitives::*;
use gemini_adk_fluent_rs::primitives::*;
```

## L0 — `gemini_genai_rs::primitives`: frames on a wire

**Promises:** a duplex, authenticated, resumable frame stream to the Live API
— connect, write frames, read events — plus the audio machinery a realtime
client needs at the edge.
**Never:** interprets a conversation, holds session state, dispatches a tool,
or decides when to speak.

| Concern | Primitives |
|---|---|
| Connect | `SessionConfig`, `connect`, `ConnectBuilder`, `ApiEndpoint`, `AccessToken`, `ResumeInfo` |
| Speak / listen | `SessionHandle`, `SessionWriter`, `SessionReader`, `SessionEvent`, `SessionPhase` |
| Say things | `Content`, `Part`, `Role`, `ModelId`, `Voice`, `Modality` |
| Tools on the wire | `Tool`, `FunctionDeclaration`, `FunctionCall`, `FunctionResponse`, `FunctionCallingBehavior`, `FunctionResponseScheduling` |
| Real time | `SpscRing`, `AudioJitterBuffer`, `bytes_to_i16`, `i16_to_bytes`, `BargeInDetector`, `TurnDetector` |
| Access | `AuthProvider`, `GoogleAIAuth`, `VertexAIAuth`, `Transport`, `TungsteniteTransport`, `Codec`, `JsonCodec` |
| Truth | `UsageMetadata`, `SessionError` |

## L1 — `gemini_adk_rs::primitives`: the conversation runtime

**Promises:** a concurrent session runtime over the L0 stream — typed shared
state, tool dispatch, governed flows with load-time compilation and a
self-explaining monitor, extraction that fills the state guards read, phases
and watchers, transcripts, persistence — and a handle for inspecting state and requesting control actions.
**Never:** opens its own idea of a socket beyond L0's transport, renders
application prose, or hides an enforcement decision — every denial carries
its reason, every stuck guard can print its atoms.

| Concern | Primitives |
|---|---|
| Shared truth | `State`, `StateKey`, `PrefixedState` |
| Capability | `ToolFunction`, `SimpleTool`, `TypedTool`, `ToolDispatcher` |
| Governance | `Flow`, `Step`, `Guard`, `Pred`, `Constraint`, `CompiledFlow`, `FlowMonitor`, `Enforcement`, `Marking`, `Verdict` |
| Explanation | `FlowExplanation`, `GuardTrace`, `Violation` |
| Understanding | `TurnExtractor`, `LlmExtractor`, `FieldPromotion`, `ExtractionTrigger` |
| Steering | `Phase`, `PhaseMachine`, `Transition`, `InstructionModifier`, `Watcher` |
| The session | `LiveSessionBuilder`, `LiveHandle`, `LiveEvent`, `EventCallbacks`, `TranscriptBuffer` |
| Memory of it | `SessionPersistence`, `SessionSnapshot`, `FsPersistence`, `MemoryPersistence` |
| Models | `BaseLlm`, `LlmRequest`, `LlmResponse` |

## L2 — `gemini_adk_fluent_rs::primitives`: authoring

**Promises:** two equivalent ways to state a session. In code: the `Live`
builder and `AgentBuilder` combinators, composed through eight one-letter
algebras — `S`tate `>>`, `C`ontext `+`, `T`ools `|`, `P`rompt `+`,
`M`iddleware `|`, `A`rtifacts `+`, `E`valuation `|`, `G`uards `|`. As data:
`SessionSpec`, the same session as one serializable document with load-time
validation and offline tests.
**Never:** invents runtime semantics. Every builder method lowers to an L1
primitive; every spec field lowers to a builder method. L2 adds *phrasing*,
not *behavior* — which is why the JSON document and the fluent chain stay
equivalent.

| Concern | Primitives |
|---|---|
| Voice session | `Live` (builder → connect → `LiveHandle`) |
| Text agents | `AgentBuilder`, `Pipeline` `>>`, `FanOut` `\|`, `*` loops, `until`, `/` fallback |
| The algebra | `S`, `C`, `T`, `P`, `M`, `A`, `E`, `G` |
| Session as data | `SessionSpec` (`.run_tests()`), `SpecResources`, `SpecTest` |
| Proof before connect | `check_contracts`, `ContractViolation` |
| Voice I/O | `voice::pump`, `voice::Playback`, `voice::Talk` *(feature `voice-io`)* |
| Telephony | `telephony::TwilioCall`, `telephony::sip::SipAgent` *(feature `sip`)*, `telephony::{g711, rtp, sdp}` — a phone call on the same pump |

## Voice applications: five lines to a conversation

The Live API speaks PCM16 — 16 kHz in, 24 kHz out. Everything between a
microphone and that contract (resampling, down-mix, playback buffering, and
barge-in: buffered speech must vanish the instant the user interrupts) is
plumbing every voice application needs and none should write. L2's `voice`
module is that plumbing, as two primitives:

**`Talk::talk()`** *(feature `voice-io`; Linux needs `libasound2-dev`)* — the
whole loop on the system's default devices:

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

Live::builder()
    .instruction("You are a helpful concierge.")
    .greeting("Greet the caller.")
    .govern(flow)
    .connect_from_env().await?
    .talk().await?;      // microphone in, speakers out, barge-in handled
```

**`voice::pump`** — the device-independent duplex core underneath, for any
audio backend (a telephony bridge, a browser gateway, a test harness): feed
microphone frames at any sample rate on one channel, receive `Playback`
instructions at any sample rate on another. Interruption arrives as an
explicit `Playback::Flush`, so stale audio is dropped, never played. The
resampler, the down-mix, and the event→playback policy are pure functions
with unit tests — the audio path is testable without a device or a session.

## Why this shape

Each layer's `primitives` module is a page of documentation that cannot rot:
its table *is* code, and its drift test references every named primitive, so
a rename or removal breaks the contract loudly at compile time — the same
philosophy as the flow compiler (fail at load, not live) applied to the SDK's
own architecture.

## See also

- [Architecture Overview](./architecture.md) — the processor lanes and data flow
- [Flows as JSON & the Flow Studio](./flow-json.md) — `SessionSpec`, the data half of L2
- [Live Sessions](./live-sessions.md) — the `Live` builder in depth
