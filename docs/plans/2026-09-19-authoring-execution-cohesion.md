# Decision record: authoring, execution and deployment cohesion

Status: accepted for phases 1–2, proposed for phases 3–5 · 2026-09-19 ·
follows the statecharts/digressions RFC (`2026-06-07-statecharts-digressions-rfc.md`)

## The product promise

A definition behaves the same in the simulator, in Studio and in a live
session, or compile/attach rejects it. Every item below is measured against
that sentence. Features that do not serve it wait.

## Phase 1 — one execution model (done in this change)

**Finding.** `Live::converse()` attached a compiled conversation's main flow
and extractors, and nothing else. The simulator drove a `FlowStack` with the
conversation's digressions and repair policies. The authoring crate owned the
stack; the runtime owned only the monitor. A scenario could pass in
Conversation CI and its digression never fire on a call.

**Decision.** The runtime owns suspension and resume. `FlowStack` moved to
`gemini_adk_rs::flow`. The Live control plane holds a `SharedFlowStack` and
nothing else; a bare `govern`/`observe` is a stack with no digressions, and
`converse` installs the overlays and repair policies on it. The authoring
layer lowers into runtime types (`Overlay`, `Resume`, `RepairPolicy`) and no
longer implements them.

**Invariant, as a test.** `live_and_simulator_drive_the_same_stack` builds the
installed stack the way connect does and the simulator's stack the way `Sim`
does, from one compiled conversation, and asserts identical governance every
turn. A control-plane test drives a digression through the real turn path.
These are the gate for any future change to either side.

## Phase 2 — one vocabulary (started in this change)

| Term | Decision |
|---|---|
| `SessionSpec` | The application document. `FlowAppSpec` and `MockToolSpec` are deprecated aliases in the server crate; they parse the same JSON and leave every example and guide. |
| `ConversationSpec` | The optional conversation behavior inside `SessionSpec`. It compiles to a `FlowStack` plus extractors. It is not a second application document. |
| stage / step / phase | Stage is authored; step is compiled; phase is the separate `PhaseMachine` mode. The glossary says so. |
| `instruction()` | The stage's guidance to the model. `say()` stays as an alias because it never meant verbatim speech, and the name should stop implying it. |
| digression / overlay | One concept. "Digression" is the public word; `overlay` remains the spec field and builder verb until a spec migration is scheduled. |
| `Call` / `Dispatch` / `Background` | Unchanged for now; documentation must describe awaiting and result delivery separately rather than lean on the names. |

Still open in this phase: shrink what the prelude re-exports in introductory
examples, and rename `overlay` in the spec behind a serde alias.

## Phase 3 — task activation and state transfer (proposed)

Selecting the next flow is half the problem. The runtime needs explicit
answers when the caller switches task while a tool is in flight:

- Each task activation gets an identity and a state scope. Tool calls and
  results carry the activation they belong to.
- Late results need an acceptance rule: cancel, discard or accept. A stale
  balance lookup must not write into a stolen-card task by default.
- Confirmations bind to the action arguments they authorized and expire on
  task switch.
- `Resume::Restart` restarts the monitor, not the business task. That is now
  documented; a separate "restart task" policy that clears scoped state is
  the phase 3 deliverable.
- Commit tools already enforce keyed deduplication and configured failure
  compensation in L1 (`tool::CommitGuard`). Phase 3 must bind that evidence to
  task ownership and define its lifetime across task changes. Cancelling
  speech, cancelling a task, and compensating an external action are separate
  operations.

## Cohesion gate before phases 3–5 (2026-10-01)

The SDK and Studio must agree on ownership before reusable skills add new
lifecycles. The following table is the contract for that work. Skill and task
rows are proposed; this change does not introduce a scheduler or routing API.

| Object | Owner and lifetime | Authoring and UI contract |
|---|---|---|
| Provider connection | L0, one transport connection with its negotiated capabilities | Carries audio, text, and tool messages. Business skills and task selection stay above this layer. |
| Live session | L1, the ongoing interaction with the caller | L2 configures it. Studio keeps its connection alive across dock navigation and stops it explicitly. |
| `SessionSpec` | L2, the versioned application source document | JSON, forms, and canvas edit the same schema. A browser edit revision is distinct from the connected session's source revision. |
| `ConversationSpec` / `Flow` | L2 conversation authoring lowers into L1 flow primitives | Optional governed behavior. A simple instruction-and-tools agent does not need a workflow. |
| `FlowStack` | L1, main flow plus nested digressions, marking, and repair policy | Live execution and embedded test replay drive this same object. A digression closing does not mean the conversation is complete. |
| `FlowSnapshot` | L1, an observation of one stack | Live status, replay, and handoff obtain progress from the stack. The browser renders the result; it does not derive completion from empty requirements. This is not a restoration checkpoint. |
| Skill definition (proposed) | L2 authors a reusable, versioned capability; L1 receives its compiled behavior | Start with name, description, instructions, typed inputs/outputs, and tools. A conversation is optional. Registry metadata refers to the definition rather than copying it into another catalog. |
| Task activation (proposed) | L1, one invocation of a pinned skill definition | Has its own identity, input, local state, status, and result. Reusing a skill creates another activation, not another definition. Studio must show activation identity and lifecycle. |
| Tool operation and approval (proposed activation binding) | L1, owned by the initiating task | Arguments, approvals, cancellation, retries, and late results retain that owner. A result cannot mutate or speak for a different task after a switch. |
| Session state / task locals / durable memory | L1 shared session facts and task state; independent memory storage | Transfer selected facts explicitly. Task restart does not silently erase shared facts; flow restart does not imply a new business task. |
| Validation, replay, generated project | L2 results for a particular source revision | Edits, loads, undo, and redo invalidate old results. Delayed responses cannot replace results for a newer revision. Live and replay observations have separate owners. |

**Implementation choice.** Use a runtime-owned snapshot as the foundation,
then give the UI explicit connection and revision ownership. A UI-only fix
would keep replay's missing digressions and inferred completion. A new skill
router would leave those disagreements in place. No new execution engine or
L1 dependency on L2 is needed.

**Current interfaces.** `FlowStack::snapshot(&State)` and
`LiveHandle::flow_snapshot()` return `FlowSnapshot`. `ServerMessage::FlowStatus`
carries that type directly. `SimSnapshot` embeds it as `status` in Rust and
flattens it in JSON, preserving the browser payload shape. Completed steps and
guard explanations refer to the same active layer; `complete` refers to the
whole stack, while `terminated` distinguishes a terminating digression.
Stack fields are observed under one stack lock. Concurrent `State` writes are
not part of an atomic snapshot.

Conversation stage IDs are unique across the main flow and digressions;
digression names are unique too. Compilation checks this after lowering safety
policies, because resolver routing, timing rules, and Studio node identity all
use these names. Reusable skill activations will need a distinct runtime
identity rather than relaxing these authoring checks.

**Replay boundary.** Embedded tests and Step through use the compiled
conversation stack, including digressions and repair. `user` advances a turn;
`set` changes state and re-latches guards without inventing a turn; `tool`
checks admission and applies declared mock effects. These tests do not prove
model extraction, phase callbacks, external services, or audio behavior.
`validate_for_replay()` checks the original document, including conflicting
bindings, while omitting the HTTP transport feature requirement. Tests and
Step through use that same validation; sanitizing bindings must not hide an
invalid source document.

**Review gates.** Exercise nested digression resume and termination, repair
turn accounting, denied tools leaving state untouched, and the shared Rust/UI
status fixture. In the browser, start a controlled session, change dock tabs,
edit during a pending result, and verify that the session survives while
outdated results disappear. Keep live provider checks separate.

**Next feature slice.** Add explicit task start/suspend/resume/cancel/complete
before automatic routing. Use one foreground voice task; background work
keeps its own operation owner. Demonstrate two activations of the same skill,
a switch while a tool is pending, and a rejected stale approval. Only then
add catalog selection and skill-library authoring to Studio. Durable resume
requires a separate checkpoint contract; `FlowSnapshot` does not provide it.

## Phase 4 — catalog routing (proposed, after phase 3)

`AgentRegistry`, `RouteTextAgent` and `TransferToAgentTool` stay as the small
application tools they are. Catalog routing is a separate subsystem:

1. Filter eligible flows (customer, channel, language, permissions, release).
2. Retrieve a small candidate set from compact descriptions; keep full
   definitions out of the selection prompt.
3. Return a typed decision: `Start`, `Continue`, `Clarify`, `Switch`,
   `Resume`, `NoMatch`. A similarity score alone never authorizes selection.
4. Activate at a controlled boundary: pin the flow version, bind tools, map
   permitted state, establish the activation scope from phase 3.
5. Reconsider only on explicit topic change, repeated failure or new evidence.

## Phase 5 — prove it (proposed)

- A routing benchmark over 10, 100 and 500 deliberately overlapping flows,
  measuring candidate recall, wrong-route rate, clarification quality,
  unsupported-request rejection, latency and context size. Task switching
  with in-flight tools and stale confirmations is part of the same suite.
  Catalog size and concurrent calls are benchmarked separately.
- `SessionSpec.version` becomes two fields: schema version and release
  identity.
- Deployment tests run against a resolved bundle: application revision,
  customer configuration, integration bindings and runtime compatibility.

## How the work is run

- One named owner per contract: compile-to-runtime, activation, routing.
- Naming and layering choices land as decision records here, not in chat.
- Before each release, someone who did not write the feature reads a
  definition and writes down what it will do in the simulator and live. If
  they are wrong, the release waits.
- New crates come last. Ownership and dependency direction are settled first;
  this change moved one module down a layer and created no crate.
