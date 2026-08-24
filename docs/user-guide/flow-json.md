# Flows as JSON (and the Flow Studio)

Because every [`Guard`] atom is a named, parameterized predicate, a `Flow` is
fully serializable — the same DAG you build with `Flow::new()…build()` in Rust
round-trips through JSON. This page documents that JSON format, the
**flow app** document that turns a flow into a runnable application without
writing code, and the **Flow Studio** — the drag-and-drop editor shipped with
`gemini-adk-web-rs` that reads and writes these documents.

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

## Flow app documents — a runnable application as one JSON file

A **flow app spec** (`FlowAppSpec` in `gemini-adk-server-rs`) wraps a flow with
everything a governed Live session needs — instruction, greeting, modality, and
a set of *declarative mock tools*:

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
  "flow": { "steps": [ … ], "constraints": [ … ] }
}
```

Each mock tool returns its canned `response` (default `{"ok": true}`) and
writes its `set_state` entries into the session `State` when called. Because
flow guards read the same state, `is_true`/`captured` conditions latch exactly
as they would against real tool implementations — the whole conversation can be
modeled, enforced, and demoed before a single real tool exists. Swap the mock
dispatcher for a real `ToolDispatcher` later; the flow JSON does not change.

App-level fields: `name`, `description`, `instruction`, `greeting` (model
speaks first when set), `modality` (`"text"` | `"audio"`), `voice`, `tools`,
`flow`.

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
  plus advisory warnings.
- **Run** — starts a live session in the `flow-studio` app
  (`/ws/flow-studio`), passing the spec in the Start message's `config`
  field. The session is governed by the flow (`.govern(..)`), the mock tools
  are wired to shared state, and after every turn and tool call the server
  pushes a `flowStatus` snapshot — active steps light up blue and done steps
  green on your canvas while you chat, and the Run tab lists admitted tools,
  blocked tools (with reasons, from `why_blocked()`), and unmet requirements.

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
