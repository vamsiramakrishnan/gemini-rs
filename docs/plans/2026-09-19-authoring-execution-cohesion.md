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
| `ConversationSpec` | The authoring model above `SessionSpec`. It compiles to a `FlowStack` plus extractors. It is not a second application document. |
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
- `Policy::commit(..).idempotency_key(..)` and `compensate_with(..)` are
  declarations the application enforces. Their docs must say so, and a
  runtime that enforces them is phase 3 work, not implied by the names.

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
