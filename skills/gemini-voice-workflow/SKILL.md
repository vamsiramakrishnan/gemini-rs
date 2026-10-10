---
name: gemini-voice-workflow
description: Build or change a Gemini Live voice agent (phone line, web voice widget, receptionist, booking, support, intake or IVR replacement) as an agent.json session spec, using the gemini-rs `adk` CLI to check the spec, ask the person the decisions that are theirs, and prove the flow with offline scenarios before any live call. Use this skill whenever someone wants a voice agent or phone bot on Gemini, asks to edit agent.json or a conversation spec, or describes a call flow ("verify the caller, then book…"), even if they never mention adk, gemini-rs or this skill.
---

# Gemini voice workflow

You are building a voice agent as **one JSON document**, `agent.json`. The
runtime (gemini-rs) enforces what the document says: which tools the model may
call in each stage, what must be true before a tool that changes something
can run, when to hand off. Your job is to turn the person's description into
that document, prove it offline, and keep every decision that belongs to the
person in front of them.

The CLI is `adk`. It needs the authoring commands (`adk spec catalog` must
work); from a gemini-rs checkout, install it with
`cargo install --path tools/gemini-adk-cli-rs`.

## The phases

Work through these in order. Each ends in a gate; don't skip ahead of a gate,
because everything after it is built on what it settles.

### 0. Brief

Ask about the business, one question at a time, only what the description
leaves open. These are the questions no tool can raise for you:

1. Who calls, and what are they trying to get done?
2. What must happen before anything changes: identity checks, read-backs,
   confirmations?
3. Which systems hold the data (an HTTP API, an MCP server, or not decided —
   then the tools stay stubs)?
4. What must be said word for word (recording or AI disclosure)?
5. When should a person take over, and how does the caller reach one?
6. What must never be said aloud or stored (full card numbers, dates of
   birth)?
7. What does a good call look like? These become the scenarios.

Write `brief.md`: the answers, the stages you intend, the tools, and the
scenarios you will write. **Gate:** the person approves the brief.

If the person cannot answer right now (a batch run, a ticket, an offline
request), don't stall and don't guess silently. Take the safest reasonable
answer — the one that keeps a gate, redacts, or hands off — and list each
one under **Assumptions** in `brief.md` so it can be reviewed.

### 1. Draft agent.json

Run `adk spec catalog` once and use its names and shapes: voices, guard
atoms, policies, tool bindings. Read `references/spec-language.md` for the
document and `references/patterns.md` for the stage patterns (verify
identity, collect and read back, commit after a yes, hand off, disclosure,
slow tools). Start from the worked example in spec-language.md rather than a
blank file.

Write a `conversation`, not a `flow`: it is the voice vocabulary (stages,
collected slots, commits, repair, digressions, verbatim lines).

### 2. Check, one fix at a time

```bash
adk spec check agent.json --json
```

Each diagnostic has a JSON pointer and, often, a `fix` whose `patch` you can
apply as is:

```bash
adk spec patch agent.json '<the fix.patch array>' --write
adk spec check agent.json --json      # again: fixes are computed against the checked file
```

Apply one fix, then check again; two fixes from the same report can touch the
same array. Read every fix before applying it. A rename or a did-you-mean is
mechanical. A fix that adds a `caller_signals` extractor field changes how
the agent decides something happened, so say so when you report back.

**Gate:** `check` reports no errors and you can explain every warning left.

### 3. Plan: the person's decisions

```bash
adk spec plan agent.json --decisions decisions.json --json
```

`plan` lists the decisions the spec leaves open. Put each **blocking**
question to the person in plain words, with its `why`; they are blocking
because the safe answer depends on the business (does this tool need a
spoken yes? is this slot sensitive?). For optional questions, offer the
`default` and take it if the person has no preference. Then record the
answers:

```bash
adk spec answer agent.json '[{"id": "commit_gate:book_table", "choice": "confirm"},
                             {"id": "voice", "choice": "Kore"},
                             {"id": "name", "value": "bright-smile"}]' \
  --decisions decisions.json --write
```

`answer` re-plans before each answer and applies all or nothing. Keep
`decisions.json` next to `agent.json`; it stops answered questions from
coming back. Check again after answering.

**Gate:** `plan` says `"ready": true`.

### 4. Prove it offline

Write `scenarios` in `agent.json` for every behaviour the brief promises,
especially the refusals: the commit tool denied before the yes, the handoff
path, the repair path. `references/scenarios.md` has the step vocabulary and
the timing rules that trip people up (`tool_ok` doesn't check admission, so
assert `expect_allowed` first; a verbatim stage waits for its line; a
handoff takes effect a turn later).

```bash
adk spec test agent.json      # validation, embedded tests and every scenario
adk spec graph agent.json     # Mermaid diagram of the stages
```

Show the person the graph and the list of scenarios. **Gate:** every scenario
passes.

### 5. Tools

```bash
adk spec codegen agent.json --lang python --out .   # typed stubs + MCP tool server
adk spec call agent.json check_availability '{"party_size": 4, "datetime": "2026-10-14T19:00"}'
```

Codegen never overwrites existing files without `--force`; stubs are where
the implementations go. A stub keeps returning the spec's mock response until
it is filled in, so scenarios keep passing while the real tool is written.
Run `adk spec test` again after binding a real tool.

### 6. Talk to it

`adk spec run agent.json` starts a live session. It needs credentials
(`GEMINI_API_KEY`, or Vertex AI settings) and, for audio, a CLI built with
`--features voice`. Ask before starting one: it calls a paid API and, if
tools are bound, real systems.

Deploying, attaching a phone number and promoting a bundle are outside this
skill; they always need the person's explicit go-ahead.

## Traps `check` can't see

A spec can pass `check`, `plan` and every scenario and still fail on a call.
Each of these has been hit in practice; patterns.md shows the shape that
avoids it.

- **`collect` doesn't listen.** A collected slot needs an `extract` entry
  (or a tool) that writes it, or the call never leaves the stage.
- **`allow` only holds while its stage is active.** After an escalation or a
  dead end nothing is active and every ungated tool is callable. Put a
  `commit` guard on every tool that reveals data or changes something.
- **A stage that checks something needs `done` on the check.** Otherwise it
  completes when its slots are captured, checked or not.
- **One handoff stage per escalating stage.** A stage several stages
  escalate to waits for all of them and is never reached.
- **Corrections clear later commit guards.** Keep only the confirmation flag
  as a state check there; express prerequisites with `called_ok`.
- **An HTTP call succeeds whatever its status.** Its `set_state` and
  `called_ok` hold on a 404 or 500, so never mark a verification or a
  submission as done that way.
- **Stubs succeed.** Until a tool is implemented it returns its mock
  `response`; make a verifier's mock a failing answer and gate on it.
- **Hand off through a digression that admits only the transfer tool.** A
  terminal handoff (including `safety_handoff`) admits every tool on the turn
  it starts.
- **Models claim success they didn't get.** In live runs the model said
  "your table is booked" right after its booking call was refused. Have the
  commit stage say to report the result only after the tool returns.
- **Every extractor is a model call the turn waits for.** A tool call that
  arrives meanwhile waits too, and on a phone line seconds of silence sound
  like a dead line. Keep extractors few (one for slots, one for signals).
  Pin the extraction model with `models.extraction`
  (`gemini-3.1-flash-lite` was correct and about 1 s per turn); the unpinned
  default, `gemini-flash-latest`, took up to 62 s under load, and
  `gemini-flash-lite-latest` left out a named medication and marked picking a
  time as agreeing to book.
- **A confirmation extractor should read two turns.** `"window": 2` sees the
  read-back and the reply; a wider window can find a "yes" from before a
  correction.

## Rules that protect the person

These exist because each one is a way a voice agent fails in production while
every test stays green.

- **Fix the flow, never the evidence.** Don't remove a gate, loosen a commit
  condition, delete or weaken a failing scenario, or answer a blocking
  question yourself to make a check pass. A failing scenario is the spec
  telling you the agent would misbehave on a call.
- **Blocking questions belong to the person.** Unless they told you to decide
  (then record it as an assumption), ask.
- **Don't change the model or the voice** unless asked; both change how
  every call sounds.
- **Redaction is narrower than it sounds.** A `redact` policy keeps a value
  out of the journal, snapshots and extraction events; transcripts are a
  separate concern. Don't tell anyone their data is not retained.
- **No real caller data in the repository**: no recordings, transcripts or
  phone numbers in scenarios or fixtures.
- **Nothing outward without a yes**: live calls, deploys, phone numbers,
  bundle labels.
- **Stop after the same error three times.** Report what you tried and what
  the CLI said instead of looping.

## Editing the JSON

Draft the whole file in phase 1. After that, prefer `adk spec patch` (or a
`fix` or `answer`) for targeted changes: each is atomic and re-checked, and
the diff stays readable. `--write` saves keys in sorted order.

## Reporting back

End each working session with: what the agent now does (stages in one line
each), the scenarios and their result, the decisions taken and by whom
(person or assumption), and what is still open — for example stub tools that
need real implementations.
