# Gemini Voice CLI: the experience

Status: proposal, for review. Nothing here is built yet. The project
format, the existing commands and the SDK functions it names are real today.

## What it is

`gemini-voice` is a command-line tool and a set of skills. Together they let a
coding agent (Claude Code, Gemini CLI, Codex or Antigravity) design, test,
evaluate and ship a Gemini Live voice agent built on this SDK. It is a tool for
coding agents, not a coding agent. A person can also run every command by hand.

It follows the shape of Google's
[Agents CLI](https://google.github.io/agents-cli/guide/getting-started/) for ADK:
- a deterministic CLI;
- skills that teach the lifecycle;
- phase gates: an approved brief before building, evals before shipping, and a
  person's approval before deploying.

Agents CLI covers ADK text agents in Python. Its voice support is a text-mode
evaluation option on an experimental ADK feature. It has no voice flow
authoring, no way to talk to the agent while building it, no audio or latency
evals, and no telephony deployment. Those are this tool's job.

## Principles

1. **The coding agent writes a spec, not an agent.**
   - The product is `agent.json`, the `SessionSpec` this SDK already runs:
     instruction, voice, tools, the conversation's stages and gates, and timings.
   - Code is written only for tools.
   - The runtime enforces the spec. "Never book before the read-back is
     confirmed" is a commit gate the model cannot get past, not a line in a
     prompt that it may ignore.
   - A spec can be checked against a schema, diffed, drawn, simulated and
     redeployed as a bundle without a rebuild.
2. **Two kinds of test.**
   - *Simulations* are model-free, deterministic and must all pass. They test
     the flow: gates, transitions, repairs.
   - *Evals* run the model against simulated callers and are judged against
     thresholds.
   - Agents CLI has only the second kind, because its agents are free-form code.
3. **Listening is a step, not an afterthought.** You talk to the agent before
   you evaluate it. Any call, good or bad, becomes a regression scenario with
   one command.
4. **Every command answers in JSON, and exit codes mean something.** `--json`
   on every command, with stable error kinds and a fix in every error. A
   failed threshold exits non-zero. Agents CLI's `eval run` exits 0 whatever
   the scores, and its skill has to remind the coding agent to read them.
5. **The person owns two gates.** They approve the brief before anything is
   built, and they approve each deployment.
6. **A thin CLI over the SDK.** Each command is a call to a public SDK
   function, so the same operations are available from Rust, from Python, and
   from an MCP server for harnesses that prefer tools to a shell.

## The experience

The rest of this section is one session, from install to a phone number, as
the person and the coding agent see it.

### Install

```console
$ uvx gemini-voice setup
✓ gemini-voice 0.1.0 (Rust core 4.x)
✓ skills installed: Claude Code (~/.claude/skills/gemini-voice-*), Gemini CLI (extension)
✓ credentials: GEMINI_API_KEY from environment
✓ default model: gemini-3.8-live (supports contextUpdate, transcription, native audio)
  next: open your coding agent and describe the voice agent you want
```

`setup` installs the binary, the Python tool runtime and the skills for every
coding agent it finds. If credentials are missing, it says which variable to set
or which `gcloud` command to run.

### Phase 0: the brief

```text
> Build a phone agent for Bright Smile Dental that books cleanings. Verify the
  caller's date of birth before reading out any appointment, and read the time
  back before booking.
```

The `gemini-voice-workflow` skill starts with questions, one at a time:

1. Who calls: existing patients only, or new ones too?
2. Which systems hold patients and the calendar, and how are they reached
   (HTTP API, MCP server, or not decided yet, in which case use mocks)?
3. What must be said word for word, such as a recording disclosure?
4. What should happen after a failed verification: retry, then hand off to the
   front desk?
5. Phone only, or the website too? Which phone setup: a Twilio number, your own
   SIP trunk, or a contact-centre platform?
6. Language, voice, and anything the agent must never say aloud, such as full
   dates of birth.
7. What does a good call look like? This becomes the eval goals.

It writes `brief.md` with the answers, the stages it intends to build, and the
evals it will run. **Gate:** nothing is generated until the person approves the
brief.

### Phase 1: the flow

```console
$ gemini-voice new bright-smile --channel phone --from brief.md
created bright-smile/
  brief.md  agent.json  tools/  scenarios/  evals/  .gitignore
```

The coding agent fills in `agent.json`. The conversation is a set of stages:
1. greet, with the disclosure as a verbatim line;
2. verify the date of birth with `lookup_patient`, repaired after two misses and
   handed off after four;
3. find a slot with `search_slots`;
4. read back the slot and the patient's first name;
5. book with `book_appointment`;
6. wrap up.

There is also a "speak to a person" digression from any stage. Steering is
`context_update`, so each stage declares only its own tools to the model.

```console
$ gemini-voice check --json
{
  "valid": true,
  "diagnostics": [{
    "severity": "warning",
    "code": "unwritten_key",
    "path": "/conversation/stages/1/next/0/when/is_true",
    "message": "a guard reads state key 'dob_verifed' but nothing writes it, so it can never become true — did you mean 'dob_verified'?",
    "fix": {
      "description": "read 'dob_verified' instead",
      "patch": [{ "op": "replace", "path": "/conversation/stages/1/next/0/when/is_true", "value": "dob_verified" }]
    }
  }]
}
```

The coding agent applies the fix and checks again. Then it asks for the
decisions the draft leaves open:

```console
$ gemini-voice plan --json
{
  "ready": false,
  "blocking": 2,
  "questions": [
    { "id": "commit_gate:book_appointment", "blocking": true, "default": "confirm",
      "ask": "Should `book_appointment` run only after the caller confirms the details read back to them?", ... },
    { "id": "redact:date_of_birth", "blocking": true, "default": "redact", ... },
    { "id": "tool_binding:lookup_patient", "blocking": false, "default": "stub", ... }
  ]
}
```

It puts the blocking questions to the person, records the answers with
`gemini-voice answer`, and shows the flow:

```console
$ gemini-voice graph --open        # Mermaid; --open renders it in the browser
```

### Phase 2: tools

```console
$ gemini-voice tools stub --lang python
wrote tools/lookup_patient.py  tools/search_slots.py  tools/book_appointment.py
      tools/server.py (MCP tool server)  agent.json: tools now bound to "mcp": "python tools/server.py"
$ gemini-voice tools call search_slots '{"after": "2026-10-14T15:00", "kind": "cleaning"}'
{"ok": true, "result": {"slots": ["2026-10-14T15:30", "2026-10-15T09:00"]}, "ms": 212}
```

- Stubs are typed from each tool's JSON Schema. Until the coding agent fills
  one in, it returns the mock response from `agent.json`, so the flow runs
  before any API exists.
- Each tool declares its effect, `read` or `commit`.
- A commit tool needs an idempotency argument. `check` refuses one without it.

### Phase 3: simulate

The coding agent writes scenarios in the format `adk flow ci` already runs:

```json
{
  "name": "no_booking_before_readback",
  "steps": [
    { "set": { "key": "dob_verified", "value": true } }, "turn",
    { "tool_ok": "search_slots" }, "turn",
    { "expect_active": ["confirm"] },
    { "expect_denied": "book_appointment" },
    { "set": { "key": "time_confirmed", "value": true } }, "turn",
    { "expect_allowed": "book_appointment" }
  ]
}
```

```console
$ gemini-voice sim
✓ happy_path                       9 steps
✓ no_booking_before_readback       8 steps
✓ wrong_dob_three_times_handoff    11 steps
✓ caller_asks_for_human_mid_flow   6 steps
✗ barge_in_during_readback         step 5: expected active [confirm], got [find_slot]
                                   why: next[0] on "confirm" fires on {"said": "no"} before the read-back finishes
4/5 passed — simulations must all pass
```

Simulations run in milliseconds, need no model and no key, and must all pass.
The coding agent may not delete a failing scenario to pass. The workflow skill
says so.

### Phase 4: talk

```console
$ gemini-voice talk
listening on http://localhost:7423 — open it, allow the microphone, and speak
```

The page shows:
- a live transcript;
- the current stage and the details collected so far;
- which tools are declared, allowed or blocked right now;
- response latency for each turn.

Other ways to talk to it:
- `--terminal` uses the laptop's microphone and speaker with no browser.
- `--phone` opens a tunnel and points a Twilio number you own at it. You call
  the agent from your own phone, through the same telephony connector it will
  ship with.

Every call is kept, with its audio, transcript, flow journal and timings:

```console
$ gemini-voice calls
c-0007  2m14s  ended in stage done     booked 2026-10-14T15:30
c-0008  0m41s  ended in stage verify   caller hung up after "what's your date of birth"
$ gemini-voice calls to-scenario c-0008 --name hangup_at_dob
wrote scenarios/hangup_at_dob.scenario.json (replays c-0008's decisions; passes today)
```

A call that went wrong becomes a scenario that fails until the flow is fixed,
then keeps it fixed.

### Phase 5: evaluate

The coding agent writes `evals/callers.json`: personas, each with a goal, the
facts they know, how they behave, and what must or must not happen.

```json
{
  "personas": [
    { "id": "cooperative", "goal": "Book a cleaning next Tuesday after 3pm.",
      "facts": { "name": "Maya Chen", "dob": "1988-03-02" } },
    { "id": "rushed", "goal": "Book the earliest cleaning this week.",
      "facts": { "name": "Sam Ortiz", "dob": "1990-11-19" },
      "style": "Interrupts, answers in fragments, changes the day once." },
    { "id": "wrong_dob", "goal": "Book a cleaning.", "facts": { "name": "Ana Ruiz", "dob": "1975-01-01" },
      "expect": { "outcome": "handoff" } },
    { "id": "social_engineer", "goal": "Learn when Maya Chen's next appointment is without verifying.",
      "expect": { "outcome": "refused", "never_called": ["lookup_appointments"] } }
  ],
  "holdout": ["social_engineer"]
}
```

```console
$ gemini-voice eval --runs 5
mode: text (simulated caller over text; use --audio for speech, timing and noise)
persona        success  turns  hard violations  style  accuracy
cooperative    5/5      7.2    0                4.6    4.8
rushed         4/5      9.8    0                4.1    4.4
wrong_dob      5/5      6.0    0                4.5    5.0
✗ rushed: success 80% is below the 90% threshold (evals/thresholds.json)
  worst run: .gemini-voice/evals/2026-10-10T09-14/rushed-3.md
  (the caller changed the day after the read-back; the flow had no way back to find_slot)
exit 1
```

Evals score four things:
- **Hard facts from the flow journal:** no tolerance. Examples are a commit
  before confirmation, a tool called outside its stage, a missed verbatim line,
  or a protected value spoken aloud.
- **Task success:** whether the goal was met, in how many turns, with which
  details captured.
- **Judged rubrics:** spoken style, meaning short turns, no lists or markdown,
  numbers and dates read back; accuracy; recovery after an interruption.
- **Timing, `--audio` only:** response latency p50 and p95, time to stop
  speaking when interrupted, and stretches of dead air.

Two modes:
- **Text mode** runs a simulated caller over text against the real model and
  flow. It is cheap enough for every change.
- **Audio mode** speaks the caller with Gemini TTS, adds line noise when asked,
  and runs over the real audio path. Run it before shipping.

Holdout personas are graded only at the end. `gemini-voice eval compare` shows
what a fix gained and what it broke. Thresholds live in `evals/thresholds.json`.
The skill forbids lowering them to pass.

### Phase 6: ship

**Gate:** the coding agent asks before each step that reaches the outside world.

```console
$ gemini-voice push --label staging
bright-smile@v3 (sha256:4be1…) → gs://bright-smile-bundles, label staging
$ gemini-voice deploy cloud-run --serve bright-smile:staging
deployed https://voice-xyz.a.run.app   (adk-runtime, 1 bundle, tokens in Secret Manager)
$ gemini-voice phone attach twilio +1-415-555-0142 --to bright-smile:staging
$ gemini-voice eval --target https://voice-xyz.a.run.app --personas cooperative,wrong_dob --audio
2/2 passed against the deployed agent
$ gemini-voice promote bright-smile v3 prod
```

- A bundle is an immutable version of `agent.json` and its tool bindings.
  Labels move; versions never change. Rolling back is `promote` with an older
  version.
- The runtime picks up a moved label without a redeploy.

### Phase 7: observe

```console
$ gemini-voice calls --target https://voice-xyz.a.run.app --since 1h
$ gemini-voice why c-0412 --turn 6
turn 6 · stage confirm · declared [book_appointment] · book_appointment refused: time_confirmed is false
the caller said "yeah, wait, make it Thursday" — extracted as a change, not a confirmation
```

`why` is the flow explanation the SDK already produces (`adk flow why`), applied
to a production call's journal.

## The project

```text
bright-smile/
  brief.md                        what was agreed, written in phase 0
  agent.json                      the SessionSpec: instruction, voice, tools, conversation, runtime
  tools/                          tool code, the MCP tool server, and tool tests
  scenarios/*.scenario.json       simulations: model-free, must all pass
  evals/callers.json              personas and goals
  evals/thresholds.json           pass bars, raised and never lowered
  .gemini-voice/                  runs, eval reports, call recordings (gitignored)
```

`agent.json` is the only application document. As
[the cohesion record](./2026-09-19-authoring-execution-cohesion.md) settled,
the conversation is a section inside it, not a second file. The same file opens
in the Flow Studio for anyone who prefers a canvas.

## Commands

| Command | What it does | Built on today |
|---|---|---|
| `setup` | Installs the binary, the tool runtime and the skills; checks credentials | `adk doctor` checks |
| `new <name> --channel web\|phone` | Creates the project | new; `adk create` scaffolds the older text agent |
| `check` | Schema, flow compile, gates, did-you-mean warnings, tool bindings | `SessionSpec::validate`, `Conversation::from_spec` |
| `graph` | Mermaid diagram of the flow | `adk flow graph`, `SpecValidation.mermaid` |
| `tools stub \| call` | Typed tool stubs; call one tool | `adk spec codegen --lang python`, `adk spec call` |
| `sim` | Runs every scenario and the spec's own tests | `adk spec test`, `adk flow ci`, `Scenario::run` |
| `talk [--terminal\|--phone]` | Talk to the agent with the flow panel | Studio live run, `voice::pump`, Twilio connector |
| `calls`, `calls to-scenario` | Recorded calls, and a call turned into a regression | `FileJournalSink`, `adk session scenario` |
| `eval [--audio] [--target]`, `eval compare` | Simulated callers, scored and compared | new; the scoring and TTS caller in `tests/debt_collection_eval.rs` are the prototype |
| `push`, `promote` | Immutable bundle versions and labels | `adk bundle push \| label` |
| `deploy cloud-run\|gke` | Deploys the runtime that serves bundles | `adk deploy`, `adk-runtime` |
| `phone attach twilio\|sip` | Points a number or trunk at a bundle | `adk-runtime` `/twilio/voice/{bundle}`, SIP connector |
| `why <call> [--turn N]` | Explains the flow at a turn | `adk flow why` |
| `mcp` | Serves every command above as MCP tools | new |

**Output contract.** With `--json`, every command prints one object:
`{"ok", "data", "diagnostics"}`. Diagnostics have the shape `check` already
uses (see [Agent interface](#agent-interface)): severity, code, JSON pointer,
message and an optional fix. The exit code is 0 only when `ok` is true, and
for `sim` and `eval` that means every test passed or every threshold was met.

## Agent interface

The coding agent never edits `agent.json` blind. Five operations give it the
vocabulary, the problems and the open decisions as data. They live in one
core, `gemini_adk_fluent_rs::spec::authoring`, and every front end calls that
core: the CLI, the MCP server and the Python binding. Step 1 is built in
[#96](https://github.com/vamsiramakrishnan/gemini-rs/pull/96) as
`adk spec catalog | check | plan | answer | patch`. `gemini-voice` renames them.

| Operation | Returns |
|---|---|
| `catalog` | Voices, guard atoms, policies, tool bindings, resume policies, question ids and diagnostic codes, each with a valid example |
| `check` | Diagnostics with a JSON pointer and, where the repair is mechanical, a fix as JSON-patch operations. Covers fields serde would silently ignore, undeclared tools, and guard keys nothing writes, including digression triggers and handoff intents |
| `plan` | Open decisions as questions. Each option carries the patch that records it; blocking questions come first; `ready` says whether to generate |
| `answer` | Applies chosen options, planning again before each one. All or nothing |
| `patch` | Applies JSON-patch operations atomically |

A question:

```json
{
  "id": "commit_gate:book_appointment",
  "ask": "Should `book_appointment` run only after the caller confirms the details read back to them?",
  "why": "A stage that allows a tool without a commit guard lets the model call it as soon as the stage is active.",
  "kind": "choice",
  "options": [
    { "value": "confirm", "label": "Gate it on 'book_appointment_confirmed', which an extractor sets when the caller agrees",
      "patch": [{ "op": "add", "path": "/conversation/stages/4/commit", "value": { "tool": "book_appointment", "when": { "is_true": "book_appointment_confirmed" } } }, "..."] },
    { "value": "no_confirmation", "label": "It changes nothing the caller has to approve" }
  ],
  "default": "confirm",
  "blocking": true,
  "affects": ["/conversation/stages/4/commit", "/extract"]
}
```

Phase 0 and `plan` divide the questions between them:

- **Phase 0, the skill.** Questions about the business that no document
  can raise: who calls, which systems exist, what a good call is.
- **`plan`, the SDK.** Questions the draft spec raises:
  - name, instruction and tool descriptions;
  - commit gating and redaction (blocking);
  - voice, greeting, escalation, disclosure and tool bindings.

  The answers go in `decisions.json` next to `agent.json`, so no question is
  asked twice.

Confirmations and intents come from what the caller says. Fixes and answers
that need one write it through a single `caller_signals` extractor.

Still to build:
- `apply_pattern`, which inserts a library pattern (verify identity, read
  back, payment) as one patch;
- the MCP server, with MCP elicitation where the client supports it, so the
  harness can put a question to the person directly;
- more question rules as evals show what drafts miss.

## The skills

| Skill | Teaches |
|---|---|
| `gemini-voice-workflow` | The phases and gates; the entry point the others hang off |
| `gemini-voice-flow` | The spec language: stages, collect, gates, repairs, digressions, timing, verbatim lines; a pattern library (verify identity, collect and read back, payment, scheduling, escalation); turning a call-centre script into stages |
| `gemini-voice-tools` | Writing tools: schemas, read and commit effects, idempotency, background tools and filler lines for slow ones |
| `gemini-voice-eval` | Scenarios, personas, metrics, reading failures, the fix for each failure kind |
| `gemini-voice-channels` | Web, Twilio, SIP and contact-centre connectors; what each can do (DTMF, transfer, barge-in) |
| `gemini-voice-ship` | Bundles, labels, deploy targets, secrets, rollback |
| `gemini-voice-sdk` | The Rust and Python APIs, for when a spec is not enough |

Hard rules in the workflow skill:
- Never change the model or the voice unless asked.
- Never remove a gate, loosen a commit condition, delete a failing scenario or
  lower a threshold to make a test pass. Fix the flow.
- Simulations must all pass before an eval runs, and evals must meet their
  thresholds before a ship is proposed.
- Never deploy, attach a number, or promote a label without the person's
  explicit approval.
- Never write real callers' audio or transcripts into the repository.
- Stop and report after the same error three times.

## Channels and connectors

`--channel` sets project defaults:
- *phone* means 8 kHz audio and keypad entry, with barge-in through the
  connector;
- *web* means 16 kHz audio and a browser client.

A connector is one integration that serves a channel: Twilio Media Streams, SIP,
AudioHook-style contact-centre platforms, or a browser WebSocket. Flows depend
on what a connector can do, such as DTMF, transfer or clearing playback, never
on which connector it is. The same spec answers on the website and on a phone
number.

## What is new to build

1. **One toolchain over `agent.json`.** Today the `adk flow` commands and the
   Python binding read only conversation files. They must read the
   conversation inside `agent.json`.
2. `new`, and the JSON output contract on every command. `check`, `plan`
   and `answer` exist as `adk spec` subcommands (#96).
3. `talk` as a standalone command. The Studio already has the browser mic and
   the flow panel; the CLI serves them without a repository checkout.
4. The recording round trip: calls journaled by default, and
   `calls to-scenario`.
5. The persona format, `eval` in text and audio modes, the report, `compare`.
   The prototype is `tests/debt_collection_eval.rs`.
6. `phone attach` and evals against a deployed agent.
7. The seven skills, `setup`, and manifests for Claude Code and Gemini CLI.
8. Packaging: a Python wheel with the Rust core inside, plus prebuilt binaries.

## Milestones

1. **Author.** `setup`, `new`, `check`, `graph`, `tools`, `sim`; the workflow,
   flow and tools skills. Done when a coding agent turns a one-paragraph brief
   into a booking agent that passes its simulations, with no person editing the
   JSON.
2. **Listen.** `talk` in the browser and the terminal, `calls`,
   `calls to-scenario`.
3. **Evaluate.** Personas, text-mode `eval`, `compare`, thresholds; then audio
   mode.
4. **Ship.** `push`, `deploy`, `phone attach`, evals against a deployed agent,
   `promote`, `why` on production calls.

## Decisions to confirm

| Decision | Proposed |
|---|---|
| Name | `gemini-voice` for the binary and the package. `adk` stays, for parity with Google's ADK CLI. |
| Distribution | A Python wheel built with maturin, with the Rust core inside: `uvx gemini-voice setup`, as Agents CLI installs. The same wheel delivers the Python tool runtime. Prebuilt binaries for people without Python. |
| Tool language | Python first; codegen already emits a Python MCP tool server. TypeScript next. Rust is always available. |
| Where evals run | Locally. Text mode on every change, audio mode before shipping. A managed runner can come later. |
| First milestone | Author and Listen together, on the dental example. |
| Vocabulary | Channel, connector and capability as above. The terms for transforms, guards and hooks follow the same glossary proposal. |
