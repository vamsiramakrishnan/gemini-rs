# Record & Replay

The determinism spine: record every byte that crosses the wire and every state
mutation, then replay any session offline through the **real** control plane —
phase machine, extractors, watchers, and tool dispatch all run for real.
Nothing is mocked above the transport seam.

## Recording a session

### Wire log — `record_wire`

One builder call taps both directions of the WebSocket:

```rust
use gemini_adk_fluent_rs::prelude::*;

let handle = Live::builder()
    .instruction("You are a weather assistant")
    .tools(dispatcher)
    .record_wire("/var/log/sessions/user-123.wire.jsonl")
    .connect_from_env()
    .await?;
```

Every outbound frame (setup, user sends, tool responses) and every inbound
frame (model audio/text, tool calls, turn completes) is appended to a JSONL
log. Each entry carries a monotonic sequence number, a direction, an
epoch-millis timestamp, and the base64-encoded raw payload:

```json
{"seq":1,"dir":"out","ts_ms":1718000000000,"payload_b64":"eyJzZXR1cCI6ey4uLn19"}
{"seq":2,"dir":"in","ts_ms":1718000000123,"payload_b64":"eyJzZXR1cENvbXBsZXRlIjp7fX0="}
```

Recording is synchronous, buffered (flushed every second and on drop), and
infallible by contract — an I/O error is logged, never surfaced into the live
session.

At lower layers the same knob is `SessionConfig::record_wire(recorder)` or
`ConnectBuilder::record_wire(recorder)`; custom backends implement the
`WireRecorder` trait (one sync `record(&self, entry: WireEntry)` method).
`MemoryWireRecorder` collects entries in memory for tests. These live under
`gemini_genai_rs::transport` (alongside `FileWireRecorder`, `WireDirection`,
`read_wire_log`, `ReplayTransport`, `ReplayControl`), not in the prelude.

### Mutation journal — `JournalSink`

The in-memory mutation journal is a bounded ring (1024 entries); a two-hour
call loses most of its history. A `JournalSink` receives **every** mutation as
it is recorded:

```rust
use std::sync::Arc;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::state::FileJournalSink;

let state = State::new();
state.set_journal_sink(Arc::new(FileJournalSink::create(
    "/var/log/sessions/user-123.journal.jsonl",
)?));
// hand `state` to the session: Live::builder()…  /  LiveSessionBuilder::with_state(state)
```

The sink is shared with all clones and delta views of the `State` and is
invoked synchronously on the write path — keep it cheap (`FileJournalSink`
buffers and flushes periodically). The ring stays in place for
`recent_mutations()` / `evidence()`; the sink adds unbounded durability.
Journal entries are serde round-trippable `StateMutation` values:

```json
{"sequence":7,"key":"app:last_city","old":null,"new":"London","origin":"set","timestamp_ms":1718000000456,"delta":false}
```

## Replaying a session

### CLI: `adk session replay`

```console
$ adk session replay user-123.wire.jsonl --journal user-123.journal.jsonl

  ADK Session Replay — user-123.wire.jsonl

  Wire log:  7 entries (4 inbound, 3 outbound), 12.3s recorded
  Mode:      offline — recorded frames only, no LLM re-execution,
             no tool re-execution (the CLI has no tool implementations)

  Turn-by-turn:
    turn  1  [text×1, text_complete×1, turn_complete×1]
             “Hello! Ask me about the weather.”
    turn  2  [text×1, tool_call×1, turn_complete×1]
             “It is 22C in London.”
             tool: get_weather({"city":"London"})

  Final state (5 keys):
    session:phase = "Disconnected"
    session:turn_count = 2
    …

  Journal diff — user-123.journal.jsonl (27 mutations, 5 keys):
    DRIFT — 1 key(s) diverged:
      - app:last_city: recorded "London", missing on replay
```

The replay feeds the recorded inbound frames through the real L1 processor
(default callbacks, no tools) and prints a turn-by-turn summary. With
`--journal` it diffs the recorded journal's per-key final values against the
replayed final state and reports **CLEAN** or **DRIFT** (non-zero exit on
drift). Being honest about scope: replay only re-processes recorded frames —
the model is never re-executed (its outputs are *in* the inbound frames), and
the CLI has no access to your tool implementations, so tool-written state keys
are expected to drift. Wall-clock-derived keys (`session:elapsed_ms`,
`session:silence_ms`, `session:remaining_budget_ms`) are excluded from the
diff.

### Programmatic: the replay harness

To re-execute your actual tools (and assert full determinism), replay in-process
with the original dispatcher attached:

```rust
use gemini_adk_fluent_rs::live::{collect_events_until_idle, replay_session};
use gemini_adk_rs::live::LiveSessionBuilder;
use gemini_genai_rs::prelude::SessionConfig;
use gemini_genai_rs::transport::read_wire_log;

let entries = read_wire_log("user-123.wire.jsonl")?;
let state = State::new();
let config = SessionConfig::new("offline"); // no network, no credentials used
let builder = LiveSessionBuilder::new(config.clone())
    .dispatcher(weather_dispatcher(state.clone()))
    .with_state(state.clone());

let replay = replay_session(config, builder, &entries).await?;
let mut events = replay.handle().events();
replay.release();          // start streaming recorded frames
replay.drained().await;    // every frame handed to the session loop
let events = collect_events_until_idle(
    &mut events,
    std::time::Duration::from_millis(300),
    std::time::Duration::from_secs(30),
).await;
let outbound = replay.outbound_frames(); // setup + regenerated tool responses
replay.disconnect().await?;
```

The replay transport is *gated*: only the setup handshake flows until
`release()` is called, so subscribe to events first and lose nothing. Frames
are delivered as fast as the processor consumes them (no original-timing
pacing). `attach_session(builder, session)` is the underlying seam — it bolts
the full three-lane processor onto any pre-connected L0 session
(`ReplayTransport`, `MockTransport`, or a real socket).

### What matches, what doesn't

Replaying the same log with the same tools through a fresh `State` reproduces:

- the per-lane `LiveEvent` sequences (fast lane and control lane each in order;
  *cross-lane* interleaving is scheduler-dependent, in production too),
- the final state and the journal's per-key final values (minus the wall-clock
  keys above),
- byte-identical setup and tool-response frames.

User-originated outbound frames (text/audio you sent) are recorded for audit
but not re-sent — they only ever existed to provoke the recorded inbound
frames. Timer-driven events (`Telemetry`, `TurnMetrics`) are not replayed
deterministically.
