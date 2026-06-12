# gemini-adk-rs: DevEx & Performance Reference

**The honest guide.** What's fast, what's slow, what's elegant, what's janky, where the bodies are buried.

---

## The Stack

```
┌─────────────────────────────────────────────────────────┐
│  L2: gemini-adk-fluent-rs                                      │
│  Live::builder().model().voice().phase().connect()       │
│  Composition: S | C | P | T | M | A                     │
│  64 builder methods → compiles 1:1 to L1                │
├─────────────────────────────────────────────────────────┤
│  L1: gemini-adk-rs                                             │
│  LiveSessionBuilder + EventCallbacks + PhaseMachine     │
│  TextAgent pipeline: LlmTextAgent → BaseLlm → tools    │
│  Three-lane processor: fast | control | telemetry       │
├─────────────────────────────────────────────────────────┤
│  L0: gemini-genai-rs                                           │
│  WebSocket transport + JSON codec + auth providers      │
│  HTTP client (reqwest) for REST generateContent         │
│  VAD, jitter buffer, audio pipeline                     │
└─────────────────────────────────────────────────────────┘
```

Everything above L0 is zero-cost at the wire level. L2 is a thin descriptor layer — no intermediate representation, no optimizer, no runtime overhead beyond L1.

---

## Performance: The Numbers That Matter

### Latency Budget Per Voice Turn

```
Event: User stops speaking → Model responds

  VAD detects silence         ~0 alloc    0μs overhead (fast lane, inline)
  TurnComplete arrives        ~0 alloc    routed to control lane via mpsc
  ├─ clear turn: state        O(n)        n = turn:* key count (~5-10)
  ├─ watcher snapshots        O(w)        w = watched keys (~3-5 HashMap clones)
  ├─ extractor LLM calls      concurrent  100-500ms each (DOMINATES)
  ├─ auto-flatten results     O(f)        f = extracted fields (~5-8 clones)
  ├─ computed state eval      O(c)        c = dirty computed vars (~2-3)
  ├─ phase evaluate           0 alloc     1 HashMap lookup + guard closures
  ├─ instruction resolve      1-2 clones  modifiers append to single String
  ├─ dedup check + send       1 lock      parking_lot::Mutex (~10ns)
  ├─ on_enter_context         0-1 alloc   sync closure, returns Vec<Content>
  ├─ prompt_on_enter          0 alloc     send turnComplete:true
  ├─ fire watchers            O(w)        per changed key, mostly no-ops
  ├─ fire temporal patterns   O(t)        detector tick, ~100ns each
  └─ callbacks                2-3 awaits  on_turn_boundary, on_turn_complete
                                          ──────────────
                                          Total framework overhead: <1ms
                                          Total with extractors: 100-500ms
                                          Network to Gemini API: free (already connected via WS)
```

**The honest truth:** Extractors dominate. Everything else is noise. If you have 3 extractors running `gemini-2.5-flash` concurrently via `join_all`, your turn latency is `max(extractor_1, extractor_2, extractor_3)` ≈ 200-500ms. The framework adds <1ms on top.

### Fast Lane: Zero Allocation

```
Audio arrives → on_audio(&Bytes) callback → your code

  broadcast::recv()          0 alloc    bytes::Bytes is refcounted, not cloned
  route to fast lane         0 alloc    enum variant, no heap
  callback invocation        0 alloc    &Bytes reference
  ───────────────
  Total: 0 heap allocations per audio frame
  Latency: <1μs framework overhead
```

Same for `on_text(&str)`, `on_input_transcript(&str, bool)`, `on_output_transcript(&str, bool)`. The fast lane never touches a Mutex, never awaits, never allocates. This is the path for real-time audio streaming.

### Control Lane: Where Work Happens

Tool calls, turn completion, phase transitions, extractions — all here. Sequential within the lane, but extractors run concurrently via `join_all`. The lane uses a 64-slot mpsc channel. If you block the control lane for >500ms, you'll queue up events.

### HTTP REST Path (TextAgent → Gemini API)

```
TextAgentTool.call(args)
  └─ LlmTextAgent.run(state)
       └─ GeminiLlm.generate(request)          ← uses cached Client
            └─ gemini_genai_rs::Client.generate_content_with()
                 └─ reqwest::Client.post()      ← reuses connection pool
                      └─ HTTP/2 to Gemini API   200-2000ms
```

**What's warm at session start:**
- `TextAgentTool` — Arc, created once
- `LlmTextAgent` — Arc, created once
- `GeminiLlm` — owns `gemini_genai_rs::Client`, created once
- `gemini_genai_rs::Client` — owns `reqwest::Client` with connection pool
- `ToolDispatcher` — Arc, HashMap lookup per call

**What's NOT warm on first call:**
- TCP+TLS handshake to Gemini API: ~100-300ms cold start
- Fix: call `llm.warm_up()` at startup

**Per-call cost after warm-up:**
- Build `LlmRequest`: ~1μs (stack struct + small vec)
- Build `GenerateContentConfig`: ~1μs (wrapper)
- reqwest sends over existing HTTP/2 connection: 0 handshake
- Network round-trip: 200-2000ms (irreducible)

### Wire Layer Overhead (L0)

| Operation | Time | Allocations |
|-----------|------|-------------|
| JSON encode text message | 1-3μs | 1 |
| JSON encode audio (base64 + JSON) | 5-20μs | 2 |
| JSON decode text response | 1-5μs | 1 |
| JSON decode audio (JSON + base64 decode) | 10-50μs | 2 |
| Arc clone | 2-5ns | 0 |
| tokio::spawn | 200-500ns | 1 (task) |
| DashMap get (uncontended) | 10-30ns | 0 (but clone after) |
| broadcast send | 50-100ns | 0 |

**Base64 tax:** 33% size overhead on all audio. Unavoidable — Gemini's wire protocol is JSON. A protobuf codec would eliminate this, but the API doesn't support it for Live.

### What Allocates Per State Read

This is the one hidden cost worth knowing:

```rust
state.get::<String>("key")
  → DashMap.get("key")           // O(1), lock-free read
  → value.clone()                // CLONE the serde_json::Value
  → serde_json::from_value(v)    // deserialize to target type
```

**Every `state.get()` clones the JSON Value and deserializes it.** For a String, that's one allocation. For a complex struct, it's a tree of allocations. This is fine for 5-10 reads per turn. If you're reading state in a tight loop, use `state.get_raw()` and work with `serde_json::Value` directly.

Guard closures (in phase transitions) typically do 1-3 `state.get()` calls. At 3 transitions × 3 reads = 9 clones per evaluation. Still <1μs total.

---

## DevEx: The Composition Algebra

### The Six Modules

| Module | Operator | What It Composes | Used In |
|--------|----------|-----------------|---------|
| **S** — State | `>>` | Transforms, predicates | Guards, transitions |
| **C** — Context | `+` | Content filters, policies | Context engineering |
| **P** — Prompt | `+` | Sections, modifiers | Instructions, phases |
| **T** — Tool | `\|` | Tool declarations | Tool registration |
| **M** — Middleware | `\|` | Cross-cutting concerns | Pipeline decoration |
| **A** — Artifact | `+` | I/O contracts | Agent pipelines |

### S — State Predicates (the most-used)

```rust
// Guards that read like English
.transition("verify", S::is_true("disclosure_given"))
.transition("pay",    S::one_of("intent", &["full_pay", "partial_pay"]))
.transition("close",  S::eq("status", "resolved"))

// Compose with boolean logic
.transition("escalate", |s| {
    S::is_true("cease_desist")(s) || S::eq("intent", "dispute")(s)
})
```

No ceremony. No builder. Just closures that read state. `S::is_true` returns `impl Fn(&State) -> bool` — a closure you can pass directly as a guard or combine with `||`/`&&`.

### P — Dual Personality

**Persona 1: Prompt engineering** (offline composition)
```rust
let prompt = P::role("debt collector")
    + P::task("Negotiate payment arrangement")
    + P::constraint("Never threaten legal action")
    + P::guidelines(&["Be empathetic", "Document everything"]);

Live::builder().instruction(prompt)  // Into<String>
```

**Persona 2: Runtime instruction modifiers** (per-turn injection)
```rust
.phase_defaults(|d| d
    .with_state(&["emotional_state", "risk_level"])     // P::with_state under the hood
    .when(|s| risk_is_high(s), "CAUTION: High risk.")   // P::when under the hood
)
```

At runtime, modifiers append to the phase instruction before each turn:
```
[Phase instruction text]
[Context: emotional_state=frustrated, risk_level=high, ...]
CAUTION: High risk. Show extra empathy and follow compliance guidelines.
```

### Phase Defaults: Write Once, Apply Everywhere

```rust
.phase_defaults(|d| d
    .with_state(&["emotional_state", "willingness_to_pay", "derived:risk"])
    .when(risk_elevated, RISK_WARNING)
    .prompt_on_enter(true)
)
.phase("disclosure").instruction(DISCLOSURE).transition("verify", guard).done()
.phase("verify").instruction(VERIFY).transition("inform", guard).done()
.phase("negotiate").instruction(NEGOTIATE).transition("pay", guard).done()
// All 3 phases inherit: with_state + when(risk) + prompt_on_enter
```

Without `phase_defaults`, you'd repeat `.with_state().when().prompt_on_enter()` on every phase. For 7 phases × 3 modifiers = 21 redundant method calls eliminated.

### enter_prompt: The One-Liner for Phase Entry

```rust
// Before: 2 methods + Content import
.on_enter_context(|_state, _window| Some(vec![Content::user("Switching to verification.")]))
.prompt_on_enter(true)

// After: 1 method, no imports
.enter_prompt("Switching to verification.")

// Dynamic version:
.enter_prompt_fn(|state, _window| {
    format!("Now verifying {}.", state.get::<String>("caller_name").unwrap_or_default())
})
```

### The Full Builder Chain

```rust
Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .voice(Voice::Kore)
    .instruction("Base instruction")
    .greeting("Start the conversation.")
    // Extraction
    .extract_turns::<MyState>(llm, "Extract emotional_state, intent...")
    .on_extracted(|name, value| async { /* broadcast to UI */ })
    // Computed state
    .computed("risk", &["sentiment"], |s| Some(json!(compute_risk(s))))
    // Phases
    .phase_defaults(|d| d.with_state(&["emotion"]).prompt_on_enter(true))
    .phase("intro").instruction("...").transition("main", guard).done()
    .phase("main").instruction("...").transition("end", guard).done()
    .initial_phase("intro")
    // Watchers
    .watch("risk").crossed_above(0.9).blocking()
        .then(|_, _, s| async { s.set("escalated", true); })
    // Temporal
    .when_sustained("frustration", pred, Duration::from_secs(30), action)
    // Tools
    .agent_tool("analyze", "Run analysis", analyzer_agent)
    .on_tool_call(|calls, state| async { Some(dispatch(calls, state)) })
    // Callbacks
    .on_audio(|data| send_audio(data))
    .on_text(|t| send_text(t))
    .on_turn_complete(|| async { log_turn() })
    // Connect
    .connect_vertex(project, location, token).await?
```

~40 lines for a fully-featured multi-phase voice agent with extraction, computed state, watchers, temporal patterns, and tool dispatch. The raw L1 equivalent would be ~80 lines of manual struct construction.

---

## The Dispatch Architecture

### Voice → Text Agent Dispatch

Two patterns, both pre-warmed at session start:

**1. Synchronous (tool call):**
```
Gemini Live model calls tool "verify_identity"
  → processor dispatches to TextAgentTool          Arc, pre-created
    → TextAgentTool.call(args)                     sets state["input"]
      → LlmTextAgent.run(state)                   shares parent State
        → GeminiLlm.generate(request)             cached Client
          → Gemini REST API                        200-2000ms
        → ToolDispatcher.call("db_lookup", args)   if model calls tools
        → loop until text response or MAX_ROUNDS=10
      ← result as JSON tool response
    ← model continues speaking with result
```

**2. Asynchronous (background dispatch):**
```
on_turn_complete callback fires
  → dispatcher.dispatch("compliance_check", agent, state)
    → tokio::spawn                                 ~200ns
      → semaphore.acquire()                        budget: N concurrent
      → agent.run(state)                           same pattern as above
      → state.set("compliance_check:result", text) watcher can react
```

**Pre-created at session start:**

| Object | Held As | Lifetime |
|--------|---------|----------|
| TextAgentTool | `Arc<dyn ToolFunction>` in ToolDispatcher | Session |
| LlmTextAgent | `Arc<dyn TextAgent>` inside TextAgentTool | Session |
| GeminiLlm | `Arc<dyn BaseLlm>` inside LlmTextAgent | Session |
| gemini_genai_rs Client | Owned by GeminiLlm | Session |
| reqwest Client | Owned by gemini_genai_rs Client | Session (connection pool) |
| ToolDispatcher | `Arc<ToolDispatcher>` | Session |
| State | `Arc<DashMap>` | Session |
| BackgroundAgentDispatcher | Owned by callback closure | Session |

**Nothing gets created on the fly.** The only per-call allocations are the `LlmRequest` struct (~200 bytes) and the HTTP round-trip.

### State Sharing: No Isolation, By Design

```
Voice Session (Live)
  │
  ├── State (Arc<DashMap>) ──────────────────────────┐
  │     ├── emotional_state = "frustrated"           │
  │     ├── risk_level = "high"                      │
  │     ├── identity_verified = false                │
  │     └── compliance_check:result = "..."          │
  │                                                  │
  ├── TextAgentTool::verify ─── reads/writes ────────┤
  │     └── LlmTextAgent ─── sets identity_verified  │
  │                                                  │
  ├── BackgroundAgentDispatcher ─── reads/writes ────┤
  │     └── compliance_agent ─── sets result key     │
  │                                                  │
  ├── Watchers ─── observe ──────────────────────────┤
  │     └── watch("identity_verified").became_true() │
  │                                                  │
  └── Phase Guards ─── read ─────────────────────────┘
        └── S::is_true("identity_verified")
```

Mutations from any agent are immediately visible to watchers, guards, and extractors. No "promote state" step needed. The tradeoff: no isolation between agents. If two agents write the same key, last write wins.

---

## What's Good

1. **Fast lane is genuinely zero-alloc.** Audio callbacks get `&Bytes` references. No copies, no locks, no async. Measured at <1μs overhead.

2. **Three-lane split is correct.** Audio never waits for tool calls. Tool calls never wait for telemetry. Each lane has independent backpressure.

3. **Extractors run concurrently.** `join_all` on extractor futures means 3 extractors take `max(t1, t2, t3)` not `t1 + t2 + t3`.

4. **Client reuse.** `GeminiLlm` caches `gemini_genai_rs::Client` (and its `reqwest::Client` with HTTP/2 connection pool). No per-call client creation.

5. **Phase defaults eliminate repetition.** One call to `.phase_defaults()` replaces N × M redundant modifier registrations.

6. **S predicates are composable.** `S::is_true("x")` returns a closure. Combine with `||`, `&&`, pass directly as guards. No builder ceremony.

7. **Deferred agent tool construction.** `.agent_tool("name", "desc", agent)` stores the spec; resolves at `connect()` time when State exists. No chicken-and-egg.

8. **Instruction dedup.** Processor checks `last_instruction` before sending `update_instruction`. Identical instructions (common when no phase change) are skipped — saves a WebSocket write per turn.

9. **enter_prompt() hides Content import.** Users don't need `use gemini_genai_rs::prelude::Content` for the most common phase-entry pattern.

10. **warm_up() on BaseLlm.** Pre-establish TCP+TLS connection at startup. First real `generate()` call hits a warm connection pool.

## What's Not Good

1. **Every `state.get()` clones + deserializes.** DashMap gives you a ref, but we immediately clone the `serde_json::Value` and deserialize it. For hot-path guard evaluation with 9+ reads per turn, this is hidden allocation pressure. Not a bottleneck today, but it'll show up in profiles if you add 20+ guards.

2. **TranscriptBuffer grows unbounded.** No cap on `turns: Vec<TranscriptTurn>`. For a 30-minute call with turns every 5 seconds, that's ~360 turns accumulating. Each turn owns String data. Extractors only window the last N, but the buffer itself never shrinks.

3. **Channel clone ceremony in callbacks.** Every async callback closure needs to capture its own `tx.clone()`. For 15 callbacks, that's 15 `let tx = tx.clone();` lines. Pure boilerplate. Could be solved with a `CallbackContext` that holds shared references.

4. **State key typos are silent failures.** `S::is_true("idenity_verified")` (typo) silently returns `false`. No compile-time check, no runtime warning. Phase transitions just don't fire, and you debug for 30 minutes.

5. **T module disconnected from Live builder.** `T::google_search() | T::url_context()` produces a `ToolComposite`, but `Live::builder()` doesn't accept it. Tools go through `SessionConfig::add_tool()` or `ToolDispatcher::register()`. The composition algebra doesn't reach the builder.

6. **M module not integrated.** Middleware composition (`M::log() | M::retry(3)`) exists but has no `.middleware()` method on the Live builder. It's an orphaned abstraction.

7. **Phase history grows unbounded.** `PhaseMachine::history: Vec<PhaseTransition>` accumulates every transition forever. For long sessions with frequent transitions, this is a slow leak.

8. **Contents cloned per LlmTextAgent round.** `self.build_request(contents.clone())` deep-clones the entire conversation history on every tool-dispatch round. For a 10-round agent with growing history, round 10 clones everything from rounds 1-9.

9. **No structured error from extractors.** If an extractor's LLM call returns garbage JSON, we get a silent failure (extraction skipped). No callback, no metric, no retry. The state key just doesn't update.

10. **VertexAI token is read once from env.** `GOOGLE_ACCESS_TOKEN` is read at `GeminiLlm::new()` time. Tokens expire after 1 hour. Long-running services will fail silently after token expiry. Need a refresh mechanism.

---

## Performance Invariants

These are the guarantees the architecture provides:

1. **Fast lane < 1ms.** No locks, no async, no allocations. Audio flows from WebSocket to your callback with only a broadcast recv + enum match in between.

2. **Single `update_instruction` per turn.** Modifiers, phase transitions, and template composition all accumulate into one resolved instruction. Dedup check prevents redundant sends.

3. **Extractors never block audio.** They run on the control lane. The fast lane processes audio independently on its own mpsc channel (512 depth).

4. **Phase evaluation is O(transitions).** No allocation. Guard closures are called in order; first match wins. Typical: 2-4 transitions checked.

5. **Watcher evaluation is O(changed_keys).** Only fires for keys whose value actually changed since last snapshot. Typical: 1-2 watchers fire per turn.

6. **Tool dispatch is O(1) lookup + O(1) Arc clone.** HashMap by name, Arc refcount bump. No serialization until the tool function itself runs.

---

## Concrete Setup Costs

### Minimum Viable Voice Agent (text-only, no phases)

```rust
let handle = Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .instruction("You are a helpful assistant.")
    .on_text(|t| print!("{t}"))
    .on_turn_complete(|| async {})
    .connect_vertex(project, location, token).await?;
```

**5 lines.** This connects to Gemini Live, sends/receives text, and calls your callbacks.

### Production Voice Agent (phases, extraction, tools, monitoring)

See the debt collection demo: **~430 lines** of builder setup for:
- 7 phases with guarded transitions
- 2 extractors (LLM + regex)
- 3 computed state variables
- 6 watchers
- 3 temporal patterns
- Tool dispatch with PII redaction
- Full UI streaming via WebSocket

The raw L1 equivalent would be ~700-800 lines. The fluent layer saves ~40%.

### Text Agent Pipeline (for background dispatch)

```rust
let llm = Arc::new(GeminiLlm::new(GeminiLlmParams::default()));
let agent = LlmTextAgent::new("analyzer", llm.clone())
    .instruction("Analyze the conversation for compliance violations.")
    .tools(Arc::new(dispatcher))
    .temperature(0.3);
```

**4 lines.** The agent is pre-created, Arc-wrapped, and reusable across dispatches.

---

## The Honest Perf Summary

| What | Cost | Why |
|------|------|-----|
| Audio passthrough | 0μs overhead | Fast lane, zero alloc, &Bytes ref |
| Text callback | 0μs overhead | Fast lane, zero alloc, &str ref |
| Phase evaluation | <1μs | O(transitions), no alloc, closure calls |
| State read | ~50-200ns | DashMap lock-free + Value clone + deser |
| Instruction send | ~10μs | JSON encode + WS send (deduped) |
| Extractor (per) | 100-500ms | **LLM round-trip (dominates everything)** |
| Tool dispatch | ~1μs + tool time | HashMap lookup + Arc clone + call |
| TextAgent round | 200-2000ms | **HTTP to Gemini API (irreducible)** |
| TurnComplete total | <1ms framework | + extractor time if extractors exist |
| Base64 audio encode | 5-20μs per chunk | Protocol tax, unavoidable with JSON wire |
| First HTTP call | +100-300ms | TLS handshake, use warm_up() to pre-pay |

**Where time actually goes:** Network. Gemini API latency is 100-2000ms per call. The framework adds <1ms. If your agent feels slow, it's not us — it's the LLM round-trip. Optimize by: fewer extractors, smaller prompts, faster models, or skip extraction on trivial turns.
