# Durable Memory (`gemini-memory-rs`)

`gemini-memory-rs` is a contextual memory engine for Gemini Live voice
sessions — an independent crate that composes with the L0/L1/L2 stack rather
than sitting inside it.

The organising principle: **context is prepared asynchronously and consumed
synchronously**. No model call, no search, and no repository write ever sits
between the model asking for memory and the memory arriving — by the time a
`recall_context` tool call lands, the answer is already in state.

```text
user speech ─► input transcription ─► retrieval-state extraction
                                           │
                                           ▼
                                   local BM25 search
                                           │
                                           ▼
                             immutable prepared snapshot ─► Gemini

final transcript ─► observation extraction ─► session ledger
                                                   │
                       ┌───────────────────────────┤
                       ▼                           ▼
                session overlay          post-session reconciliation
                (usable now)                       │
                                                   ▼
                                         canonical OKF markdown
```

## Getting started

```rust,ignore
use gemini_memory_rs::prelude::*;

let engine = MemoryEngine::in_memory(UserId::new("usr_72ab"));
engine.compile_index().await?;

let session = engine.begin_session(SessionId::new("ses_01"));

session.begin_turn(TurnId(1));
session.observe_final_transcript(TurnId(1), "I am pescatarian").await?;
session.on_turn_complete(TurnId(1)).await?;

session.begin_turn(TurnId(2));
let snapshot = session
    .prepare(TurnId(2), "what do you remember about my dietary preferences")
    .await?;
for fact in snapshot.facts.iter() {
    println!("{}", fact.presented_statement());
}

// Seals the conversation and reconciles its evidence into canonical memory.
let report = session.finish().await?;
```

Run the whole lifecycle offline — two conversations, a correction, and the
Markdown they leave behind:

```bash
cargo run -p gemini-memory-rs --example memory_pipeline
```

## Wiring into a Live session

Memory has exactly two touchpoints on a live conversation, and both are
mechanisms the runtime already owns: the **extraction pipeline** (a
`MemoryTurnExtractor` runs at each turn boundary and promotes its fields into
governed `State`) and the **tool dispatcher** (which serves `recall_context`
and `manage_memory`). There is deliberately no third mechanism — an earlier
revision carried its own channel and control loop, and every duplicated
mechanism is a second place for turn bookkeeping to drift. The whole
installation is one extension trait:

```rust,ignore
use gemini_memory_rs::runtime::{LiveMemoryExt, MemorySlot};

let session = Arc::new(engine.begin_session(SessionId::new("ses_01")));

Live::builder()
    .with_memory_slots(
        session,
        [
            MemorySlot::new("dietary_identity", "user:diet"),
            MemorySlot::new("venue_preference", "user:venue"),
        ],
    )
    // Satisfied from memory for a returning user, so they are not
    // asked again for something they already told us.
    .phase("plan_dinner")
        .needs(&["user:diet", "user:venue"])
        .done()
```

`with_memory(session)` is the slotless form. A slot filled from memory
satisfies `phase.needs(..)` and `Flow` guards exactly as one filled by the
user would — which is the point: the application asks for what it needs and
does not care whether the answer arrived this minute or last month.
`memory_tools(session)` exposes the two tools as a composite for manual
`with_tools` wiring.

## Declarative binding: `spec.memory`

A [`SessionSpec`](./user-guide/flow-json.md) can declare memory instead of
wiring it by hand. Slots project remembered facts into governed state keys
where flow guards and `needs` read them, and the `remember` effect (available
to phases, watchers, and temporal patterns) routes through the engine's
explicit-command path:

```json
{
  "memory": {
    "slots": [
      { "predicate": "dietary_preference", "to": "memory:diet" }
    ]
  },
  "flow": {
    "steps": [
      { "id": "suggest", "done": { "captured": ["memory:diet"] } }
    ]
  }
}
```

The seam is a trait (`spec::MemoryBinding` in `SpecResources`), implemented by
`gemini_memory_rs::runtime::SessionMemoryBinding` — so L2 stays free of the
engine, and a spec-authored `remember` is indistinguishable downstream from a
user-asked one. The Flow Studio provisions an in-process engine per run.

## Design commitments

- **Canonical memory is human-readable Markdown.** Indexes, caches and
  snapshots are derived and disposable; the corpus is the only authoritative
  artefact — read it, diff it, hand-edit it, delete it.
- **Partial transcripts are hypotheses.** They may prefetch context; they may
  never become evidence.
- **The model proposes, deterministic code commits.** Extraction may be a
  model call. Admission, TTLs, deletion, promotion and privacy are not.
- **Explicit statements outrank inference**, however often the inference
  recurs.
- **One immutable snapshot per turn**, so what the model remembers cannot
  change halfway through a sentence.
- **Memory failure is never fatal.** Every degradation path ends in "nothing
  found", not in a failed turn.

## Module layout

| Module | Responsibility |
|--------|----------------|
| `core` | Domain vocabulary, deterministic policy, event log |
| `okf` | Canonical Markdown records, the repository, transactional commit |
| `bm25` | Fielded lexical index, ranking, search explanation |
| `transcript` | Partial/final accumulation, debouncing, generation guard |
| `retrieval` | Retrieval plans, fusion, budgeted context assembly |
| `ingestion` | Observation extraction, candidate ledger, session overlay |
| `reconcile` | Consolidation, conflict resolution, promotion, commit |
| `runtime` | Live wiring: `LiveMemoryExt`, turn extractor, tool surface, spec binding |
| `evals` | Fixture-driven quality harness |

## Extending it

Three seams take a model without touching anything else:

- `RetrievalPlanExtractor` — a structured-output call that refines the
  rule-based plan, wrapped in a deadline that falls back to the rules.
- `MemoryObservationExtractor` — the real evidence extractor; the bundled
  rule-based one is a floor, not a ceiling.
- `SemanticFallback` — paraphrase-tolerant retrieval, reached only when
  lexical search finds too little.

The repository and event log are traits; the bundled implementations are
in-process. Swap them for durable backends without changing a caller.
