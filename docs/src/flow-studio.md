# The Flow Studio

Flow Studio is a visual editor for governed sessions, served by the ADK Web
UI:

```bash
cargo run -p gemini-adk-web-rs
# → http://localhost:25125/flows
```

It is a design surface over [`SessionSpec`](./user-guide/flow-json.md) —
everything you do on the canvas *is* the JSON document, and everything in the
document is the fluent Rust program. The Studio invents no semantics of its
own: the same compiler validates, the same `FlowMonitor` replays tests, the
same codegen prints the program.

<p align="center"><img src="./assets/studio/flow-studio.gif" alt="Flow Studio click-through: load a cookbook, drag nodes, validate, run the embedded tests, scrub a simulated session on the canvas, read the generated Rust" width="900"></p>

## The canvas

Steps are nodes; `after` dependencies are edges. Unconditional edges are
solid; **conditional edges** (`{step, when}`) render dashed with their guard
as the label — the dashed `is_true(is_emergency)` edge below is how the
clinic-intake cookbook branches to `close` on an emergency, merging with an
`any` join. Terminal steps render as stadium outlines. Drag nodes to arrange
them, or let **Auto layout** arrange the DAG; **Validate** compiles the flow
server-side and reports diagnostics in the docked console.

<p align="center"><img src="./assets/studio/canvas.png" alt="The clinic-intake DAG: identify, triage, schedule, and a terminal close step, with a dashed conditional emergency edge labeled is_true(is_emergency), a green valid badge, and compile diagnostics" width="900"></p>

## Structured editors

Selecting a step opens its editor: posture (the instruction imposed while the
step is active), ground template (a state-interpolated fact line), tool
allow/deny lists, terminal flag, gate, incoming edges with per-edge
conditions, and the completion guard built from the closed `Guard`
vocabulary — `is_true`, `captured`, `called_ok`, `resolved`, and friends,
composable with any/all. Guard editors autocomplete state keys from the
spec's declared state dictionary.

<p align="center"><img src="./assets/studio/step-editor.png" alt="Step editor: posture, ground template, allowed tools, and an any-of completion guard over symptoms_recorded and escalated" width="900"></p>

The **Flow** pane edits flow-level constraints (`once`, `never … until`,
`require`, `reset` loops) and temporal patterns; the **App** pane edits
instruction, tools (mock/HTTP/MCP), extraction, phases, watchers, the state
dictionary, computed variables, durable memory slots, and runtime tuning.
The **JSON** pane is the document itself, two-way: edit either side.

## Tests and Preview — offline, no API key

Every bundled cookbook embeds conformance tests (`user` / `tool` / `set`
events with `expect` assertions). **Tests** replays them through the real
`FlowMonitor` and reports each script; **Preview** scrubs one test
event-by-event while done and active steps light up on the canvas:

<p align="center"><img src="./assets/studio/preview.png" alt="Preview mode scrubbing event 3 of 6: identify and triage marked done, schedule active, the scrubber showing the current tool event" width="900"></p>

The same replay engine runs in CI: a unit test walks the gallery manifest and
requires every cookbook to compile and pass its own embedded tests.

## Code — the program the document is

The **Code** tab shows the generated `main.rs` and `Cargo.toml` the document
lowers to. Because L2 never invents semantics, generation is
pretty-printing — every spec field becomes the corresponding builder call.
Generated cookbook apps compile as standalone crates under
`RUSTFLAGS="-D warnings"`.

<p align="center"><img src="./assets/studio/code.png" alt="Code tab: the generated main.rs for clinic-intake — tool registration, the flow chain, and the builder calls the JSON document lowers to" width="900"></p>

## Run — a live session against the canvas

**Run** connects a real session governed by the flow on screen. Active steps
light up as the conversation progresses; the Run pane shows admitted and
blocked tools, unmet requirements, and each active step's **guard truth
tree** — exactly which atom a stuck step is waiting on. Posture and ground
edits made while connected apply on the next turn boundary
(`LiveHandle::update_step_posture`).

## The cookbook gallery

Six industry scenarios ship in the **Examples** menu, each a complete
`SessionSpec` with mock tools, governance constraints, and embedded tests:

| Cookbook | Industry | Highlights |
|----------|----------|------------|
| Debt collection | Financial services | compliance gates, `once` payment, declarative extraction |
| Patient intake | Healthcare | conditional emergency edge + `any` join into close |
| Line support | Telecom | `reset` loop — a re-test reopens the diagnostic step |
| Call screening | Front desk | spam-verdict-gated transfer |
| Returns desk | E-commerce | eligibility-gated single refund |
| Table booking | Hospitality | ambient memory, computed state |

The manifest lives at
`apps/gemini-adk-web-rs/static/examples/flows/index.json`; the JSON documents
next to it double as starting points for your own flows — import, edit,
re-export.
