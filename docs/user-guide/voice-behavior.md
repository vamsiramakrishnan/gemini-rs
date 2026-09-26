# Voice Behavior in the Conversation Graph

A conversation graph decides which stage is active, which tools are
admitted and what the model is told. A voice call also needs rules for how
it sounds and for the unhappy paths: silence, a caller talking over a
disclosure, a changed answer, a stage that is not working.

This guide covers the parts of a `Conversation` (or a JSON
`ConversationSpec`) that set that behavior, and what the runtime does with
each. The flow-level parts run on the same `FlowStack` in the simulator and
in a live session.

| Concern | Where it is declared | What the runtime does |
|---|---|---|
| Pacing | `timing` on a stage | Reprompts on silence, cues fillers, holds the floor, sets endpointing |
| Exact wording | `verbatim` on a stage | Checks the output transcript before the stage can complete |
| Corrections | Automatic for main-flow `collect` stages | Re-opens downstream stages and withdraws their confirmations |
| Digressions | `overlays` | Suspends the active layer, then resumes, restarts or terminates |
| Repair | `repair` on a stage | Raises reprompt and escalate signals, can route to a hand-off |
| Policies | `policies` | Redacts state keys, deduplicates and compensates commit tools |

## Per-stage voice timing

`VoiceTiming` sets the pacing while a stage is active. Every field is
optional. An unset field leaves the session's own behavior alone.

| Field | Builder | Effect while the stage is active |
|---|---|---|
| `reprompt_after_ms` | `reprompt_after(silence)` | After this much user silence, ask the model to repeat its question |
| `reprompt` | `reprompt_with(silence, text)` | The text sent when reprompting (default: `DEFAULT_REPROMPT`) |
| `filler_after_ms` | `filler_after(after)` | Emit a filler cue when a tool call runs longer than this |
| `interruptible` | `uninterruptible()` | `false` holds the floor: the user cannot barge in while the model speaks |
| `end_of_speech_ms` | `end_of_speech(pause)` | How long a pause must last before the user's turn ends |
| `context_delivery` | `context_delivery(delivery)` | Override the session's `ContextDelivery` for this stage |

The time builders take a `std::time::Duration` and store milliseconds.

### Setting it

On a `Conversation`, call `.timing(..)` on the stage being authored:

```rust,ignore
use std::time::Duration;
use gemini_adk_fluent_rs::prelude::*;

let convo = Conversation::new("account")
    .stage("ask")
        .instruction("Ask for the caller's account number.")
        .collect(["account_number"])
        .allow(["lookup_account"])
        .timing(
            VoiceTiming::new()
                .reprompt_with(Duration::from_secs(6), "Ask for the account number again, briefly.")
                .filler_after(Duration::from_millis(1500))
                .end_of_speech(Duration::from_millis(900)),
        )
    .stage("done").after("ask").terminal()
    .compile()?;
```

In a JSON spec, the same stage carries a `timing` object. Unset fields are
omitted:

```json
{
  "id": "ask",
  "say": "Ask for the caller's account number.",
  "collect": ["account_number"],
  "allow": ["lookup_account"],
  "timing": {
    "reprompt_after_ms": 6000,
    "reprompt": "Ask for the account number again, briefly.",
    "filler_after_ms": 1500,
    "end_of_speech_ms": 900
  }
}
```

`context_delivery` serializes as `"immediate"` or `"deferred"`, and
`uninterruptible()` as `"interruptible": false`. Stages inside a digression
take `timing` too.

A session governed by a plain `Flow` sets timing per step with
`Live::builder().govern(flow).stage_timing("verify", timing)`. Timing needs a
governed flow; without one it is not applied. With both, a
`stage_timing` for a step wins over the conversation's timing for that
step, whether it is called before or after `Live::converse(&convo)`.

### Merging and publication

When the session starts, and after every turn and tool call, the flow stack
merges the timing of the active steps in the driving layer (the main flow,
or the digression that is suspending it) and publishes the result to state
under `session:voice_timing` (the constant `VOICE_TIMING_KEY`). It writes
only when the value changes, and removes the key when no active step has
timing. The runtime reads the key from state, so a test or a watcher can
read it the same way.

When several steps with timing are active at once, `VoiceTiming::merge`
takes the more cautious setting of each field:

- the shorter `reprompt_after_ms` and `filler_after_ms`;
- the longer `end_of_speech_ms`;
- `interruptible: false` if any active step forbids barge-in;
- `reprompt` and `context_delivery` from the first active step, in
  declaration order, that sets them.

### What the runtime does with each field

**Reprompt on silence.** The telemetry lane keeps `session:silence_ms`
current: the time since the last session event. A timer checks it every
100 ms. When the silence passes the active stage's `reprompt_after_ms` and
the model is not speaking, it sends the reprompt text to the model as a user
turn and emits `LiveEvent::Reprompted { silence_ms }`. It fires once per
silence. Any activity, including the model's reply, ends the silence and
re-arms the timer. The timer is started only when at least one stage in the
session sets `reprompt_after_ms`.

**Filler cue.** When an inline tool call is still running after
`filler_after_ms`, the control lane emits
`LiveEvent::FillerCue { tool, elapsed_ms }` once. The tool is not cancelled
or delayed. The runtime does not play anything itself: the application
reacts to the event with an earcon or a holding line.

**Holding the floor.** With `interruptible: false`, `LiveHandle::send_audio`
replaces each microphone chunk with silence of the same length while the
model is speaking (the `is_model_speaking` session flag). The chunk is still
sent, so the stream keeps its cadence, but neither the server's VAD nor the
client's input VAD can hear the caller cut in. Once the model stops speaking,
audio passes through again.

**End of speech.** `end_of_speech_ms` sets the turn-commit end-of-turn hold
in `send_audio`, and installs a turn-commit policy if none was configured.
It applies only under client activity authority
(`Live::client_interruption_authority()`). The server's VAD is fixed at
setup, so under server authority this field has no effect. See
[turn commitment](./hardening.md#turn-commitment-holds-and-sustains-not-raw-edges).

**Context delivery.** At a turn boundary, the steering context for the turn
is delivered with the active stage's `context_delivery` if it sets one, else
with the session's. If any stage sets `deferred`, the session creates the
deferred-context queue even when the session itself delivers immediately.

`LiveEvent` (in `gemini_adk_fluent_rs::live`) reaches the application
through `handle.stream()` or `handle.events()`; match on `FillerCue` and
`Reprompted` there.

## Verbatim stages

A stage instruction steers what the model says. It does not guarantee
wording. A verbatim stage makes exact wording a completion condition:

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

const TERMS: &str = "Calls may be recorded for quality and training purposes.";

let convo = Conversation::new("disclosure")
    .stage("terms")
        .verbatim(TERMS)
    .stage("help").after("terms").terminal()
    .compile()?;

let handle = Live::builder()
    .output_transcription()
    .converse(&convo)
    .connect_from_env()
    .await?;
```

In a spec, the stage field is `"verbatim": "<text>"`. The compiler lowers it
as follows:

- The stage's posture gets an instruction to say the text exactly as
  written, followed by the text, after any `instruction` the stage has.
- The stage's completion guard also requires the state key
  `verbatim:{step}` to be `true`, in addition to any other completion.
- If the stage sets no `timing`, it gets `VoiceTiming::new().uninterruptible()`.
  A stage that sets its own `timing` uses that value as given, so add
  `.uninterruptible()` to it if the model should still hold the floor.

At the end of each model turn, the control lane compares the turn's output
transcript with the required text. Similarity is word level: one minus the
word edit distance divided by the length of the longer text, after
lowercasing and dropping punctuation. The turn passes at 0.9 or above
(`VERBATIM_MIN_SIMILARITY`). One wrong word in a ten-word passage still
passes; in a nine-word passage it does not. The verdict is written to
`verbatim:{step}`, and `LiveEvent::VerbatimChecked { step, similarity, passed }`
is emitted. Once a turn passes, a later turn in the same stage does not reset
the flag. A paraphrase keeps the conversation in the stage, where the posture
asks for the exact text again.

The check reads what was actually said, so it needs output transcription.
Enable it with `.output_transcription()` or `.transcription()`. With no
transcript, nothing is checked and the stage does not complete.

`Motif::disclosure(id, ack_key)` (in `gemini_adk_fluent_rs::motifs`) is a
related stage: it asks the model to read the required disclosure, completes
when `ack_key` is true, and is uninterruptible by default. It does not check
wording. Set the stage's `timing` to allow barge-in.

## Corrections

A caller who says "actually, make it five" after confirming a booking has
changed a slot that a later stage already relied on. For every main-flow
stage that collects slots, the compiler finds the stages downstream of it
(through `after`, `next` and repair `escalate_to` edges, transitively) and
the state keys their `commit` guards read, excluding collected slots: the
confirmations to withdraw. It then lowers a reset of those downstream
stages, gated on the slot's correction flag.

At each turn boundary, the flow stack compares every watched slot with the
value it saw last. A change from one captured value to another is a
correction. The first value is not a correction, and neither is a slot being
removed. On a correction the stack sets `correction:{slot}` to `true`,
removes the confirmation keys, and the reset un-latches the downstream
stages so they run again with the new value. The flag is lowered once the
main flow has advanced past it, so the next correction is a new rising edge.
A correction made while a digression drives keeps its flag raised until the
main flow advances.

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

let convo = Conversation::new("booking")
    .stage("collect")
        .collect(["party_size"])
    .stage("book").after("collect")
        .commit("book_table", Guard::is_true("confirmed"))
        .complete_when(Guard::called_ok("book_table"))
    .stage("end").after("book").terminal()
    .compile()?;

// A correction to party_size clears the booking confirmation.
assert_eq!(convo.correction_policies()["party_size"], ["confirmed"]);
```

With this definition, after `party_size` changes from 4 to 5, `book_table`
is denied again and `book` is active until `confirmed` is set again. The
test `a_corrected_slot_reopens_the_confirmation` in
`crates/gemini-adk-fluent-rs/src/conversation.rs` runs that sequence as a
scenario.

## Digressions and resume

A digression (overlay) is a named sub-flow with a trigger guard. When the
trigger holds at a turn boundary, the digression suspends whichever layer is
driving and governs the session until it completes: tool admission,
postures, grounds, timing and verbatim requirements all come from its active
steps. The suspended layer's marking is untouched.

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::conversation::Resume;

let convo = Conversation::new("support")
    .stage("collect")
        .collect(["issue"])
    .stage("resolve").after("collect").terminal()
    .overlay("faq")
        .trigger(Guard::is_true("intent:faq"))
        .stage("answer")
            .complete_when(Guard::is_true("faq_answered"))
        .stage("faq_end").after("answer").terminal()
        .resume(Resume::Previous)
    .end_overlay()
    .compile()?;
```

In a spec, digressions go in `overlays`:

```json
"overlays": [
  {
    "name": "cancel",
    "trigger": { "is_true": "intent:cancel" },
    "stages": [{ "id": "goodbye", "say": "Confirm the cancellation and say goodbye.", "terminal": true }],
    "resume": "terminate"
  }
]
```

An overlay authored with the builder has no trigger until `.trigger(..)` is
set, and never fires without one. An overlay with no `require` is complete
when its terminal stages are done.

The stack applies these rules:

- **Triggers.** Triggers are evaluated against the main flow's context. The
  first declared overlay whose trigger holds and that is not already on the
  active path is entered. A digression does not re-enter itself while it is
  active, but once it has closed it is entered again if its trigger still
  holds, so clear the trigger fact once it has been handled.
- **Nesting.** A digression can be interrupted by another one, which then
  drives until it completes. The active path is outermost first; the state
  key `flow:overlay` names the driving digression (`null` when the main flow
  drives).
- **Closing turn.** A digression stays the active layer through the turn on
  which it completes, so that turn's posture (for example "hand off to a
  human now") reaches the model. Its resume policy applies at the next turn
  boundary.

What happens when a digression closes depends on its `resume` policy, which
applies to the layer beneath it (the next digression on the path, or the
main flow):

| `Resume` | JSON | Effect |
|---|---|---|
| `Previous` (default) | `"previous"` | The layer beneath continues exactly where it was suspended |
| `Restart` | `"restart"` | The layer beneath restarts its monitor: marking, fired `on_enter` actions and reset edges. State is not cleared, so filled slots stay filled. Restarting the main flow also clears its repair signals and counters |
| `Terminate` | `"terminate"` | The conversation ends, even from a nested digression: the whole active path is cleared |

After a terminate, the stack governs nothing: no active steps or postures,
and every tool is denied with a reason naming the digression. The state key
`flow:terminated` is set to `true`. The runtime does not hang up by itself;
watch that key and close the session.

`Policy::safety_handoff(intents)` is lowered to a digression named `safety`
that triggers on any `intent:{name}` flag and resumes with `Terminate`.

## Repair escalation

A `RepairPolicy` on a main-flow stage turns stalling into state signals:

| Field | Builder | Signal |
|---|---|---|
| `reprompt_after` (default 2) | `RepairPolicy::new(reprompt_after, escalate_after)` | `repair:{step}:reprompt` after this many turns active without completing |
| `escalate_after` (default 4) | same | `repair:{step}:escalate` after this many turns |
| `escalate_after_interruptions` | `.escalate_after_interruptions(n)` | `repair:{step}:escalate` after `n` barge-ins while the step is active |
| `escalate_after_tool_failures` | `.escalate_after_tool_failures(n)` | `repair:{step}:escalate` after `n` failed or timed-out tool calls while the step is active |
| `escalate_to` | `.escalate_to(step)` | Route to `step` when the escalate signal is raised |

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

let convo = Conversation::new("support")
    .stage("collect")
        .collect(["issue"])
        .repair(
            RepairPolicy::new(10, 10)
                .escalate_after_interruptions(2)
                .escalate_to("handoff"),
        )
    .stage("handoff").terminal()
    .compile()?;
```

In a spec the stage field is `repair`, for example
`{ "reprompt_after": 2, "escalate_after": 4, "escalate_to": "handoff" }`.
Omitted thresholds take the defaults. With `escalate_to`, the compiler adds an edge to the target gated on the
escalate signal, and the stage also completes when the signal is raised. An
unknown target is a `ConversationError::Spec` at compile time. The test
`repeated_barge_ins_escalate_to_the_handoff` in `conversation.rs` checks that
two barge-ins in `collect` raise the escalate signal and the next turn
completes through `handoff`.

Repair is tracked for main-flow stages only. Turn counts, barge-ins and tool
failures are not counted while a digression drives. When a step leaves the
active set, its counters are dropped and its reprompt signal is set to
`false`; its escalate signal is set to `false` too, unless the step completed
by escalating.

The `repair:{step}:reprompt` signal is only a state key. Nothing in the
runtime acts on it by itself; read it from your own guards or watchers. It is
separate from the silence reprompt in `VoiceTiming`, which is measured in
time and sends a message to the model.

## Runtime-enforced policies

A `Policy` is a cross-cutting aspect attached to a whole conversation with
`Conversation::policy(..)`, or to a session with `Live::policy(..)`. Import
it from `gemini_adk_fluent_rs::policy`. In a spec, policies go in
`policies`, tagged by `kind`:

```json
"policies": [
  { "kind": "redact", "keys": ["card_number", "cvv"] },
  { "kind": "commit", "tool": "charge_card", "idempotency_key": "{user_id}:{amount}", "compensate_with": "refund" },
  { "kind": "safety_handoff", "intents": ["self_harm", "abuse"] }
]
```

`Live::converse(&convo)` installs the conversation's redact and commit
policies alongside any added with `Live::policy`, in either order. A safety hand-off
needs the conversation compiler: passed to `Live::policy`, it is reported as
a configuration error at connect.

### Redaction

`Policy::redact(keys)` marks state keys as sensitive
(`State::redact_keys`). Where state leaves the process, their values are
replaced with `"[redacted]"`:

- the durable journal sink, when one is attached (both old and new values);
- persistence snapshots (the snapshot's state map);
- `LiveEvent::Extraction` events, for a field named by a redacted key or such
  a field one level inside an extractor's object result.

A key also covers its scoped forms, such as `app:card_number`. Reads inside
the process (`get`, guards, tools) see the real value, and so does the
in-memory mutation journal. Transcript text is not covered: configure
`Live::redaction(..)` for that, and note that the snapshot's transcript
summary is not touched by state redaction. For what redaction does and does
not guarantee across fragments, telemetry, raw deltas, recordings and
handoffs, read [hardening](./hardening.md) before relying on it.

### Commit governance

`Policy::commit(tool)` wraps a registered tool in a `CommitGuard` at connect:

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::policy::Policy;

let handle = Live::builder()
    .tools(charge_card_tool)
    .tools(refund_tool)
    .policy(
        Policy::commit("charge_card")
            .idempotency_key("{user_id}:{amount}")
            .compensate_with("refund"),
    )
    .connect_from_env()
    .await?;
```

- **Idempotency.** Each `{name}` in the template takes the call's argument
  `name`, else the state value at `name`. A call whose rendered key already
  succeeded returns the first result without running the tool again. The
  result is stored in the session's `State` under
  `idempotency:{tool}:{key}`. If any placeholder has no value (or is `null`),
  the call is not deduplicated and a warning is logged, so an incomplete key
  never merges two different commits.
- **Compensation.** When the tool fails, the compensating tool runs with the
  same arguments. If it succeeds, `compensated:{tool}` is set to `true`. The
  original failure is still returned to the model. A failed compensation is
  logged.

Both names must be registered function tools. If the commit tool or the
compensating tool is missing, connect fails with an `AgentError::Config`
that names the tool.

A commit policy does not decide when the tool may run. Gate that with the
stage's `.commit(tool, guard)` (or `commit` in the spec), which keeps the
tool denied until its confirmation guard holds.

## Testing offline

All of this can be tested without a model or credentials.

The model-free simulator drives the same `FlowStack` a live session drives.
`Sim` (in the prelude) has `interrupt()` and `tool_failed(tool)`, and a
`Scenario` (in `gemini_adk_fluent_rs::simulation`) has the matching
`SimStep::Interrupt` and `SimStep::ToolFailed`. Neither advances a turn. In
scenario JSON they are `"interrupt"` and `{ "tool_failed": "lookup" }`, so
repair, correction and digression behavior can go into the
[conversation CI](./conversation-ci.md) corpus. The simulator publishes
`session:voice_timing` to its state after each turn, so a test can assert the
merged timing with `sim.slot::<VoiceTiming>("session:voice_timing")`.

The timers, audio path and transcript checks run in a live session, so test
them against `gemini_adk_fluent_rs::testing::ScriptedServer`. It plays a
script of server frames (`.speaks(..)` for output transcription,
`.calls(..)`, `.interrupts()`, `.frame(..)`) into your configured `Live`
builder and returns the events, sent messages and state. The tests in
`crates/gemini-adk-fluent-rs/tests/` cover each area:

| File | Covers |
|---|---|
| `voice_timing.rs` | Filler cues, reprompt once per silence, the mic silenced while the floor is held |
| `verbatim.rs` | A paraphrase keeps the stage open; the exact text completes it |
| `policies.rs` | A retried commit charges once; a commit policy for a missing tool fails at connect |

Tools attached to a scripted session still run, so use controlled
implementations in these tests. See [testing](./testing.md) for the
scripted server and simulator in detail.
