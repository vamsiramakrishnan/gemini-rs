# gemini-memory-rs

A contextual memory engine for Gemini Live voice sessions.

The organising principle is that **context is prepared asynchronously and
consumed synchronously**. No model call, no search, and no repository write ever
sits between the model asking for memory and the memory arriving — by the time a
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

```rust
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

```rust
use gemini_memory_rs::runtime::{channel, run_memory_control_loop, tools};

let session = Arc::new(engine.begin_session(SessionId::new("ses_01")));
let (sender, receiver) = channel(256);
tokio::spawn(run_memory_control_loop(receiver, session.clone(), state.clone()));

Live::builder()
    .on_input_transcript({
        let sender = sender.clone();
        move |text, is_final| { sender.input_transcript(turn, text, is_final); }
    })
    .on_vad_start({
        let sender = sender.clone();
        move || { sender.user_activity_started(turn); }
    })
    .with_tools(
        tools::recall_context_tool(session.clone())
            | tools::manage_memory_tool(session.clone()),
    )
```

Every method on the sender is a bounded `try_send` and nothing else. Fast-lane
callbacks have a sub-millisecond budget; a dropped speculative event costs a
little retrieval quality, a blocked audio callback costs the conversation.

## Layout

| Module | Responsibility |
|--------|----------------|
| `core` | Domain vocabulary, deterministic policy, event log |
| `okf` | Canonical Markdown records, the repository, transactional commit |
| `bm25` | Fielded lexical index, ranking, search explanation |
| `transcript` | Partial/final accumulation, debouncing, generation guard |
| `retrieval` | Retrieval plans, fusion, budgeted context assembly |
| `ingestion` | Observation extraction, candidate ledger, session overlay |
| `reconcile` | Consolidation, conflict resolution, promotion, commit |
| `runtime` | Live wiring: state keys, control loop, tool surface |
| `evals` | Fixture-driven quality harness |

## Design commitments

- **Canonical memory is human-readable Markdown.** Indexes, caches and snapshots
  are derived and disposable; the corpus is the only authoritative artefact, and
  it can be read, diffed, hand-edited and deleted.
- **Partial transcripts are hypotheses.** They may prefetch context; they may
  never become evidence.
- **The model proposes, deterministic code commits.** Extraction may be a model
  call. Admission, TTLs, deletion, promotion and privacy are not.
- **Explicit statements outrank inference**, however often the inference recurs.
- **One immutable snapshot per turn**, so what B remembers cannot change halfway
  through a sentence.
- **Memory failure is never fatal.** Every degradation path ends in "nothing
  found", not in a failed turn.

## Extending it

Three seams take a model without touching anything else:

- `RetrievalPlanExtractor` — a structured-output call that refines the rule-based
  plan. Wrapped in a deadline that falls back to the rules.
- `MemoryObservationExtractor` — the real evidence extractor. The bundled
  rule-based one is a floor, not a ceiling.
- `SemanticFallback` — paraphrase-tolerant retrieval, reached only when lexical
  search finds too little.

The repository and event log are traits; the bundled implementations are
in-process. Swap them for durable backends without changing a caller.

## License

MIT
