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
| `phase.needs(&["user:diet"])` | A returning user is not asked again |
| `phase.requires(&["user:diet"])` | A hard gate a memory can open |
| `Flow` guard `done(captured([...]))` | The step advances on memory alone |
| `P::with_state(&["user:diet"])` | The value appears in the phase instruction |
| watchers, repair | Read the same keys, unchanged |

```rust
Live::builder()
    .with_memory_slots(session, [
        MemorySlot::new("dietary_identity", "user:diet"),
        MemorySlot::new("venue_preference", "user:venue"),
    ])
    .phase("gather")
        .needs(&["user:diet", "user:venue"])   // skipped for a returning user
        .done()
    .phase("suggest")
        .requires(&["user:diet"])              // opened by memory
        .done()
```

Slots promote with `KeepKnown`, so what the live conversation established always
beats what memory recalls.

Each row of that table is asserted against a *driven* `PhaseMachine` and
`FlowMonitor` in `tests/governed_integration.rs` — a real `requires` gate that
stays shut until memory opens it, a real `Guard::captured` that admits a tool
only once the slot exists. Asserting the slot's value alone would prove the
extractor writes a key, not that any gate reads it.

**Slot keys use the platform's `scope:key` convention** — `user:diet`, not
`user.diet`. The gates are indifferent: `needs`, `requires` and `Guard::is_set`
route through `State::contains`, which treats the key as an opaque string, so
either form satisfies them. The colon buys composition with the prefix scopes,
so `state.user().get::<String>("diet")` finds the slot instead of silently
reading `None`. `derived:` would be semantically apt and functionally wrong: its
fallback exists only in `get`/`with`, and `contains` has none, so a `derived:`
slot would be invisible to exactly the gates memory exists to satisfy.

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

## Storage and index lifecycle

There are exactly two artifacts. Markdown files are the truth; the index is a
cache that can be thrown away and rebuilt from them.

### Where a fact lives

`category_path` routes a record to a file by status, then by kind. Status wins,
so a superseded record physically leaves the active file:

```
users/<user>/profile.md                 Identity
users/<user>/preferences.md             Preference, LocationPreference
users/<user>/relationships.md           RelationshipPreference
users/<user>/routines.md                Routine
users/<user>/episodes/<YYYY-MM>.md      Episodic, partitioned by month
users/<user>/staged/patterns.md         Staged (promoted, not yet confirmed)
users/<user>/superseded/<YYYY-MM>.md    Superseded, Expired
users/<user>/tombstones/records.md      Deleted
users/<user>/manifest.json              id → path, status, kind + revision
```

One file holds many records, concatenated as YAML front matter blocks. The
front matter is the machine's copy; the `# Fact` / `# Evidence Summary` /
`# Supersedes` sections below it are the human's, and they are what a user is
shown when they ask what is remembered.

### Write

Commits are transactional per user namespace and idempotent by key:

1. Apply writes to the in-memory namespace.
2. Re-materialize every affected file from scratch — a record that changed
   status is written to its new file and removed from its old one in the same
   pass, so it can never appear in both.
3. Write only files whose contents actually changed; delete files no longer
   wanted. `FsStore` writes to a temp path and renames, so a reader never sees
   a half-written file.
4. Write `manifest.json` **last**. It is the commit point: until it lands, the
   revision has not advanced and a retry re-applies cleanly.
5. Only then publish the new revision in memory and record the idempotency key.

The manifest carries the last 256 idempotency keys, so the at-least-once
guarantee survives a restart. Reconciliation retried against a freshly opened
repository recognises a transaction that already landed instead of applying it
a second time and re-reinforcing the same evidence.

An optimistic `expected_revision` makes a concurrent commit fail with
`RevisionConflict` rather than interleave.

### Index

`compile_index` reads the corpus, keeps `Active` records, tokenizes each into
fielded postings (subject, entities, aliases, predicate, tags, location,
statement), and swaps the result into an `IndexHandle` whose `generation`
counter increments. It is a **full rebuild, not incremental** — a plan issued
against generation *N* is discarded if it lands after *N+1*, which is what
makes the swap safe without locking readers.

It runs after session completion and after reconciliation, never mid-turn. The
entity table is rebuilt with it, which is how `wife → rhea` stays current.

Two indexes are searched, not one: the canonical index above, and a session
overlay holding what the user said in *this* conversation. The overlay shadows
canonical, so a correction takes effect immediately rather than after the next
compile.

---

## Latency

Measured, not estimated: `cargo run --release --example latency_budget` for the
local path, `tests/model_latency_probe.rs` for the model calls. 4-core
container, `gemini-2.5-flash`.

### The synchronous path — all of it local

| corpus | plan (rules) | search + fuse + assemble | total p50 |
|---|---|---|---|
| 10 | 9.3 µs | 4.6 µs | **14 µs** |
| 100 | 7.3 µs | 16.2 µs | **23 µs** |
| 1 000 | 9.5 µs | 73.2 µs | **83 µs** |
| 10 000 | 19.0 µs | 992 µs (p95 9.4 ms) | **1.0 ms** |

A personal corpus is hundreds of facts, so the real answer is **tens of
microseconds, against a ~20 ms audio frame**. Nothing here needs a budget.

10 000 records is the scaling cliff — p95 reaches 9.4 ms because scoring is
linear in matching documents. It is far past where a single user's memory goes,
but it is where sharding would start.

### Everything else is off the path

| stage | p50 | p95 | when |
|---|---|---|---|
| index build, 100 records | 573 µs | 620 µs | after a session |
| index build, 1 000 records | 5.2 ms | 6.4 ms | after a session |
| index build, 10 000 records | 67 ms | 77 ms | after a session |
| observation extraction | 2.2 s | 5.2 s | after the user's turn completes |
| prepare incl. model plan | 1.6 s | 1.8 s | speculatively, during the model's reply |

Both model calls are bounded and degrade rather than block: plan extraction
races a 4 s deadline with the rule plan already in hand, and observation
extraction failing raises `ExtractionFailed` without failing the turn.

**Total added to the user-perceived response path: the tens of microseconds in
the first table.** The seconds in the second table are spent while the model is
already speaking.

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

### English is the meeting point, on both sides

Two rules that look opposed and are the same rule seen from either end:

- **The canonical record is always English.** `statement`, `predicate`,
  `value`, `subject`, `qualifier`. Reconciliation is lexical, so the same claim
  spoken as "I am vegetarian", "main vegetarian hoon" and "मैं शाकाहारी हूँ"
  must land on one predicate and one value or three restatements reinforce
  nothing and produce three records. Tested in `canonical_language_e2e`.
- **The search terms are never normalized to English.** The query arrives in
  whatever language the user is speaking this turn, and an index holding only
  English has nothing for it to match.

The bridge is that **both** sides carry English as well as the user's words.
The retrieval plan expands "mera khaana ka preference" to `khaana, food, diet`;
the stored fact carries `khaana, food, diet, vegetarian`. They meet on `food`
and `diet`.

Getting this wrong is subtle and was caught only by the end-to-end tests: an
earlier prompt told the extractor to keep the user's own words, and it dutifully
stored `hoon` and `khata` — the words in *that* sentence — instead of `khaana`,
the word a *later question* would use. Retrieval went to zero. Making the query
side expand into English too is what removed the dependency on ingestion
correctly guessing future vocabulary months in advance.

### Predicates come from the corpus, like entities

The extraction model is shown the predicate names already in use for this user
and told to reuse one when the new fact is about the same thing — *including
when it contradicts*. Left to name each fact freshly it writes
`dietary_preference` one session and `dietary_identity` the next, and
"actually I'm pescatarian now" becomes a second active record instead of
superseding the first. That was a ~1-in-3 flake on the correction test; it is
5-for-5 with the corpus vocabulary in the prompt.

This is the entity table's trick applied to predicates, and it is the third
instance of the same pattern: **the vocabulary comes from the data, not from a
list in the binary and not from the model's imagination.**

The remaining gap is a question in one language about a fact stored before this
change, whose search terms are English-only. Recompiling the corpus does not
regenerate them; only a restatement does.

## Known limits

- **No stemming beyond regular plurals.** "diet" will not match a record indexed
  under "dietary". This is what record aliases and the semantic fallback are for,
  and the tokenizer says so. Aggressive stemming conflates names, which in a
  personal corpus is worse.

  SPLADE was priced as the fix and measured in `experiments/`. Query-side is out
  — 21 ms p50 on four idle cores, 64 ms on one, against a synchronous path that
  currently costs 83 µs. Doc-side is affordable (~21 ms once per record, at
  consolidation time, cached into the front matter) and its English expansions
  are genuinely good: `vegetarian → meat, eat, eating, food, animal`. It is not
  adopted yet because it buys English recall only — the model has no Hindi, so
  `khaana` expands to WordPiece debris — for the cost of an ONNX Runtime
  dependency and a 532 MB model.
- **Refinement detection is lexical.** A more specific restatement refines when
  it strictly contains the incumbent's terms. Semantically-equivalent
  paraphrases that share no vocabulary reconcile as contradictions.
- **Coexistence needs an explicit qualifier.** Two facts under one predicate
  coexist when they carry different qualifiers; the engine does not infer that a
  qualifier is what distinguishes them.
- **Deletion by topic is lexical**, and a command naming no target deletes
  nothing rather than guessing.
- **The Live path is covered for text-in/audio-out only.** `live_session_e2e`
  connects a real WebSocket session, states a fact over the wire and reads the
  model's *output transcription* — the three things an application does that no
  text-path test touches. VAD, barge-in and partial-transcript speculation still
  have no live coverage; they need a real microphone stream.

  Those tests resolve the model from `GEMINI_LIVE_MODEL` rather than a
  `GeminiModel` variant, because both named Live variants have been retired
  server-side. See the `live_model` doc comment for how to list what a key can
  actually reach.

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
