# Contextual memory engine

Status: **implemented (engine complete; model-backed extraction and durable
backends are seams, not implementations)** · 2026-07-26

Audience: smartglasses engineering, Gemini Live platform engineering, privacy
and security engineering.

Runtime: `crates/gemini-memory-rs`, inside the Rust `gemini-rs` server.

---

## BLUF

Memory is a server-side, local-first subsystem embedded in the Live runtime. No
LLM, vector database or remote service sits on the real-time voice path.

```text
user speech
   │
   ▼ input transcription
retrieval-state extraction        deterministic rules, then optional OOB model
   │
   ▼
local BM25 over the compiled corpus
   │
   ▼
immutable prepared snapshot ──────► available to Gemini on the next eligible turn
```

Ingestion is a separate path:

```text
final transcript
   │
   ▼ observation extraction
session candidate ledger
   ├── session overlay            usable this conversation, immediately
   ├── micro-reconciliation       every 4 turns or 90s
   └── post-session reconciliation
           │
           ▼
     canonical OKF Markdown ──► incremental index refresh
```

Consistency model:

| Scope | Guarantee |
|-------|-----------|
| Explicit corrections and commands | Immediate |
| Facts learned this conversation | Session-consistent |
| Long-term memory | Eventually consistent after reconciliation |

The decisive principle: **prepare context asynchronously, consume it
synchronously.** Gemini receives memory in milliseconds because the interpretation
and retrieval already happened while someone was speaking.

---

## What shipped

Nine modules in one crate. The design proposed eight sibling crates
(`gemini-memory-core`, `-transcript`, `-retrieval`, `-bm25`, `-ingestion`,
`-reconcile`, `-okf`, `-evals`); they ship as modules of one crate with the same
names and the same boundaries. One publishable unit, one version, one CI target —
and the module graph enforces the same layering, since nothing below `engine`
depends on anything above it.

| Module | Responsibility |
|--------|----------------|
| `core` | Domain vocabulary, deterministic policy, append-only event log |
| `okf` | Canonical Markdown, the repository, transactional commit |
| `bm25` | Fielded lexical index, ranking, search explanation |
| `transcript` | Stable-prefix accumulation, debouncing, generation guard |
| `retrieval` | Plans, fusion, budgeted context assembly |
| `ingestion` | Observation extraction, candidate ledger, session overlay |
| `reconcile` | Consolidation, conflict resolution, promotion, commit |
| `runtime` | Live wiring: the turn extractor, slot projection, the two tools |
| `engine` | The facade: `MemoryEngine` (outlives conversations), `MemorySession` |
| `evals` | Fixture-driven quality harness enforcing the acceptance thresholds |

---

## Architecture

### Runtime placement

Co-located in the Live session worker: the transcript accumulator, the retrieval
plan state, the session overlay, the warm BM25 reader, the prepared-memory cache,
the tool dispatcher, and the event producer.

Background: candidate extraction, full session reconciliation, pattern promotion,
index compilation, and quality audits.

Server-side rather than on-device, because it buys stable compute, warm indexes,
durable queues, straightforward identity enforcement, and consistent behaviour
across glasses and phone.

### Integration: memory adds no mechanism of its own

Memory rides what the Live runtime already has. It is **one `TurnExtractor` and
two tools**, and nothing else:

- the extraction pipeline already accumulates transcripts, segments them by turn
  boundary, fires extractors under a trigger policy, and promotes their fields
  into governed `State`;
- the tool dispatcher already serves function calls.

An earlier revision of this crate carried its own channel, control loop and
state keys. All of it duplicated the above, and every duplicated mechanism is a
second place for turn bookkeeping to drift. It was deleted.

The one thing memory adds is **slot projection**, and it is what makes memory
useful to an application rather than merely present. A remembered fact is
written into a governed `State` key, and from there every existing mechanism
reads it:

| Mechanism | What a filled slot does |
|-----------|-------------------------|
| `phase.needs(&["user.diet"])` | A returning user is not asked again |
| `phase.requires(&["user.diet"])` | A hard gate a memory can open |
| `Flow` guard `done(captured([...]))` | The step advances on memory alone |
| `P::with_state(&["user.diet"])` | The value appears in the phase instruction |
| watchers, repair | Read the same keys, unchanged |

```rust
Live::builder()
    .with_memory_slots(session, [
        MemorySlot::new("dietary_identity", "user.diet"),
        MemorySlot::new("venue_preference", "user.venue"),
    ])
    .phase("gather")
        .needs(&["user.diet", "user.venue"])   // skipped for a returning user
        .done()
    .phase("suggest")
        .requires(&["user.diet"])              // opened by memory
        .done()
```

Slots promote with `KeepKnown`, so what the live conversation established always
beats what memory recalls.

### Turn lifecycle

1. **Turn boundary** — the pipeline hands the extractor a transcript window.
2. **Ingest** — the finalized utterance becomes observations, the ledger and
   overlay update, and the reconciliation cadence advances.
3. **Prepare** — the *next* turn's retrieval snapshot is built while the model is
   still speaking. This is the whole latency argument.
4. **Project** — what memory knows fills the application's slots.
5. **Tool call** — served from the prepared snapshot when it covers the query,
   otherwise from a bounded local search that never touches the network.

Speculating on *partial* transcripts is deliberately not done. It would buy
first-turn latency only, and it cannot be expressed through the extraction
pipeline, which by construction sees finalized turns. Paying for a second
runtime to get it was not worth it.

### Logical versus transport sessions

A logical conversation spans several Live WebSocket connections. A reconnect is
not a session end. Sessions seal on explicit completion, on idle timeout
(3 minutes), or on an explicit application signal — never on a transport event.

---

## Key decisions

**Server-side runtime.** Stable compute, warm indexes, durable queues, identity
enforcement, observability.

**OKF Markdown as canonical memory.** Transparent, inspectable, portable,
deletable, easy to evaluate. Every index is derived from it and can be rebuilt
from scratch, which is also the recovery path after any corruption or schema
change.

**BM25 as the default engine, semantic retrieval as fallback.** A personal corpus
is hundreds to low thousands of short records; lexical search answers in
microseconds and explains itself. Semantic search is reached only when lexical
finds too little.

**Speculative preparation.** All expensive work happens while the user or the
model is speaking, so the tool-call path is a state read.

**Final transcripts only, for evidence.** Partials get revised. Evidence that can
be revised is not evidence.

**A session overlay.** Users expect a fact stated thirty seconds ago to be
available. Waiting for reconciliation would break exactly the illusion the system
exists to create.

**Immutable per-turn snapshots.** What B remembers must not change halfway
through a sentence.

**The model proposes; deterministic code commits.** Extraction may be a model
call. Admission, TTLs, deletion, ownership, promotion and privacy are not.

---

## Deviations from the proposal

Three, each deliberate.

### 1. One crate, not eight

Same module names, same boundaries, one publishable unit. Splitting into eight
crates multiplies manifests, versions and CI targets without adding a boundary
the module graph does not already enforce.

### 2. An in-process BM25 index rather than Tantivy

The proposal named Tantivy. The index is a *derived, disposable* artefact by the
proposal's own principle (§6.1), rebuilt from the corpus in milliseconds. At this
corpus size an in-process inverted index answers in tens of microseconds and
needs no segment management, no writer lifecycle, and no crash-recovery story of
its own — the canonical Markdown *is* the recovery story. That removes a whole
class of "index disagrees with corpus" failures, and drops a heavy dependency
from a published crate.

`MemoryIndex`'s surface is narrow (`upsert` / `remove` / `search` / `build`)
precisely so a segment-based backend can be substituted later without touching a
caller.

### 3. Ranking signals multiply rather than add

The proposal's §13.3 reads as additive boosts, and §36.3 renders an additive
derivation. Implemented additively, ranking is unstable: BM25's IDF term shrinks
as a term becomes common, so a fixed `+1.5` recency boost can outweigh the entire
relevance signal and rank a recent-but-irrelevant record above an exact match.
This was caught by a test, not by inspection.

Signals therefore multiply the lexical score. The explanation still records each
signal as the delta it actually caused, so a rendered derivation adds up to the
final score exactly as §36.3 shows — and a test asserts that it does.

### 4. A hand-written YAML subset for front matter

`serde_yaml` is unmaintained and `cargo-deny` is configured to fail on
unmaintained workspace dependencies. The engine both writes and reads OKF front
matter, so it needs exactly the shape it emits: block mappings, block sequences
of scalars, and scalars. That subset is ~250 lines, carries no dependency, and
names the offending line on a parse failure. Flow mappings, anchors, aliases,
multi-line scalars and tab indentation are rejected with a diagnostic.

---

## Privacy and trust

- **User speech only.** Bystander, assistant-originated and unattributed speech
  is refused at ingestion, and again at admission — not filtered downstream.
- **Sensitive categories are never promoted on inference.** Health, religion,
  politics, sexuality and similar require an explicit statement. Repetition of a
  sensitive inference is still an inference, and the promotion bar says so.
- **Retrieved memory is untrusted data.** Instruction-shaped content is refused at
  ingestion, so it can never be replayed as an instruction. Memory reaches the
  model as tool-response data, never as system instruction.
- **Deletion removes content.** A deleted record leaves a content-free tombstone
  carrying an id and a timestamp; a test asserts the statement text is gone from
  every file, including superseded copies.
- **Namespace isolation.** Records are keyed by owner, a write into another
  user's namespace is refused, and every path is checked for traversal before it
  reaches a filesystem.

---

## Failure handling

Memory failure never terminates a voice interaction. Every path degrades:

| Failure | Behaviour |
|---------|-----------|
| Partial transcripts dropped | Final transcript is authoritative; nothing lost |
| Plan extractor fails or hangs | Falls back to the deterministic plan at the deadline |
| Observation extractor fails | Turn's evidence is skipped; the transcript event allows a retry |
| Semantic fallback fails or is slow | Lexical results stand; the caller never learns |
| Lexical search fails | Overlay only, then an empty result |
| Event log unavailable | Explicit mutations are not reported as durable |
| Reconciliation fails | Sealed ledger is preserved; retryable |
| Index publication fails | Previous revision keeps serving; the corpus is unaffected |

Ordering is never assumed. The Live API delivers input transcription
independently of turn boundaries, so turn identity is assigned locally and every
asynchronous task carries the generation it started at; a late result is
discarded rather than allowed to overwrite newer context.

---

## Evaluation

`evals` runs a hand-written corpus through the real pipeline — no stubs — and
enforces the design's acceptance criteria as tests:

| Metric | Threshold |
|--------|-----------|
| Retrieval precision | ≥ 0.85 |
| Retrieval recall | ≥ 0.80 |
| Skip accuracy | 1.00 |
| Forbidden-record leakage | 0 |
| Mean context | ≤ 250 tokens |
| Maximum context | ≤ 500 tokens |
| Ingestion accuracy | 1.00 |
| False durable stores | 0 |

The cases are the specification. Changing one to make a run go green is a product
decision, not a fix.

---

## What is not built

Honest list.

- **Model-backed extraction.** Both extractor traits ship with rule-based
  implementations and deadline wrappers. The rules are a floor: they capture
  explicit statements and memory commands, which is most of what matters, and
  nothing subtler. Wiring a structured-output Gemini call is the next step and
  touches nothing else.
- **Durable repository and event log backends.** Both are traits with in-process
  implementations plus a filesystem-backed store. Redis, DynamoDB or object
  storage are drop-in.
- **Semantic fallback backend.** The seam, the deadline and the ranking
  integration exist; no embedding index is wired to them.
- **Session persistence across process restarts.** The event log records enough
  to rebuild a session overlay by replay; the replay path itself is not written.
- **Longitudinal simulation.** §37.4's 30/90/180-day synthetic timelines are not
  generated, so corpus growth and contradiction drift are unmeasured.
- **Explain CLI.** `SearchExplanation::render` produces the output; no
  `bmem explain-search` binary wraps it.
- **Idempotency keys do not survive a repository reload.** They are held in
  process, so retrying an at-least-once transaction after a restart applies it
  twice and inflates evidence counters. Persisting them in the manifest is the
  fix; the in-process guarantee holds today, the cross-restart one does not.
- **Proactive context injection.** V1 deliberately uses tool responses. The
  turn-boundary injection path stays unbuilt until it is validated against real
  Live model behaviour.

---

## Language

Ingestion is code-switch native — Hinglish, Tanglish and Devanagari all extract
correctly, because the extraction model does that work.

Retrieval was first made to work the obvious way: Hindi and Tamil recall
phrases, romanized function words as stop words, a kinship table. It passed,
and it was wrong. The lists were load-bearing because the planner used them to
decide *whether to search at all*, so every phrasing missing from a list was a
question that silently got no memory — and no list is ever finished. The rule
planner was being asked to be a language model.

The lists are gone. Three properties replace them, none of which name a
language:

- **Search unless there is nothing to search with.** A local BM25 pass costs
  tens of microseconds, so the planner no longer rules on whether a question
  "needs" memory — a judgement it cannot make. It strips function words and
  searches. The only skip left is the lexical one: an utterance with no content
  words has no query to run.
- **An absent term is free.** `hai` and `enakku` do not need to be stop words.
  A term no document contains has no postings, contributes no score, and IDF
  discounts the ones that appear everywhere. Removing them by hand bought
  nothing that the index was not already doing.
- **Entities come from the corpus.** `wife` resolves to `rhea` because the fact
  about Rhea lists it as an alias, written by the extraction model in whatever
  language the user spoke. A relationship the engine has never heard of has no
  memories to retrieve, so failing to spot it costs nothing.

Every stored fact still carries model-generated **search terms** in both the
user's language and English; lexical retrieval can only match words that are
present. That is where the language knowledge lives now — in the corpus, from
the model that read the speech, rather than in a table shipped in the binary.
The remaining phrase tables are hints only: a missed match costs a little
ranking quality on that turn, never the memory.

The remaining gap is a question in one language about a fact stored before this
change, whose search terms are English-only. Recompiling the corpus does not
regenerate them; only a restatement does.

## Known limits

- **No stemming beyond regular plurals.** "diet" will not match a record indexed
  under "dietary". This is what record aliases and the semantic fallback are for,
  and the tokenizer says so. Aggressive stemming conflates names, which in a
  personal corpus is worse.
- **Refinement detection is lexical.** A more specific restatement refines when
  it strictly contains the incumbent's terms. Semantically-equivalent
  paraphrases that share no vocabulary reconcile as contradictions.
- **Coexistence needs an explicit qualifier.** Two facts under one predicate
  coexist when they carry different qualifiers; the engine does not infer that a
  qualifier is what distinguishes them.
- **Deletion by topic is lexical**, and a command naming no target deletes
  nothing rather than guessing.

---

## Rollout

| Phase | Status |
|-------|--------|
| 0 — Foundations: schemas, OKF, index, evals | Done |
| 1 — Read-only Live memory: plans, snapshots, `recall_context` | Done |
| 2 — Session ingestion: observations, ledger, overlay, `manage_memory` | Done |
| 3 — Durable reconciliation: sealing, consolidation, resolution, commit | Done |
| 4 — Pattern intelligence: promotion; semantic fallback seam | Partial — no embedding backend, no longitudinal eval |
| 5 — User controls and hardening | Not started — no view/export UI, no load testing, no DR |
