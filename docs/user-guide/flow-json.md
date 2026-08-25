# Flows as JSON (and the Flow Studio)

Because every [`Guard`] atom is a named, parameterized predicate, a `Flow` is
fully serializable — the same DAG you build with `Flow::new()…build()` in Rust
round-trips through JSON. This page documents that JSON format; the
**`SessionSpec`** document (`gemini_adk_fluent_rs::spec`) that turns a flow
into a complete runnable application — tools, extraction, phases, watchers,
fragments, and an embedded test suite — without writing code; and the
**Flow Studio**, the drag-and-drop editor shipped with `gemini-adk-web-rs`
that reads, writes, validates, tests, and live-runs these documents.

## Loading a flow from JSON

`Flow` derives `Serialize`/`Deserialize`, so this is plain serde:

```rust,ignore
let flow: Flow = serde_json::from_str(&std::fs::read_to_string("flow.json")?)?;
let compiled = flow.compile_with_tools(&["verify_identity", "charge_card"])?;

let handle = Live::builder()
    .tools(dispatcher)
    .govern_compiled(compiled)
    .connect_from_env()
    .await?;
```

The only thing that does not serialize is `Guard::custom(..)` — the code-only
escape hatch. Serializing a flow containing one is an error, never a silent
drop.

## The flow JSON format

A flow document has `steps`, and optionally `constraints`, `ambient`, and
`confirm_tools`:

```json
{
  "steps": [
    {
      "id": "verify",
      "posture": "Verify the caller's identity before anything else.",
      "allow": ["verify_identity"],
      "done": { "is_true": "identity_verified" }
    },
    {
      "id": "take_payment",
      "after": ["verify"],
      "allow": ["charge_card"],
      "done": { "called_ok": "charge_card" }
    },
    { "id": "close", "after": ["take_payment"], "terminal": true }
  ],
  "constraints": [
    { "once": "charge_card" },
    { "never_until": { "tool": "charge_card", "until": { "is_true": "ptp_confirmed" } } },
    { "require": ["close"] }
  ],
  "ambient": ["recall_context"]
}
```

### Step fields

| Field | Type | Meaning |
|---|---|---|
| `id` | string | Unique step id (required) |
| `after` | [string] | Dependency step ids — the DAG edges |
| `gate` | Guard | Extra eligibility beyond dependencies |
| `done` | Guard | Completion condition (required unless `terminal`) |
| `posture` | string | Instruction imposed while active |
| `ground` | string | State-interpolated fact template (`{key}` / `{key?yes:no}`) |
| `allow` | [string] | Tool whitelist while active (empty = no restriction) |
| `deny` | [string] | Tools forbidden while active |
| `terminal` | bool | Completes on eligibility, needs no `done` |

### Guard atoms

Guards are the serde encoding of the `Pred` enum (externally tagged,
snake_case):

| JSON | Rust constructor |
|---|---|
| `"always"` | `Guard::always()` |
| `{ "is_true": "key" }` | `Guard::is_true("key")` |
| `{ "is_set": "key" }` | `Guard::is_set("key")` |
| `{ "eq": ["key", value] }` | `Guard::eq("key", value)` |
| `{ "captured": ["a", "b"] }` | `Guard::captured(["a", "b"])` |
| `{ "called_ok": "tool" }` | `Guard::called_ok("tool")` |
| `{ "done": "step" }` | `Guard::done("step")` |
| `{ "all": [g, …] }` | `Guard::all([...])` |
| `{ "any": [g, …] }` | `Guard::any([...])` |
| `{ "not": g }` | `Guard::not(g)` |

### Graph edges: branching, merging, and loops

`after` entries are plain strings (unconditional) **or conditional edges** —
objects whose guard must hold for the edge to be satisfied. Conditional edges
out of one source are how a flow branches; `"join": "any"` on the merge step
accepts whichever branch completes. The canvas renders conditional edges
dashed with their guard as the label:

```json
{ "id": "schedule",
  "after": [ { "step": "triage", "when": { "not": { "is_true": "is_emergency" } } } ] },
{ "id": "close",
  "after": [ "schedule", { "step": "triage", "when": { "is_true": "is_emergency" } } ],
  "join": "any", "terminal": true }
```

Loops are **not** back-edges — the DAG stays acyclic and statically
checkable. Iteration is explicit marking surgery: a `reset` constraint
un-latches steps on its guard's *rising edge*, re-arms their `on_enter`, and
forgives the `called_ok` evidence their completion guards reference (so a
`once` on such a tool counts per latch-cycle; state keys stay the
application's to clear):

```json
{ "reset": { "steps": ["diagnose"], "when": { "is_true": "retest_requested" } } }
```

### Constraints

| JSON | Meaning |
|---|---|
| `{ "once": "tool" }` | The tool may complete at most once |
| `{ "before": ["a", "b"] }` | Step `a` must be done before `b` starts |
| `{ "never_until": { "tool": t, "until": g } }` | Forbid `t` until `g` holds |
| `{ "require": ["a", "b"] }` | Steps required for flow completion |
| `{ "reset": { "steps": [...], "when": g } }` | Un-latch steps on `g`'s rising edge (loops) |

## `SessionSpec` — a runnable application as one JSON file

A **session spec** (`gemini_adk_fluent_rs::spec::SessionSpec`; re-exported by
`gemini-adk-server-rs` as `FlowAppSpec` for compatibility) wraps a flow with
everything a governed Live session needs:

```json
{
  "name": "collections",
  "instruction": "You are a debt-collection assistant.",
  "greeting": "Greet the caller and ask for their name.",
  "modality": "text",
  "tools": [
    {
      "name": "verify_identity",
      "description": "Verify the caller's identity.",
      "parameters": { "type": "object", "properties": { "name": { "type": "string" } } },
      "response": { "verified": true },
      "set_state": { "identity_verified": true }
    }
  ],
  "extract": [ … ],
  "state": { … }, "computed": [ … ],
  "memory": { … },
  "phases": [ … ],
  "watch": [ … ], "patterns": [ … ],
  "runtime": { … },
  "fragments": { … }, "use_fragments": [ … ],
  "tests": [ … ],
  "flow": { "steps": [ … ], "constraints": [ … ] }
}
```

In Rust, `SessionSpec::apply(live, &state, &resources)` configures a `Live`
builder from the document; code-only concerns (callbacks, custom guards,
middleware) are added on the returned builder afterwards. `GET
/api/flows/schema` serves the document's own JSON Schema — for editor
autocomplete, and for validating machine-authored specs at generation time.

### Tools: mock, HTTP, MCP

A declared tool without a binding is a **mock**: it returns its canned
`response` (default `{"ok": true}`) and writes `set_state` into the session
`State`. Because flow guards read the same state, `is_true`/`captured`
conditions latch exactly as against real implementations — model, enforce, and
demo the whole conversation before any real tool exists.

Add an `"http"` binding and the same tool performs a request instead
(`http-tools` feature; `{args.field}` / `{state.key}` interpolate into the
URL, headers, and body; `save_response_as` stores the response under a state
key; `set_state` still applies, so guards latch identically):

```json
{ "name": "check_availability",
  "http": { "method": "GET", "url": "https://api.example.com/slots?guests={args.party_size}" },
  "save_response_as": "availability", "set_state": { "availability_checked": true } }
```

MCP toolsets plug in the whole ecosystem with one line, resolved at connect
time:

```json
"mcp": ["http://localhost:3000/mcp"]
```

### Extraction: the flow advances from speech alone

`extract` entries run an out-of-band model against the transcript, fill a JSON
schema, and **promote** fields into the bare state keys guards read — so a
`captured(["ptp_amount","ptp_date"])` guard latches with no tool call
anywhere:

```json
"extract": [{
  "name": "promise_to_pay",
  "instruction": "Extract the payment amount and date the caller agrees to.",
  "schema": { "type": "object", "properties": {
    "ptp_amount": { "type": "number" }, "ptp_date": { "type": "string" } } },
  "trigger": "every_turn",
  "promote": [ { "field": "ptp_amount" },
               { "field": "ptp_date", "policy": "overwrite" } ]
}]
```

Promotion policies: `keep_known` (default), `overwrite`, `true_only`,
`non_empty`; `"to"` renames the target key. (`Live::extract_json` is the
code-level equivalent.) Running extraction needs a model:
`SpecResources.extraction_llm`.

### State dictionary and computed variables

The `state` section is the session's **data dictionary**: declare each key's
type, meaning, and starting value. Declaring it is optional, but once present
it powers autocomplete in the Studio's guard editors, undeclared-key warnings
at validation, typed `StateKey` constants in generated code, and `default`
seeding at connect:

```json
"state": {
  "party_size":          { "type": "number",  "description": "Guests in the party." },
  "availability_checked": { "type": "boolean", "default": false }
}
```

`computed` entries are **derived variables as data** — the counterpart to
`Guard` for values. A closed expression vocabulary (`key`, `const`, `add`,
`mul`, `sub`, `div`, `min`, `max`, `eq`, `gt`, `gte`, `lt`, `lte`, `all`,
`any`, `not`, `if`, `coalesce`, `concat`, `count_true`) evaluates over state;
the result is written to `derived:{key}`, dependencies are inferred from the
expression, dependency cycles are load-time errors, and guards read the result
by its bare key (the `derived:` fallback):

```json
"computed": [{
  "key": "large_party",
  "from": { "gte": [ { "key": "party_size" }, { "const": 6 } ] }
}]
```

A step can then `done`/`gate` on `{"is_true": "large_party"}`, a watcher can
watch it, and the offline simulator recomputes it after every event — the same
semantics in the live runtime, the simulator, and generated code.

### Durable memory

The `memory` section installs the memory subsystem (`gemini-memory-rs`)
declaratively: the ambient `recall_context` / `manage_memory` tools, turn
ingestion and end-of-session reconciliation, and **slots** that project
remembered facts into governed state keys — where `needs`, `captured`, and
every guard read them exactly as if the caller had just said it, so a
returning caller is never asked twice:

```json
"memory": {
  "slots": [
    { "predicate": "dietary_identity", "to": "user:diet" },
    { "predicate": "seating_preference", "to": "user:seating" }
  ]
}
```

At apply time the engine arrives through `SpecResources.memory` — any
`MemoryBinding` implementation; `gemini_memory_rs::runtime::SessionMemoryBinding`
wraps a `MemorySession`. The Studio provisions an in-process engine per run
automatically. The `remember` effect (below) writes through the same binding.

### Phases, watchers, and patterns over the same guard vocabulary

Phase transitions, watcher conditions, and pattern conditions reuse the closed
`Guard` atoms; handlers are closed `EffectSpec`s — one vocabulary honored
identically wherever effects fire:

| Effect | Meaning |
|---|---|
| `{"set": {…}}` | write state keys |
| `{"context": "…"}` | inject a model-role steering turn (read, not answered) |
| `{"prompt": "…"}` | inject text **and** ask the model to respond now — the "make the model speak" effect |
| `{"remember": "…"}` | durably remember a note (`{state.key}` interpolates; needs the `memory` section) |

```json
"phases": [
  { "name": "greeting", "instruction": "Welcome the caller.",
    "transitions": [ { "to": "main", "when": { "is_true": "greeted" } } ],
    "on_enter": [ { "set": { "entered": true } } ] },
  { "name": "main", "tools": ["search"], "needs": ["topic"], "terminal": false }
],
"initial_phase": "greeting",
"watch": [
  { "key": "large_party", "condition": "became_true",
    "effects": [ { "context": "Mention the set-menu policy." },
                 { "remember": "Books for large parties ({state.party_size} guests)" } ] }
]
```

Watchers receive the live session writer, so the full effect vocabulary
applies — a watcher can steer, prompt, or remember, not only mutate state.
(`set` remains as sugar for a leading `{"set": …}` effect.)

Phase guards evaluate against state alone (there is no flow marking at a
phase boundary), so `called_ok`/`done` atoms there are a validation *error* —
latch a state key instead.

**Temporal patterns** fire when a condition holds continuously — for wall
seconds or consecutive turns — the "sounded confused for 30 seconds" reactor,
as data:

```json
"patterns": [{
  "name": "stuck", "when": { "is_true": "repeating" }, "turns": 3,
  "effects": [ { "set": { "needs_help": true } },
               { "context": "The caller seems stuck — offer to summarize the options." } ]
}]
```

### Runtime tuning: the control plane as data

Everything that is configuration rather than conversation lives in one
`runtime` section; every field lowers to a `Live` builder setter, and omitted
fields keep the builder's defaults:

```json
"runtime": {
  "temperature": 0.6,
  "thinking_budget": 1024, "include_thoughts": true,
  "transcription": { "input": true, "output": true },
  "proactive_audio": true,
  "vad": { "start_sensitivity": "high", "end_sensitivity": "low",
           "silence_duration_ms": 400 },
  "soft_turn_timeout_ms": 1500,
  "steering": "context_injection",
  "context_delivery": "deferred",
  "repair": { "nudge_after": 2, "escalate_after": 5 },
  "persistence": { "fs": { "dir": "/var/sessions" } },
  "session_id": "user-123"
}
```

Per-tool async calling rides on the tool declaration itself:
`"background": true` marks a tool non-blocking (the model keeps speaking while
it runs), and `"scheduling": "interrupt" | "when_idle" | "silent"` picks how
the async response lands (scheduling implies background).

### Fragments: reusable flow modules

Declare a flow fragment once, splice it anywhere under a namespace — step ids
become `{namespace}/{id}` and internal `after`/`done(step)`/constraint
references are rewritten:

```json
"fragments": { "verify": { "steps": [ … ] } },
"use_fragments": [ { "fragment": "verify", "namespace": "v", "after": ["greet"] } ],
"flow": { "steps": [ { "id": "book", "after": ["v/confirm"], … } ] }
```

This is how a compliance team owns `disclosure` once and every flow imports it.

### Embedded tests: conformance without an API key

`tests` script conversations as data and assert flow state at checkpoints. The
script replays through the *real* `FlowMonitor` with the declared tools' mock
semantics — offline, no model, CI-friendly (`POST /api/flows/test`, the
Studio's **Tests** button, or `spec.run_tests()`):

```json
"tests": [{ "name": "premature charge is blocked", "script": [
  { "tool": "charge_card" },
  { "expect": { "blocked": ["charge_card"], "complete": false } }
]}]
```

Events: `{"user": "…"}` (turn boundary), `{"tool": "name"}` (mock semantics +
completion, or a failure if the flow blocks it unexpectedly), `{"set": {…}}`
(stands in for extraction), `{"expect": {done, active, allowed, blocked,
state, complete}}`.

### Validation: everything that can fail, fails at load time

`SessionSpec::validate` runs the flow compiler with the declared tools as the
registry, checks fragments splice, rejects marking atoms in phase guards — and
diffs the state keys guards **read** against the keys the session **writes**
(tool `set_state`, extraction promotions, phase/watcher effects). A guard
waiting on a key nothing sets is the dominant silent failure in data-authored
flows; it now surfaces as a warning with a did-you-mean suggestion:

```
a guard reads state key 'identity_verifed' but no tool, extractor, phase, or
watcher writes it (it can never latch) — did you mean identity_verified?
```

## The Flow Studio

`cargo run -p gemini-adk-web-rs`, then open **http://localhost:25125/flows**.

The Studio is a drag-and-drop editor over exactly this document:

- **Canvas** — steps are nodes; drag from a node's output port onto another
  node to add an `after` edge; click an edge to remove it. Auto-layout
  arranges the DAG by topological depth.
- **Step inspector** — posture, ground template, allow/deny lists, terminal
  flag, and a structured guard builder covering every atom (including nested
  `all`/`any`/`not`).
- **Flow tab** — cross-cutting constraints (`once`, `before`, `never…until`,
  `require`), ambient tools, confirm tools.
- **App tab** — instruction, greeting, modality, and the mock tool editor.
- **JSON tab** — the live document. Two-way: edit and Apply, import a file,
  copy, or download. The JSON you export is exactly what
  `serde_json::from_value::<FlowAppSpec>` (or a bare `Flow`) loads.
- **Validate** — `POST /api/flows/validate` runs the real compiler
  (`Flow::compile_with_tools`) server-side and reports every diagnostic:
  unknown tools, unreachable steps, unguarded commit tools, ordering cycles,
  unwritten guard keys (with did-you-mean suggestions), plus advisory
  warnings.
- **Tests** — replays the spec's embedded test suite offline through the real
  flow monitor (`POST /api/flows/test`) and reports each script's result. No
  API key involved.
- **Run** — starts a live session in the `flow-studio` app
  (`/ws/flow-studio`), passing the spec in the Start message's `config`
  field. The session is configured by `SessionSpec::apply` (governance,
  tools, extraction, phases, watchers), and after every turn, tool call, and
  extraction the server pushes a `flowStatus` snapshot — active steps light
  up blue and done steps green on your canvas while you chat, and the Run tab
  lists admitted tools, blocked tools (with reasons, from `why_blocked()`),
  unmet requirements, and each active step's **guard truth tree**: exactly
  which atom it is waiting on.
- **Live posture editing** — while a session runs, committing a posture or
  ground edit in the step inspector sends `updateFlowPostures`; the monitor
  re-projects postures at every turn boundary, so the change steers the very
  next turn.

Two examples ship with the Studio (toolbar → Examples): a governed
debt-collection call and a restaurant booking flow.

### Validate endpoint

```
POST /api/flows/validate
Body: a FlowAppSpec, or a bare flow ({"steps": [...]})
```

Response:

```json
{
  "valid": true,
  "errors": [],
  "warnings": ["step 'x' has no posture — …"],
  "mermaid": "flowchart TD\n …",
  "tools": ["charge_card", "verify_identity"],
  "steps": 5
}
```

## See also

- [Governed Flows](./flow.md) — the flow model, enforcement semantics, verbs
- [Per-Tool Policies](./tool-policies.md) — commit tools and confirmation
