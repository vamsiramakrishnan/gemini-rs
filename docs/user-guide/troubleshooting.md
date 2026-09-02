# Troubleshooting & FAQ

Common errors, their causes, and fixes. For the conceptual model see
[Architecture](./architecture.md); for imports see the
[migration guide's kernel/submodule map](./migration.md).

## Imports & the prelude

### "cannot find type/function … in this scope" after upgrading

The L2 `prelude` is a **kernel**, not an everything-glob. Most types are still
there, but advanced ones moved to focused submodules. Add the matching import:

| Missing | Add |
|---------|-----|
| `LiveEvent`, `RuntimeContract`, `FieldPromotion`, `SessionSnapshot`, `LiveSessionBuilder`, `ToolExecutionMode`, `*Contract` | `use gemini_adk_fluent_rs::live::*;` |
| `Toolset`, `ConfirmationProvider`, `Recognizer`, `FrameSpec`, `SlotSpec` | `use gemini_adk_fluent_rs::tools::*;` |
| `Conversation`, `ConversationSpec`, `CompiledConversation` | `use gemini_adk_fluent_rs::conversation::*;` |
| `call_agent`, `AgentMode`, `provenance`, `AgentTrait` | `use gemini_adk_fluent_rs::agents::*;` |
| `LlmRequest`, `LlmResponse`, `GeminiLlmParams`, `LlmRegistry` | `use gemini_adk_fluent_rs::llm::*;` |
| `SlotEvidence` | `use gemini_adk_fluent_rs::state::*;` |
| `CompiledFlow`, `StepAction`, `Violation` | `use gemini_adk_fluent_rs::flow::*;` |
| `Scenario`, `Sim`, `Motif` | `gemini_adk_fluent_rs::{simulation, motifs}` |

### "method `get_tools` not found … trait `Toolset` is not in scope"

A trait method needs the trait in scope. `use gemini_adk_fluent_rs::tools::Toolset;`.

### `Agent` is ambiguous / the trait and the builder collide

The L1 `Agent` *trait* is exported as **`AgentTrait`** (in `prelude` and
`agents`); `Agent` at L2 is the `AgentBuilder` alias. Use `AgentTrait` when you
mean the runtime trait.

## Live sessions

### No audio output / model returns text but I asked for audio

The native-audio Live models only support `Modality::Audio` output. For text,
call `.text_only()`.

### `on_thought` never fires

Thinking (`thinkingConfig`) is **Google AI only** — it's auto-stripped on
Vertex AI. Thought summaries also require `.include_thoughts()`.

### Audio glitches, stutters, or deadlocks during a session

A fast-lane callback (`on_audio`, `on_text`, `on_thought`, `on_vad_*`) is doing
too much. These run synchronously on the hot path and must complete in well under
a millisecond: no allocations, no locks, no `await`. Send across a channel with
`try_send` and do the work elsewhere. If a slow downstream consumer is the
problem, opt into a lossy [delivery policy](./live-callbacks.md) so the router
drops frames instead of stalling.

### Tool definitions won't update mid-session

By design — Live sessions only allow instruction updates after connect. Tool
declarations are fixed at connect time. Declare every tool up front.

### Vertex AI connection fails or frames look wrong

Use `wss://aiplatform.googleapis.com/...` (not `global-aiplatform…`). Vertex
sends Binary WebSocket frames (handled automatically). API versions differ
(`v1beta` for Google AI, `v1beta1` for Vertex) — `ApiEndpoint` handles it.

### Async tool calling (`NonBlocking` / scheduling) seems ignored

Async function calling is **Google AI only**. On Vertex these fields are stripped
from the wire automatically — set them unconditionally and check
`config.supports_async_tools()` at runtime.

## Builders & phases

### Phase builder doesn't compile / nothing happens

Each `.phase(...)` chain must end with `.done()`, and the machine needs an
explicit `.initial_phase("...")`.

### My phase instructions aren't being applied additively

`instruction_template` replaces the entire instruction. For additive
composition use `instruction_amendment` or phase modifiers (`P::with_state`,
`P::when`).

## Connecting & auth

### Where do credentials come from?

`.connect_from_env()` resolves Google AI vs Vertex from
`GOOGLE_GENAI_USE_VERTEXAI`, reads the standard env vars, and falls back to
`gcloud auth print-access-token` for Vertex. See
[Authentication & Connecting](./auth-and-connecting.md).

## FAQ

**Which crate should I import?** The highest one you need — start with
`gemini_adk_fluent_rs::prelude::*` and pull in submodules as the compiler asks.

**Where does `Flow` live?** `Flow`/`Guard`/`FlowMonitor` are **L1 runtime**
types surfaced in the L2 kernel prelude; the fuller flow vocabulary is in
`gemini_adk_fluent_rs::flow`.

**Can I run the cookbook examples without credentials?** The Crawl foundations
(`01-foundations`, `02-combinators`, `03-composition`) and other
construction-only examples run with no credentials. Live examples read auth from
the environment.

**How do I capture the model's full response even when the user interrupts?**
Use `on_generation_complete` / `.extract_on_generation::<T>(...)` — it fires
before interruption truncation.
