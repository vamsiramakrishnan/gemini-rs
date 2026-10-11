# Decision Models (Jev)

A decision model answers typed questions about a piece of state. It returns
calibrated probabilities instead of text. TypeSafe AI's Jev
(`typesafe-ai/jev`) is one, served through Vercel AI Gateway.

| Question | Answer |
|---|---|
| `boolean` | `probability` that the statement is true |
| `choice` | the chosen option and each option's probability (1 to 255 options) |
| `score` | a position on an ordered scale of 2 to 10 levels, indexed from 0 |

A voice agent decides many things after each turn, and most of them have
exactly this shape:

- whether the caller agreed to the read-back;
- whether they asked for a person or described an emergency;
- which offered time they picked;
- which stage comes next.

An [extractor](extraction.md) backed by a language model can decide them too,
but each call takes seconds, and its answer is latched into a state key that
then has to be cleared when the caller changes their mind. A decision model
answers in about a quarter of a second. It writes no text, so it cannot fill
an open slot such as a name or a date; keep those with an extractor.

Decisions are part of the governed flow. You declare each question once, and
guard on it where it matters with `decided`. The runtime works out which
questions the flow can act on, asks them in one request about the rolling
conversation, and records each answer for the caller turn it is about.

## Setup

Enable the `ai-gateway` feature (`gemini-adk-rs/ai-gateway`, or
`gemini-adk-fluent-rs/ai-gateway`) and give the session a credential:

```bash
# .env.local at the repository root (git-ignored)
AI_GATEWAY_API_KEY=...
```

| Variable | Use |
|---|---|
| `AI_GATEWAY_API_KEY`, else `VERCEL_OIDC_TOKEN` | Credential (required) |
| `AI_GATEWAY_DECISION_MODEL` | Model, default `typesafe-ai/jev` |
| `AI_GATEWAY_BASE_URL` | Gateway address, default `https://ai-gateway.vercel.sh` |

`adk spec run` reads `.env.local`, then `.env`. Jev needs paid AI Gateway
credits. On the free tier the Gateway answers `403` with "Free tier users do
not have access to this model".

`examples/ai-gateway-jev` makes the same call with the TypeScript AI SDK
(`experimental_decide`). Run it with `npm run decide` to check the
credential.

## Declare the questions

```json
"decisions": {
  "confirmed": {
    "type": "boolean",
    "instructions": "In their last turn, did the caller agree to the booking that was read back?",
    "criteria": { "true": "the caller said yes in their own words",
                  "false": "they hesitated, changed a detail, or only picked an option" }
  },
  "wants_person": {
    "type": "boolean",
    "instructions": "In their last turn, did the caller ask to speak to a person?"
  },
  "picked_slot": {
    "type": "choice",
    "instructions": "Which of the offered times did the caller choose?",
    "options_from": "availability.slots",
    "none": "the caller has not chosen one of the offered times",
    "writes": "slot"
  },
  "next_step": {
    "type": "choice",
    "instructions": "What should the assistant do next, given the caller's last turn?",
    "criteria": { "book": "the caller confirmed the read-back",
                  "read_back_again": "the caller changed or questioned a detail",
                  "keep_collecting": "nothing was decided yet" }
  },
  "frustration": {
    "type": "score",
    "instructions": "How frustrated does the caller sound in their last turn?",
    "criteria": ["calm", "slightly impatient", "frustrated", "angry"]
  }
}
```

| Field | Default | Meaning |
|---|---|---|
| `type` | | `boolean`, `choice` (1 to 255 options) or `score` (2 to 10 levels) |
| `instructions` | | The question. Ask about "their last turn" for a confirmation or an intent |
| `criteria` | | Boolean: what `true` and `false` mean. Choice: option to description. Score: level descriptions, lowest first |
| `options_from` | none | Choice options read from a state path (`key` or `key.field`) at each ask. An option is a string, a number, or an object keyed by its `id`, `value` or `name` |
| `none` | none | Adds the option `none_of_these` to a choice. A choice always picks something, so a pick among offered options needs a way to say "not yet" |
| `at_least` | 0.85 boolean, 0.6 otherwise | The `P(true)` a yes needs (a no needs `1 -` this), or the certainty a pick or score needs |
| `writes` | none | Also write the decided value into this state key: the boolean, the chosen option (an object option whole), or the score |

## Guard on them

`decided` is a guard atom like `captured` or `called_ok`, usable wherever a
guard is: a stage's `commit`, `done` and `next`, a digression's `trigger`, a
flow step's gate and edges, a `never_until` constraint, a phase transition
and a pattern.

| Guard | Holds when the answer for the current caller turn |
|---|---|
| `{"decided": "confirmed"}` | is yes, picked an option, or gave a score |
| `{"decided": {"confirmed": false}}` | is no, or picked `none_of_these` |
| `{"decided": {"next_step": "book"}}` | picked `book` |
| `{"decided": {"frustration": {"at_least": 2}}}` | is a score of 2 or more |
| `{"decided": {"frustration": {"at_most": 1}}}` | is a score of 1 or less |

In Rust, `Guard::decided`, `decided_no`, `decided_is`, `decided_at_least`
and `decided_at_most`.

```json
"conversation": {
  "stages": [
    { "id": "offer", "collect": ["slot"], "next": [{ "to": "confirm", "when": { "captured": ["slot"] } }] },
    { "id": "confirm",
      "commit": { "tool": "book", "when": { "decided": "confirmed" } },
      "next": [{ "to": "offer", "when": { "decided": { "next_step": "read_back_again" } } }] }
  ],
  "overlays": [
    { "name": "handoff", "trigger": { "decided": "wants_person" }, "stages": [ ... ] }
  ]
}
```

An answer counts only for the caller turn it is about. A yes given three
turns ago does not satisfy a guard now, so nothing is latched and nothing has
to be cleared when the caller corrects themselves: the next ask reads the
correction. An unsure answer satisfies no expectation, neither yes nor no.

`adk spec check` rejects a `decided` that names no question, or expects
something its question cannot answer (`false` of a choice without `none`, an
option a fixed choice does not have, a bound on a boolean). It warns about a
question that no guard names and that writes nothing.

## When questions are asked

There are two decision points:

1. **The caller's turn ends.** Asked alongside the turn's extractors, so it
   adds no wait to the turn pipeline.
2. **The model calls a tool.** Asked before the call is admitted, about the
   turn still in progress. When the model calls the commit tool in the same
   breath as the caller's "yes, go ahead", the gate reads that yes; a
   digression the answers trigger (a request for a person) opens before the
   call is judged.

At the turn's end the runtime asks the questions the flow can act on:

- those named by the guards of the flow that is driving (its stages, edges
  and constraints), so a stage entered during the turn already has its
  answers, and by the triggers of digressions that could open;
- those that `writes` a key an active stage's guards read, so a pick fills
  the stage that collects it;
- those named by phase transitions and patterns, asked at every caller turn.

At a tool call it asks only what decides whether that call is admitted: the
commit (`never…until`) guards on the called tools, and the triggers of
digressions that would admit them. A call no decision governs asks nothing
and does not wait.

Each round is one request, and latency barely moves with the number of
questions. A question already answered for this caller turn is not asked
again. Questions only a digression's own stages name wait until it opens,
and while a digression drives, the main flow's questions wait for it to
finish.

The decision model reads one state:

- `conversation`: the last 40 turns (`Decisions::history_turns`), as caller lines, tool
  results and agent lines in order;
- `context`: what the active stages ground the model in;
- `active_stages`: the stages that are active.

Each answer is recorded in state under `decision:{id}`:

```json
{ "outcome": "yes", "confidence": 0.97, "turn": "7:3f2a…" }
```

| Outcome | From |
|---|---|
| `yes`, `no` | a boolean at `P(true) ≥ at_least`, or `≤ 1 - at_least` |
| `chosen` (with `value`) | a choice whose certainty is at least `at_least` |
| `none` | `none_of_these` picked with that certainty |
| `scored` (with `value`) | a score whose certainty is at least `at_least` |
| `unsure` | anything else: in the uncertain band, refused, too slow, failed, or a choice with no options offered yet |

Certainty is the model's reported confidence, or else the chosen option's
probability. A score between two levels (2.5 on a 0 to 3 scale) is not
unsure about its direction: its certainty is the probability of the two
levels it lies between.

The model gets 2 seconds (`Decisions::with_timeout`). A slow or
failed ask records its questions `unsure`, so a guard on them does not hold:
the commit is refused, not admitted. A question recorded unsure this way is
asked again at the next decision point.

The bank holds no session state, so one `Decisions` can serve every session.

## In Rust

```rust,ignore
use std::sync::Arc;
use gemini_adk_rs::decision::{Decision, Decisions, GatewayDecisionModel, Question};

let jev = Arc::new(GatewayDecisionModel::from_env()?);
let decisions = Decisions::new(jev)
    .question("confirmed", Decision::new(
        Question::boolean("In their last turn, did the caller agree to the booking that was read back?")
            .when("the caller said yes in their own words",
                  "they hesitated, changed a detail, or only picked an option"),
    ))
    .question("picked_slot", Decision::new(
        Question::choice::<&str, &str>("Which of the offered times did the caller choose?", []),
    )
        .options_from("availability.slots")
        .or_none("the caller has not chosen one of the offered times")
        .writes("slot"));

let booking = Conversation::new("booking")
    .stage("offer").collect(["slot"])
        .next("confirm", Guard::captured(["slot"]))
    .stage("confirm").commit("book", Guard::decided("confirmed"))
        .next("done", Guard::called_ok("book"))
    .stage("done").terminal()
    .compile()?;

Live::builder().decisions(decisions).converse(&booking)
```

`Live::builder().decisions(..)` turns on input and output transcription,
which the conversation is built from.

In a spec, `SpecResources::decision_model` must be set when `decisions` is
declared. `adk spec run` creates a `GatewayDecisionModel` from the
environment, and `adk spec codegen` writes the same bank and guards into the
generated project.

## Offline scenarios

A scenario scripts the answers with a `decide` step, which starts a new
caller turn:

```json
{ "decide": { "confirmed": true, "picked_slot": "2026-10-20T09:00", "frustration": 2.5 } }
```

`true`/`false` answer a boolean, an option's key a choice (`"none_of_these"`
for none), a number a score, and `null` is unsure. A pick from
`options_from` writes the offered option, and `writes` applies, as in a live
session. A `user` step also starts a new caller turn, so a scripted yes does
not outlive the turn it was given in. `Scenario::from_journal` turns the
answers recorded in a session's journal back into `decide` steps, so an
incident replays with the same answers.

## Where to use it

- **Confirmations.** `commit` guarded by `decided`, judged at the tool gate
  in about a quarter of a second, with no latched flag to clear when the
  caller changes a detail.
- **Intents that open digressions.** A `trigger` on `decided` opens the
  handoff or the emergency path before the tool the model called with it is
  judged.
- **Stage routing.** A choice over what happens next, compared in `next`
  guards with `{"decided": {"next_step": "…"}}`. It is asked only while the
  stage that branches on it is active.
- **Picks among offered options.** `options_from` plus `none` plus `writes`,
  instead of asking a language model to restate the option.
- **Escalation and tone.** A score with `at_least` in a pattern or a phase
  transition (asked every turn), such as moving to a de-escalation phase.
- **Judging live runs.** The spec live-eval harness asks Jev whether the
  agent claimed a success no tool result supports, whether the read-back
  matched, and how far the caller's goal was met.

Set `at_least` by the cost of a wrong answer. A commit confirmation needs a
higher bar than an intent that only starts a handoff.

## Measured

`tests/decision_latency.rs` in `gemini-adk-fluent-rs` times Jev from Rust
through AI Gateway. Run it with
`cargo test -p gemini-adk-fluent-rs --test decision_latency -- --ignored --nocapture`.
Over 78 calls about one caller turn there were no errors and no wrong
answers:

| Case | p50 | p90 | max |
|---|---|---|---|
| 1 question | 223 ms | 293 ms | 539 ms |
| 4 questions | 221 ms | 282 ms | 5,967 ms |
| 8 questions | 224 ms | 253 ms | 362 ms |
| 4 questions, about 10× the state | 224 ms | 260 ms | 270 ms |
| 10 at once, 4 questions | 249 ms | 589 ms | 621 ms |

Adding questions or state barely moves latency. One call in 78 took about
6 seconds, so a timeout and a fallback still matter. For comparison, a
`gemini-flash-latest` extraction of the same kind took 2.3 s at the median
with thinking off, and 7.7 s with it on.

### Labelled decisions, without a Live session

`tests/decision_eval.rs` runs 75 labelled cases from
`tests/fixtures/decisions/cases.json` through the same path a session
uses: the fixture's questions are `decisions` entries compiled into one
`Decisions` bank, each case is asked with `Decisions::ask` over its
`Conversation` (its facts as the active stages' grounding), and the answer
is read back from state as a `decided` guard reads it. The cases cover
consent, replies to a different question, prompt injection,
speech-recognition noise, other languages, long calls, asking for a
person, cancelling, dental emergencies, picks among offered times and
prescriptions, frustration, and judging a finished call. Each case runs
with the last exchange and with the whole call. The 150 asks take about 5
seconds.

```text
cargo test -p gemini-adk-fluent-rs --test decision_eval -- --ignored --nocapture
```

| Conversation | Right | Unsure | Wrong | p50 / p90 |
|---|---|---|---|---|
| last exchange | 78 | 9 | 0 | 222 / 336 ms |
| whole call | 77 | 10 | 0 | 219 / 368 ms |

- **Nothing was decided wrongly at the default thresholds.** What Jev
  could not decide is what a person would also find ambiguous: "mm hmm"
  (0.70 to 0.78), "Yes, but can you make it 7:30?" (0.21 to 0.24), the
  injection (0.27 to 0.36), "I don't want to talk to a machine" (0.65),
  "I'll call back later" (0.57 to 0.62).
- **The closest call was a time pick before any read-back.** "Seven
  o'clock is perfect", said when the agent had asked for a name, scored
  0.74 to 0.75. That is below the 0.85 consent threshold, so it stays
  unsure; lowering the threshold for confirmations would book it.
- **Picks, emergencies, scores, other languages, long calls and judging
  were all right**, including "my blood pressure one" for Lisinopril, the
  misheard "met forming" for Metformin, and "this is the third time I've
  called" (2.5 between frustrated and angry, both right).
- **The whole call is as fast as the last exchange.** Asking about "their
  last turn" keeps an earlier yes from being read again, which is why the
  runtime can send 40 turns of history.

### Live A/B

`SPEC_LIVE_SIGNALS=both` in the spec live-eval harness runs the 11
scenarios on `gemini-3.8-live` with typed and with spoken (TTS) callers.
The two arms differ only in who decides the caller signals: the fixtures'
extractor latching flags, or Jev answering `decisions` that `decided`
guards read (confirmation questions carry true/false criteria). Both arms
keep the extractors that fill slots, pinned to `gemini-3.5-flash-lite` at a
thinking budget of 64 (see [extraction](extraction.md#choosing-the-extraction-model)).

| Input | Signals | Checks | Scenarios passing | Signals landed after the turn, p50 / p90 | Commit-tool wait p50 | Every tool wait p50 / p90 / max |
|---|---|---|---|---|---|---|
| text | extractor | 22/25 | 10/11 | 727 / 1,183 ms | 634 ms | 1 / 745 / 1,370 ms |
| text | Jev | 22/25 | 10/11 | 242 / 586 ms | 290 ms | 1 / 386 / 802 ms |
| voice | extractor | 21/25 | 9/11 | 759 / 1,199 ms | 598 ms | 0 / 586 / 823 ms |
| voice | Jev | 20/25 | 9/11 | 250 / 552 ms | 213 ms | 1 / 242 / 709 ms |

- **Decisions land about three times sooner than a fast extractor, and ten
  times sooner than the old default.** With extraction on the rolling
  `gemini-flash-latest`, the extractor arm's signals landed 2.6 to 2.8 s
  after the turn and its commits waited about 2 s.
- **Commit tools are decided at the gate.** The model calls `book_table` in
  the same breath as the caller's yes; the gate asks Jev about the turn in
  progress (about 250 ms) and admits it. Other tool calls ask nothing.
- **The double handoff is gone.** In `trattoria-person` (voice) the
  extractor arm transferred twice in this run and the one before: the
  intent landed after the transfer and opened the handoff digression, which
  then waited for a transfer of its own. With Jev the gate opens the
  digression before it judges the transfer; that scenario passed in every
  run.
- **Fail closed, by design.** In `pharmacy-refill` (voice, Jev) the agent's
  read-back came out garbled ("Which prescription would you like to I can
  refill your Lisinopril. Please confirm…") and the caller's "Yes, that's
  right. Please submit it." scored 0.84, under the 0.85 bar: unsure, so
  `submit_refill` was refused three times and the call ended unsubmitted.
  Lower `at_least` only where a wrong yes is cheap.
- **A callback is not a transfer.** "Yes, please have the pharmacist call
  me back" read as asking for a person in 11 of 12 runs across both arms,
  and in one the call was transferred after the callback was booked. The
  extractor's field said "asked to speak to a person or a pharmacist", and
  Jev scored the callback 0.90 against it. Saying what is not a transfer
  fixed both: the extractor was right in 20 of 20 offline (0 before), and
  Jev, given true/false criteria, scored the callback 0.14 and "Can I talk
  to the pharmacist right now?" 0.94. The scenario now also expects that
  `handoff_to_staff` never runs, and it did not in any of the 20 runs since.
- **A check must be about what is committed.** In two spoken runs the model
  called `check_refills` on Lisinopril before the caller named a
  medication, and when the caller asked for Atorvastatin, which has no
  refills, `submit_refill` was admitted on the stale "refillable" result.
  Guards read state, not the call's arguments, so the fixture now requires
  the medication before the check (`{"captured": ["medication"]}`). A guard
  that binds a commit's arguments to the checked values would close the
  general case.
- **An unspecified confirmation reads a pick as a yes.** Before the
  confirmation question had criteria, one run booked on "Seven o'clock is
  perfect", said before any read-back. With criteria it stays unsure, as in
  the labelled eval (0.74).

Still failing or ambiguous in both arms, and not about who decides:

- **dental-happy.** The caller's lines are fixed and drift from what the
  agent asks, so the booking is never confirmed.

Two harness faults hid earlier numbers. The spoken caller stopped sending
audio after each line's trailing silence, so 38 of 78 spoken turns waited
out the 75 s timeout; it now streams silence between lines, as a real
microphone does. And a check for the booking reference compared the
transcript with "tr-2044" while the agent says "T R two zero four four";
spelled-out numbers now match.

The judge catches what the checks miss: in one earlier flash run the
booking check passed, but the agent had told the caller "The booking is
confirmed" without booking, and Jev scored that unbacked claim at 0.77.

## Limits

- Text only. State is up to 32,000 tokens, and 64,000 per request including
  the questions.
- Probabilities are rounded to two decimals, so a distribution may not sum
  to 1.
- Calibration is TypeSafe's claim, measured over many predictions. It does
  not guarantee any single answer. Measure on your own calls before trusting
  a threshold.
