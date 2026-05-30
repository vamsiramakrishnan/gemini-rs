# RFC: The Extraction Kit — `Extract` records with pluggable sources

Status: proposed · Author: library team · 2026-05-30

> Refined by the [Governed Agents synthesis](./2026-05-30-governed-agents-synthesis.md):
> field "sources" are **Resolvers** (`Recognizer`/`Llm`/`Fetch`/`Mcp`/`Agent`/`NativeFn`),
> `Guard` is the shared predicate, and `Mode` (`Call`/`Dispatch`/`Background`)
> applies to async resolvers.

## Motivation

Structured state is the spine of every governed agent: `Flow` guards read it,
repair fires from its gaps, the model is grounded on it, and downstream agents
consume it. Today there is exactly one way to produce it — `extract_turns`, an
out-of-band LLM over the transcript. That is one source among several, it is the
slowest and costliest, and it leaves two large classes of structured data
ungoverned:

1. **Deterministic facts** the transcript already contains (amounts, dates, IDs,
   menu items, yes/no) — extractable on CPU in microseconds, no model.
2. **Dynamic-system facts** (a balance, availability, a CRM record) — today
   fetched as an *opaque tool round-trip* whose JSON goes to the model and never
   becomes typed, validated, cached State.

This RFC makes `Extract` **the single declarative way** to produce structured
state, with the *source* of each field pluggable — transcript (deterministic or
OOB-LLM), the model (native function call), or a live system (tool/MCP fetch) —
all fused into one governed record, grounding the model and optionally
triggering downstream agents. It is accelerator-free: deterministic sources are
CPU; the LLM and fetch sources are remote calls.

## The one-line model

> An **`Extract` record** is a typed struct whose fields are *resolved from the
> cheapest reliable source*, fused into governed `State` with provenance, used to
> ground the model and (optionally) to trigger downstream agents.

## Closed vocabulary (cemented)

### Nouns
`Record` · `Field` · `Source` · `Recognizer` · `Cascade` · `Effect` · `Trigger`
· `Provenance`.

### Verbs
- Record: `record::<T>()` · `field(name, Source)` · `source_default(Cascade)`
  · `require([fields])` · `trigger(Trigger)`
- Sources: `recognizer(..)` · `llm(model, prompt)` · `native_capture(name)`
  · `fetch(tool, args)` · `mcp(server, tool, args)` · with `.ttl(d)` /
  `.prefetch_on(step)` / `.validate(fn)`
- Effects: `on_complete(Effect)` · `dispatch(name, agent)` · `ground(template)`

### 🚫 Ban-list at the `Extract` surface
`extractor`, `prompt`, `tool`, `watcher`, `phase`, `field-promotion`. These are
*lowering details* (an `Extract` author never types them). A "fetch source" *uses*
a tool but is not the tool concept; the LLM source *uses* a prompt but the author
declares a `Source`, not a prompt.

## Sources (the closed set)

| Source | Channel | When | Cost | Serializable |
|---|---|---|---|---|
| **Recognizer** | CPU on transcript | every turn (cheap) | µs | yes |
| **Llm** | remote flash on transcript window | on `Trigger`, gated by `should_extract` | ~100ms, parallel | no (prompt is code) |
| **NativeFn** | the model calls a generated capture-tool | model-driven | ~0 (rides the turn) | name only |
| **Fetch** | a registered tool, args bound from State | reactive: args available ∧ cache stale | network | yes |
| **Mcp** | an MCP toolset | same as Fetch | network | yes |

## Recognizer kit (deterministic transcript atoms — CPU, no accelerator)

| Recognizer | Backed by | Notes |
|---|---|---|
| `money` | `rust_decimal` + regex | typed `Money`, currency-aware |
| `datetime` / `duration` | `chrono` + a duckling-style normalizer | "next Tue 3pm", "twelve hundred" |
| `integer` / `ordinal` / `percent` | regex + word-number map | `near = [...]` keyword anchoring |
| `one_of` / `enum_words` | `fst` / `aho-corasick` gazetteer | catalogs/menus, millions of entries |
| `regex` (+ `validate`) | `regex` | Luhn/checksum/format validation |
| `fuzzy` / `phonetic` | `strsim` (Jaro-Winkler) / `rphonetic` | resolve ASR-mangled names to a roster |
| `yes_no` | lexicon | confirmations |

Each recognizer yields `Option<(typed value, confidence)>`; `validate` rejects or
flags low-confidence values (never hallucinated).

## Formal model — resolution

A record `R` has fields `f₁..fₙ`; each field has a **Cascade** `[s₁, s₂, …]` of
sources in priority order, plus an optional `validate`.

- Each source evaluates to `Option<Resolved{ value, confidence, provenance }>`.
- **Fusion:** the highest-priority non-`None` resolution wins; an existing value
  is kept (`KeepKnown`) unless a later resolution has strictly higher confidence.
  Provenance (`source`, `span`/`tool`, `confidence`) is recorded to `state_meta`.
- **Timing / demand:**
  - *Recognizers* run **every turn** (idempotent, cheap) on the transcript.
  - *Llm* runs on its `Trigger`, **skipped** (`should_extract = false`) when the
    fields it covers are already filled at high confidence.
  - *Fetch/Mcp* are **reactive**: invoked when their `args` (bound from State) are
    all available and the cached value is absent or stale (per `ttl`). Result is
    mapped → captured → validated.
  - *NativeFn* is **model-driven**: fires when the model calls the generated
    capture-tool (platform-gated; see D8).
- **Completion:** `R` is complete when its `require`d fields are present. This is
  exactly what a `Flow` step reads via `done(captured([...]))`.

## Effects (on field/record resolution)

- **Set State** (always): namespaced `<record>:<field>` plus promoted keys.
- **`ground(template)`** → a curated projection ("balance: $1,250, verified")
  injected as turn-boundary steering — *the model sees governed State, never the
  raw API JSON* (anti-hallucination, privacy).
- **`dispatch(name, agent)`** → `BackgroundAgentDispatcher` with the typed record
  + shared `State` — extraction becomes a **router**, not just a recorder.
- Emit `LiveEvent` (`Extraction` / `Resolution` / `Dispatch`) on the trace.

## Authoring surface

```rust
#[derive(Extract)]
struct Account {
    #[extract(regex = ACCT, validate = "luhn")]            // transcript-deterministic
    account_id: Option<String>,
    #[extract(source = "llm")]                             // OOB fallback
    reason: Option<String>,
    #[resolve(fetch = "get_balance", args = { account_id }, ttl = "60s")]
    balance: Option<Money>,                                // a live system, when account_id is known
    #[resolve(mcp = "crm/lookup_customer", args = { account_id })]
    customer: Option<Customer>,                            // MCP
}

Live::builder()
    .extract(Extract::record::<Account>()
        .native_capture("record_account")                 // also let the model volunteer it (D2)
        .ground("Verified account {account_id}; balance {balance}.")
        .on_complete(dispatch("risk_check", risk_agent)))  // downstream trigger (D4)
    .govern(flow)                                          // Flow reads done(captured(["account_id"]))
    .connect_from_env().await?;
```

`#[derive(Extract)]` mirrors `#[tool]`. Deterministic + fetch + mcp sources are
serializable (data-driven scripts); `llm`/`native`/`custom` are code-only.

## Interplay with Flow & dynamic systems (the "better")

- **Fetch-as-source:** dynamic data becomes typed, validated, cached State — not
  an opaque round-trip. `Flow` guards can read it; the model is grounded on it.
- **Pull + Push:** *pull* for declared data dependencies (args bound from State,
  resolved when needed — the GraphQL-resolver / dataloader / Salsa-incremental
  pattern); *push* via `native_capture` for ad-hoc, model-initiated fetches.
- **Prefetch on Flow-step entry** (`.prefetch_on("verify")`) — resolve in
  parallel so the next step has data ready; no mid-conversation tool stall.
- **Reads vs commits** reuse `Flow`: fetches are repeatable/cacheable; `commit`
  tools stay `once`/gated.
- **TTL freshness**, **fan-out/join** (v2), **MCP first-class**.

## Lowering / anti-leakage

`Extract` adds **no new runtime** — each `Source` compiles onto existing machinery:

| Source / effect | Lowers to |
|---|---|
| Recognizer / Llm | a `TurnExtractor` (Llm = today's `LlmExtractor`) |
| Fetch / Mcp | a reactive resolver over `ToolDispatcher` / `McpToolset` (arg-bind + `PolicyTool` TTL cache + validate) |
| NativeFn | a generated `ToolFunction` capture-tool on the dispatcher (`Silent` scheduling) |
| Fusion / provenance | `FieldPromotion` + `MergePolicy` + `state_meta` |
| `ground` | turn-boundary `context_buffer` steering |
| `dispatch` | `BackgroundAgentDispatcher` |

`extract_turns` becomes **sugar** for `Extract::record::<T>().source_default(llm(..))`.
`Extract` is the one surface new users learn; `LlmExtractor`/`TurnExtractor` remain
power-user escape hatches.

## Decisions (resolved — win-win)

| # | Decision | Win-win |
|---|---|---|
| D1 | Source per field | Pluggable Cascade; default `deterministic → llm` |
| D2 | Native-audio fn-calling | Opt-in `native_capture` source (generated capture-tool); OOB/deterministic default for passive observation |
| D3 | "Single way" | `Extract` canonical; `extract_turns` = sugar; lowers onto `TurnExtractor` — no new runtime |
| D4 | Trigger downstream agents | `on_complete(dispatch(..))` via `BackgroundAgentDispatcher`; record + State to the sub-agent |
| D5 | Authoring | `#[derive(Extract)]` + builder; serializable where deterministic/fetch/mcp |
| D6 | Fusion | `(value, confidence, provenance)`; priority by cascade order; `KeepKnown` unless higher confidence |
| D7 | Timing | `ExtractionTrigger` (+ `ModelDriven` for native, `Reactive` for fetch/mcp) |
| D8 | Platform gating | deterministic+OOB everywhere; native auto-gated by `supports_async_tools()`/caps → OOB fallback; fetch/mcp gated by availability |
| I1 | Push vs pull (systems) | Both: pull for declared deps, push for native |
| I2 | When to resolve | Lazy/reactive by default; `prefetch_on(step)` for known needs |
| I3 | What the model sees | Curated `ground` projection, never raw JSON |
| I4 | Freshness | Per-source `ttl`; reuse `PolicyTool` cache |

## Worked use cases

- **Booking:** `datetime` recognizer → `fetch("check_availability", {datetime})`
  → `ground("{datetime} is open")` → `native_capture("book")` doubling as the
  action. Flow gates `book` until a valid parsed `datetime` exists.
- **Debt collection:** `account_id` (regex+luhn) → `balance` (fetch, ttl 60s) →
  `customer` (mcp) → `ground`; PTP via `money`+`datetime` recognizers feeding the
  Flow `done(captured(["ptp_amount","ptp_date"]))`.
- **Call screening:** caller name (`fuzzy`→roster gazetteer) + spam
  (`aho-corasick`) + `on_complete(dispatch("notify", screen_agent))`; fully
  on-device (no transcript leaves the process).

## Build plan (phasing)

- **v1:** recognizer kit; `RecordExtractor: TurnExtractor`; `#[derive(Extract)]`;
  `Cascade` + fusion/provenance; `fetch`/`mcp` sources (reactive on
  arg-availability + `ttl` cache + validate); `native_capture`; `on_complete`
  dispatch; `ground`; subsume `extract_turns`. Cookbook (booking) + `extraction.md`.
- **v2:** true demand-driven (lazy) resolution; Flow-driven `prefetch`; parallel
  fan-out/join; the full reactive dependency graph; recognizer expansion.

## Open questions

1. v1 timing — start **trigger+reactive** (simple) or go straight to
   **demand-driven** (lazy resolution when a guard reads a field)?
2. Default cascade priority per recognizer type vs. fetch (system facts should
   outrank transcript guesses for the *same* field — make priority per-field).
3. `native_capture` default scheduling — `Silent` (non-disruptive) vs `WhenIdle`.
