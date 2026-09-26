# Testing Conversations Offline

Test a Live session without a network, a credential, or a model. Four tools
cover different parts of a session:

| Tool | Replaces | Use it to test |
|---|---|---|
| `ScriptedServer` | The Gemini Live server | Tools, governance, extractors and callbacks against a scripted model |
| `ManualClock` | The system clock | Decisions that depend on elapsed time |
| `Tape` and `TapedLlm` | Out-of-band model and resolver calls | Extractors, resolvers and agents that call out during a session |
| `Scenario::from_journal` | The whole session | A spec change against what a recorded session did |

All of them are model-free. A passing test establishes what your application
does with the scripted or recorded inputs. It does not measure what a real
model would say.

## Scripted Live sessions

`ScriptedServer` is a script of what the server sends: model text, what it
heard, tool calls, interruptions. `play` connects a fully configured `Live`
builder to the script over an in-memory transport and runs it.

```rust
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::testing::ScriptedServer;
use serde_json::json;

#[tokio::test]
async fn answers_a_weather_question() {
    let run = ScriptedServer::new()
        .hears("What's the weather in Paris?")
        .calls("get_weather", json!({ "city": "Paris" }))
        .says("It is sunny in Paris.")
        .play(
            Live::builder()
                .instruction("You are a weather assistant.")
                .tools(T::simple("get_weather", "Weather for a city", |args| async move {
                    Ok(json!({ "city": args["city"], "sky": "sunny" }))
                })),
        )
        .await
        .unwrap();

    let responses = run.tool_responses();
    assert_eq!(responses[0]["name"], "get_weather");
    assert_eq!(responses[0]["response"]["sky"], "sunny");
    assert!(run.transcript_text().contains("It is sunny"));
    run.disconnect().await;
}
```

The script stands in for the model. Everything the builder configured runs as
it would in a live call: tools, phases, extractors, watchers, flow governance
and callbacks. The session sends its setup message and tool responses to the
script, and `ScriptedRun` lets you read them back.

### Writing the script

`ScriptedServer::new()` starts with the setup handshake. Each method appends
one server message:

| Method | Server message |
|---|---|
| `text(t)` | Model text, turn still open |
| `turn_complete()` | The model ends its turn |
| `says(t)` | `text(t)` followed by `turn_complete()` |
| `hears(t)` | Input transcription: the user said `t` |
| `speaks(t)` | Output transcription of the model's speech |
| `calls(name, args)` | A tool call. Ids are `call-1`, `call-2`, … in script order |
| `interrupts()` | The model's turn was interrupted |
| `frame(json)` | Any raw server message the helpers do not cover |

`frame` takes a `serde_json::Value` in the Live API wire format, for example a
`toolCallCancellation` message.

### When a run is settled

`play` releases every frame, then collects events until the session has
settled: every scripted tool call that was not cancelled has been answered,
and no event arrived for the settle window. The window is 200 ms by default;
change it with `settle_after(Duration)`. Waiting for tool answers matters
because a slow tool emits no events while it runs. The whole wait is capped at
30 seconds.

### Reading the result

| `ScriptedRun` method | Returns |
|---|---|
| `handle()` | The `LiveHandle`, still connected |
| `state()` | The session's `State` |
| `events()` | Every `LiveEvent` emitted while the script played |
| `transcript_text()` | The `TextDelta` events concatenated |
| `sent()` | Every message the session sent, parsed as JSON, in order |
| `setup()` | The `setup` message the session opened with |
| `tool_responses()` | Every function response sent, in order |
| `disconnect()` | Disconnects the session |

### Governance, proven offline

Because the flow monitor runs for real, a script can check that a tool is
refused out of order. Here the model tries to take a payment before verifying
identity:

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gemini_adk_fluent_rs::compose::tools::ToolComposite;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::testing::ScriptedServer;
use serde_json::json;

fn counting_tool(name: &'static str, calls: Arc<AtomicUsize>) -> ToolComposite {
    T::simple(name, name, move |_| {
        let calls = calls.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        }
    })
}

#[tokio::test]
async fn governance_blocks_an_out_of_order_tool() {
    let verified = Arc::new(AtomicUsize::new(0));
    let paid = Arc::new(AtomicUsize::new(0));
    let flow = Flow::new()
        .step("verify").allow(["verify_identity"]).done(Guard::called_ok("verify_identity"))
        .step("pay").after("verify").allow(["take_payment"]).done(Guard::called_ok("take_payment"))
        .build()
        .unwrap();

    let run = ScriptedServer::new()
        .calls("take_payment", json!({ "amount": 10 }))
        .calls("verify_identity", json!({}))
        .calls("take_payment", json!({ "amount": 10 }))
        .says("Payment taken.")
        .play(
            Live::builder()
                .tools(counting_tool("verify_identity", verified.clone()))
                .tools(counting_tool("take_payment", paid.clone()))
                .govern(flow),
        )
        .await
        .unwrap();

    assert_eq!(verified.load(Ordering::SeqCst), 1);
    assert_eq!(paid.load(Ordering::SeqCst), 1); // the early call never ran
    let responses = run.tool_responses();
    assert_eq!(responses.len(), 3); // every call is answered
    assert_ne!(responses[0]["response"], json!({ "ok": true })); // a refusal
    run.disconnect().await;
}
```

The refused call is still answered: its response is `{"error": reason}` in
place of the tool's result.

### Driving the connection yourself

`into_transport()` returns the script as a `ReplayTransport` and its
`ReplayControl`, for a test that needs to subscribe, send, or inspect frames
at its own pace. Nothing past the handshake flows until `release()`:

```rust
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::testing::ScriptedServer;

let (transport, control) = ScriptedServer::new().says("Hello.").into_transport();
let handle = Live::builder().connect_with_transport(transport).await?;
let mut events = handle.events(); // subscribe before releasing
control.release();
control.drained().await;          // every frame handed to the session loop
let outbound = control.outbound_frames(); // raw bytes: setup, tool responses
```

`Live::connect_with_transport` accepts any `gemini_genai_rs::transport::Transport`.
At L1, `LiveSessionBuilder::connect_with_transport` does the same for a
builder you assembled yourself. Both disable reconnection: the transport is
not reconnected if it closes.

## Controlling time

The runtime components listed below read time from a `Clock`. In production
it is `SystemClock`. In a test it can be a `ManualClock` that moves only when
you move it, so the same inputs give the same timing decisions on every run.

```rust
use std::sync::Arc;
use std::time::Duration;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::testing::ManualClock;

let clock = Arc::new(ManualClock::new());
let state = State::new().with_clock(clock.clone());

let t0 = state.clock().now();
clock.advance(Duration::from_secs(5));
assert_eq!(state.clock().now() - t0, Duration::from_secs(5));
```

The clock travels with `State`: every clone and delta view shares it. Install
it with `State::with_clock` or `State::set_clock`, or on the session builder
with `Live::builder().clock(clock.clone())` (fluent) or
`LiveSessionBuilder::clock` (L1), which set it on the session state. Set it
before the session starts, because some components capture the clock when
they are built.

| `ManualClock` method | Effect |
|---|---|
| `new()` | Wall time starts at the current system time |
| `starting_at(t)` | Wall time starts at `t`, for example a recording's first timestamp |
| `advance(d)` | Moves forward by `d` |
| `set_elapsed(d)` | Moves to `d` after the origin, if that is later; never goes backwards |
| `elapsed()` | Time since the origin |

`gemini_adk_fluent_rs::testing` re-exports `Clock`, `SharedClock`,
`SystemClock` and `ManualClock`.

### What reads the clock

| Component | What it takes from the clock |
|---|---|
| Temporal patterns | The current instant when a pattern is checked, so "sustained for 5 s" means 5 clock seconds |
| Phase machine | Time in the current phase and the timestamps in its transition history |
| Session signals | `session:elapsed_ms`, `session:silence_ms`, `session:remaining_budget_ms`, and the GoAway deadline |
| Voice reactor | Timestamps for playback and speech events |
| Resolver cache | Expiry of a resolver field's `ttl` |
| Mutation journal | `StateMutation::timestamp` |

The clock decides how much time has passed, not when a check runs. Temporal
patterns are still checked on events and on the runtime's 500 ms timer, and
session signals are written when the telemetry lane flushes them. Advancing a
`ManualClock` takes effect at the next check.

`replay_session` installs its own `ManualClock`, replacing any clock the
builder set. It starts at the first inbound frame's recorded timestamp and
moves to each frame's recorded time as the frame is delivered, so the replay
sees the original gaps between frames however fast it runs.
`ReplaySession::clock()` returns it.

## Taping model and resolver calls

Replaying the wire log does not cover calls a session makes out of band: an
LLM extractor, an async resolver behind a slot, a background agent. A `Tape`
records each call's input and output once, then answers from the recording.

```rust
use std::sync::Arc;
use gemini_adk_fluent_rs::llm::{BaseLlm, LlmRequest, LlmResponse, MockLlm};
use gemini_adk_fluent_rs::testing::{MemoryTape, TapedLlm};

let tape = Arc::new(MemoryTape::new());

// Record against a real model (here, a scripted one).
let recording = TapedLlm::recording(
    MockLlm::script([LlmResponse::from_text("Paris")]),
    tape.clone(),
);
recording.generate(LlmRequest::from_text("Capital of France?")).await?;

// Replay with no model at all.
let offline = TapedLlm::replaying("gemini-2.5-flash", tape);
let reply = offline.generate(LlmRequest::from_text("Capital of France?")).await?;
assert_eq!(reply.text(), "Paris");
```

`TapedLlm` implements `BaseLlm`, so wrapped in an `Arc` it goes wherever a
model does, for example `LlmExtractor::new(name, llm, prompt, window)`.
`TapedLlm::recording(inner, tape)` passes every call through to `inner` and
records it. `TapedLlm::replaying(model_id, tape)` needs no inner model and no
credential.

For resolvers, `taped_resolver(tape, mode, name, fetch)` wraps a fetch
function and returns one with the shape `Extract::field_resolve` and
`Conversation::resolve_slot` take. `mode` is `TapeMode::Record` or
`TapeMode::Replay`; in `Replay` the fetch is never called. `name` scopes the
recording, so two resolvers given the same arguments do not answer for each
other.

```rust
use std::sync::Arc;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::testing::{FileTape, TapeMode, taped_resolver};

// `fetch_balance` is the real fetch; in Replay mode it is never called.
let tape = Arc::new(FileTape::open("tests/tapes/balance.jsonl")?);
let record = Extract::record("account")
    .field_resolve("balance", ["account_id"], None,
        taped_resolver(tape, TapeMode::Replay, "balance", fetch_balance))
    .build();
```

### Matching and misses

Calls are matched on their kind (`llm`, or `resolver:{name}`) and their input
as canonical JSON: the request, or the resolver's arguments. The same input
recorded twice replays its two outputs in recording order. A recorded failure
replays as a failure.

A replay never calls out. When the tape holds no (more) calls for an input,
`TapedLlm` returns `LlmError::Config` and a taped resolver returns `Err`, both
quoting the unmatched input.

`MemoryTape` keeps calls in memory: `entries()` returns every call recorded
so far and `MemoryTape::from_entries` loads them for replay. `FileTape` keeps
one `TapeEntry` per JSONL line: `create(path)` truncates the file and records
into it, and `open(path)` loads it for replay and appends any new calls.
Implement the `Tape` trait (`record` and `next`) for other storage.

## Scenarios from recordings

A governed session writes every governance decision to its mutation journal.
`Scenario::from_journal` turns that journal into a model-free scenario you can
run against the conversation spec in CI, so an incident becomes a regression
test. Record the journal with a `JournalSink` (see
[record & replay](./record-replay.md)).

When a flow governs the session, the control plane writes the keys below.
The three tool keys are written only for calls that reach the tool
dispatcher. `from_journal` orders the mutations by sequence number and maps
each entry to scenario steps:

| Key | Written when | Scenario steps |
|---|---|---|
| `flow:tool_call` | A call passes the flow gate, before it runs: `{"tool", "id"}` | `ExpectAllowed(tool)` |
| `flow:tool_denied` | The flow refuses a call in enforce mode: `{"tool", "id", "reason"}` | `ExpectDenied(tool)` |
| `flow:tool_result` | An admitted call completes: `{"tool", "id", "ok"}` | `ToolResult { tool, ok }` |
| `flow:active` | A turn boundary where the flow is evaluated: the active step ids | `Turn`, then `ExpectActive` |
| Any other key | Any write | `Set { key, value }`, the latest value per key, before the next step above |

Keys the runtime owns are skipped, so the simulator must reach them itself.
The skipped prefixes are `session:`, `flow:`, `derived:`, `repair:`,
`correction:`, `state_meta:`, `idempotency:`, `compensated:`, `verbatim:`,
`turn:` and `bg:`.

```rust
use std::sync::Arc;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::simulation::Scenario;
use gemini_adk_fluent_rs::state::MemoryJournalSink;
use gemini_adk_fluent_rs::testing::ScriptedServer;
use serde_json::json;

let journal = Arc::new(MemoryJournalSink::new());
let state = State::new().with_journal_sink(journal.clone());

let run = ScriptedServer::new()
    .calls("set_party", json!({ "size": 4 })).says("Four people. Shall I book?")
    .calls("confirm", json!({})).says("Confirmed.")
    .calls("book_table", json!({})).says("Booked.")
    .play(Live::builder().state(state.clone()).tools(booking_tools(&state)).converse(&convo))
    .await?;
run.disconnect().await;

let scenario = Scenario::from_journal("booking-incident", &journal.entries());
scenario.run(&convo, Enforcement::Enforce).await?; // the spec reproduces it
```

Here `booking_tools` returns test tools that write `party_size`, `confirmed`
and `booked` into `state`, and `convo` is a `CompiledConversation` that
commits `book_table` behind `Guard::is_true("confirmed")`. The scenario begins:

```json
{"name": "booking-incident", "steps": [
  {"expect_allowed": "set_party"}, {"set": {"key": "party_size", "value": 4}},
  {"tool_result": {"tool": "set_party", "ok": true}},
  "turn", {"expect_active": ["book"]},
  ...
]}
```

Run the same scenario against a stricter spec, for example one that also
requires `manager_approved`, and it fails at the `ExpectAllowed("book_table")`
step.

### Driving steps yourself

`Scenario::run` applies each step with `Sim::apply`, which is public so
another driver can share the same semantics. `apply` returns `Err` with the
reason when an expectation step does not hold. Three steps model events that
do not advance a turn:

| Step | Effect |
|---|---|
| `ToolFailed(tool)` | A tool failed or timed out; counts toward the active stage's `escalate_after_tool_failures` |
| `Interrupt` | The user barged in; counts toward the active stage's `escalate_after_interruptions` |
| `ToolResult { tool, ok }` | The flow observes a completed call, as the live runtime records it. Unlike `ToolOk`, it does not advance a turn |

In JSON these are `{"tool_failed": "lookup"}`, `"interrupt"` and
`{"tool_result": {"tool": "book_table", "ok": true}}`.

### From the command line

The `adk` CLI reads a journal written by `FileJournalSink`:

```bash
# Print the scenario a recording implies (JSON on stdout).
adk session scenario user-123.journal.jsonl --name booking-incident

# Re-run the recorded decisions through a spec, turn by turn.
adk flow replay booking.spec.json --journal user-123.journal.jsonl

# Explain the flow at turn 3: active steps, what each waits for, blocked tools.
adk flow why booking.spec.json --journal user-123.journal.jsonl --turn 3
adk flow why booking.spec.json --journal user-123.journal.jsonl --turn 3 --tool book_table
```

`adk session scenario` names the scenario after the file when `--name` is
omitted. Save its output as `<name>.<label>.scenario.json` next to
`<name>.spec.json` and `adk flow ci` runs it with the rest of the corpus (see
[conversation CI](./conversation-ci.md)).

`adk flow replay` prints each turn's active steps, slot writes and tool
outcomes, and a `DIVERGED` line at every step where the spec decides
differently from the session. It ends with `CLEAN` when the spec reproduces
every recorded decision, and otherwise exits non-zero.

`adk flow why` replays the recording up to turn `N` (0 is before the first
turn) and prints the flow explanation as JSON. With `--tool`, it answers
whether that tool was admitted and, if not, why.

## What is and is not deterministic

The tools in this guide substitute the transport, the clock, or an
out-of-band call. Nothing else is replaced.

- The model is scripted or recorded. Its responses are the frames you
  provide.
- Attached tools still run their real implementations. A tool that sends an
  email, charges a card, or writes to a database does so in a scripted run or
  a replay. Attach controlled implementations in offline tests.
- Anything a tool or callback calls directly, outside a `TapedLlm` or a taped
  resolver, reaches the real service.
- Cross-lane event interleaving is scheduler-dependent. Compare each lane's
  events in order, or assert on final state.

## See also

- [Record & Replay](./record-replay.md): recording wire logs and journals,
  and replaying them through the processor.
- [Conversation CI](./conversation-ci.md): running scenario corpora in CI.
- [Capacity and Cost per Session](./capacity.md): measuring the runtime
  against a scripted transport.
