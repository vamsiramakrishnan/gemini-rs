# Decision record: developer experience and packaging

Status: accepted · 2026-09-23 · complements `2026-06-11-100x-strategy-memo.md`

## The finding

The June memo is right about the moat: enforced governance, model-free
simulation and a speech-to-speech substrate, in one open-source Rust runtime.
It is silent on the funnel, and the funnel leaks in the first minute.

Five audits of this workspace (first hour, public surface, tools and
structured output, errors and testing, packaging) agree on one shape. The SDK
is very wide — about 5,100 public items, 45 error types, a prelude of roughly
240 names documented as "~30" — and the path every evaluator takes first is
broken underneath:

- `AgentBuilder::build` silently drops `model`, `top_p`, `top_k`,
  `stop_sequences`, `thinking`, `output_schema` and every built-in tool. A
  user who changes the model gets the default model and no error.
- `#[tool]` sends raw `schemars` output, so an `Option<String>` parameter
  becomes `"type": ["string", "null"]`, which the API rejects — on Live, by
  closing the socket during setup.
- `T::simple` declares no parameters, so the model is told the tool takes
  none.
- Asking one question takes a `State`, a magic `"input"` key that is empty
  on a typo, an `Arc`, and eight concepts. There is no multi-turn text chat,
  no text streaming and no typed output.
- A missing API key on the text path is not an error until the first request,
  and then it arrives as a string, flattened twice, with the status code gone.
- `conditional()` does not branch. `Live::dispatcher()` discards tools
  registered before it. A built agent cannot be passed back into the
  composition APIs, so the cookbook casts seven times.
- There is no public mock model; the tests hand-roll seventeen.
- The scaffold (`adk create`) emits a project that does not compile.

A Rust developer who meets any of these before reaching a Conversation, a
Live session or `Sim` leaves with the wrong opinion of the thing that is
actually unique. Adding surface does not fix that. A small, honest, complete
core with progressive disclosure does, and so does pruning the surface around
it.

## Principles, and where they come from

Each principle is a mechanism a well-regarded library already proved.

1. **Honest configuration.** Every builder method changes what is sent, or
   `build()` fails. (`reqwest::RequestBuilder`; clap rejects what it cannot
   honour.)
2. **Progressive disclosure over one set of types.** A one-liner, then a
   client, then a builder, then the raw request — each a strict superset of
   the last. (`reqwest::get` → `Client` → `RequestBuilder`; rig's
   `client.agent(..).build().prompt(..)`.)
3. **The type is the schema.** Tool arguments, tool results, structured
   output and extraction derive from Rust types through one schema pipeline.
   (serde and schemars; Vercel AI SDK `output: Output.object({ schema })` on
   the same `generateText` call; pydantic-ai `output_type=`.)
4. **Tools are documented functions.** A tool is an `async fn`; its doc
   comment is its description; `# Arguments` describes the parameters.
   (axum handlers; rig `#[tool]`; OpenAI Agents SDK `@function_tool`
   docstring parsing.)
5. **One error, many kinds.** Errors stay typed until the caller decides, with
   `is_*()` predicates and a status, and say how to fix the problem.
   (reqwest and sqlx; miette `help:`.)
6. **Tests are first class.** A public mock model, scripted or programmable,
   that records what it was asked. (pydantic-ai `TestModel`, `FunctionModel`
   and `capture_run_messages`; Vercel AI SDK `MockLanguageModelV4`.)
7. **One name.** One crate to add and one path to import. (tokio, axum;
   `rig-core` imported as `rig`.)

## The capability set, ranked

| # | Capability | Mechanism borrowed |
|---|---|---|
| 1 | **Honest configuration.** Every `AgentBuilder` setting reaches the request: model, sampling, stop sequences, thinking, output schema, built-in tools. `Live::dispatcher` merges instead of replacing. `conditional()` branches. | reqwest, clap |
| 2 | **A golden path.** `GeminiLlm::from_env()?` fails fast with the fix in the message; `build()` takes any `BaseLlm` (no `Arc`); `agent.ask(..)`, `agent.chat()`, and one primitive, `run_with(RunRequest) -> RunResult`, underneath. | reqwest, rig |
| 3 | **Typed output.** `agent.ask_as::<T>(..)` and `AgentBuilder::output::<T>()`, schema from `T`, one bounded repair retry on a parse failure. | Vercel `Output.object`, pydantic-ai `output_type` |
| 4 | **One schema pipeline.** `tool::wire_schema::<T>()` is the only way a Rust type becomes a declaration — used by `#[tool]`, `TypedTool`, typed output and extraction. | schemars with a provider transform (rig, pydantic-ai) |
| 5 | **Tools as documented functions.** `#[tool]` takes its description from `///`, parameter descriptions from `# Arguments`, returns any `Serialize`, and accepts any error. | rig, OpenAI Agents SDK |
| 6 | **Streaming.** `agent.stream(..)` yields text deltas, tool calls and a final `RunResult`, across tool rounds, over real `streamGenerateContent` SSE. | Vercel `streamText` |
| 7 | **Typed errors.** LLM failures keep their kind and status end to end (`AgentError::Llm`), with `is_rate_limited()`, `is_auth()`, `status()` and `is_retryable()`; `?` works across the text path. | reqwest, sqlx |
| 8 | **A public mock model.** `MockLlm`: scripted replies, a function model, captured requests; `LlmResponse::from_text(..)` and `::tool_call(..)`. | pydantic-ai, Vercel AI SDK |
| 9 | **Usage and GenAI spans.** `RunResult` carries summed token usage, tool calls and duration; model calls and tool executions emit OpenTelemetry GenAI spans. | pydantic-ai `RunResult.usage()`, OTel GenAI semantic conventions |
| 10 | **Composition that composes.** `Arc<A>` is a `TextAgent`; `Arc<L>` is a `BaseLlm`. | tower's `Service for Arc<S>` |
| 11 | **One name.** A `gemini-adk` facade, imported as `gemini_adk`, with a kernel prelude. | tokio, `rig-core` → `rig` |
| 12 | **A scaffold that compiles, tested in CI;** MSRV published. | cargo-generate, LiveKit `lk app create` |
| 13 | **Deprecate, don't duplicate.** Aliases get `#[deprecated]` pointing at the one name. | Rust API guidelines |

## The golden path

Before, from `examples/quickstart` (17 lines, 8 concepts):

```rust
let llm = Arc::new(GeminiLlm::new(GeminiLlmParams::default()));
let agent = AgentBuilder::new("assistant")
    .instruction("Answer in one sentence.")
    .build(llm)?;
let state = State::new();
state.set("input", "What is the Gemini Live API?")?;
println!("{}", agent.run(&state).await?);
```

After — each rung adds one concept:

```rust
use gemini_adk::prelude::*;

let agent = AgentBuilder::new("assistant")
    .instruction("Answer in one sentence.")
    .build(GeminiLlm::from_env()?)?;

println!("{}", agent.ask("What is the Gemini Live API?").await?);   // ask
let city: City = agent.ask_as("Describe Paris.").await?;             // typed output
let mut chat = agent.chat();                                         // history
chat.send("Hi, I'm Ada.").await?;
let mut events = agent.stream("Tell me a story.");                   // streaming
```

The ladder continues into the moat unchanged: `.tool(get_weather())`, then
`Live::builder()…talk()` for voice, then `Conversation` and `converse` for
governance, then `Sim` for model-free tests. The quickstart and README are
rewritten to climb it.

## Deliberately not in this change

- **One error type across all 45.** The text path's errors become typed and
  convertible, which is what `?` needs. Collapsing Live, memory, storage and
  wire errors into a facade `Error` is a larger migration and is recorded
  here, not attempted.
- **Tool context injection** (pydantic-ai `RunContext`). It needs
  `ToolFunction::call` to receive a context; that change touches every tool
  implementation and deserves its own review.
- **The composition algebra** (`S C T P M A E G`, `>> | * /`). Half of it has
  no consumers, and its operators disagree on meaning. Redesigning it is a
  breaking change with its own RFC; this change only fixes what is wrong
  inside it (`conditional`) and stops advertising it first.
- **Testing a fluent `Live` session offline** (`connect_with_transport`).
  Valuable and next in line; the Live builder's connect path needs a seam
  first.
- **Python bindings, `deploy`, and the web app.** Outside the Rust funnel this
  record is about.

## Order of work

Test foundations first, because every later step is tested with them:

1. `MockLlm`, response constructors, `BaseLlm for Arc<L>`, `TextAgent for Arc<A>`.
2. One schema pipeline; `#[tool]` rebuilt on it.
3. Honest configuration; `conditional`; `Live::dispatcher`.
4. Typed LLM errors; `GeminiLlm::from_env`.
5. `run_with`, `RunResult`, `ask`, `ask_as`, `output::<T>`, `chat`.
6. Streaming, wire to agent.
7. Usage and GenAI spans.
8. Deprecations and a kernel prelude.
9. The `gemini-adk` facade, published MSRV, a scaffold tested in CI.
10. Docs, quickstart, README and examples on the golden path.
