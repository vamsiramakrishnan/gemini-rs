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

### Constraints

| JSON | Meaning |
|---|---|
| `{ "once": "tool" }` | The tool may complete at most once |
| `{ "before": ["a", "b"] }` | Step `a` must be done before `b` starts |
| `{ "never_until": { "tool": t, "until": g } }` | Forbid `t` until `g` holds |
| `{ "require": ["a", "b"] }` | Steps required for flow completion |

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
  "phases": [ … ],
  "watch": [ … ],
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

### Phases and watchers, over the same guard vocabulary

Phase transitions and watcher conditions reuse the closed `Guard` atoms;
handlers are closed `EffectSpec`s (`set` state, inject `context`):

```json
"phases": [
  { "name": "greeting", "instruction": "Welcome the caller.",
    "transitions": [ { "to": "main", "when": { "is_true": "greeted" } } ],
    "on_enter": [ { "set": { "entered": true } } ] },
  { "name": "main", "tools": ["search"], "needs": ["topic"], "terminal": false }
],
"initial_phase": "greeting",
"watch": [
  { "key": "app:score", "condition": { "crossed_above": 0.9 },
    "set": { "alert": true } }
]
```

Phase guards evaluate against state alone (there is no flow marking at a
phase boundary), so `called_ok`/`done` atoms there are a validation *error* —
latch a state key instead.

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
