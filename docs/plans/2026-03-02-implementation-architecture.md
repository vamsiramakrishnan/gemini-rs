# Implementation Architecture — Module Map, Deltas & Concurrency Design

**Date**: 2026-03-02
**Status**: Implementation Blueprint
**Input Documents**:
- `callback-mode-design.md` — CallbackMode, ToolExecutionMode, ResultFormatter
- `fluent-devex-redesign.md` — FnStep, dispatch/join, LiveHook, enhanced Middleware, Route/Gate
- `voice-native-state-control-design.md` — Computed state, Phase machine, Watchers, Temporal patterns
- `gemini-genai-api-behavior.md` — Wire protocol, session lifecycle, audio/video, VAD, voice/language, constraints

---

## 1. Current Architecture Snapshot

### What Exists Today (Abbreviated)

```
gemini-genai-rs (L0) — Wire protocol, zero application logic
├── SessionHandle            — send_audio/text/video/tool_response/client_content/instruction
├── SessionEvent (17 variants) — broadcast to subscribers
├── SessionCommand (9 variants) — mpsc to transport loop
├── SessionPhase (10 states)   — FSM with validated transitions
├── SessionConfig              — setup builder, URL generation, wire serialization
├── SessionWriter/Reader       — traits for abstraction
├── FunctionCallingBehavior    — Blocking / NonBlocking (wire types only, unused by runtime)
├── Transport / Codec traits   — generic connection loop
└── Content/Part/Role/FunctionCall/FunctionResponse — wire primitives

gemini-adk-rs (L1) — Runtime, callbacks, tools, agents
├── State                      — DashMap<String, Value>, delta tracking, app/user/temp prefixes
├── EventCallbacks             — 8 sync fast-lane + 10 async control-lane + 3 interceptors
├── processor.rs               — spawn_event_processor(), route_event(), run_fast_lane(), run_control_lane()
├── SharedState                — interrupted AtomicBool, resume_handle, last_instruction
├── LiveSessionBuilder         — config + callbacks + dispatcher + extractors → connect()
├── LiveHandle                 — send_*, state(), extracted()
├── TranscriptBuffer           — push_input/output, end_turn(), window(), format_window()
├── TurnExtractor / LlmExtractor — OOB LLM extraction on TranscriptTurn window
├── ToolDispatcher             — register_function/streaming/input, call_function, cancel_by_ids
├── ToolFunction/StreamingTool/InputStreamingTool — traits
├── Middleware / MiddlewareChain — observe-only hooks (8 methods)
├── Plugin / PluginManager     — control-flow hooks (Continue/Deny/ShortCircuit)
├── Agent trait / LlmAgent     — event-loop agent implementation
├── AgentSession               — intercepting SessionWriter wrapper
└── AgentTool                  — wraps Agent as ToolFunction

gemini-adk-fluent-rs (L2) — Fluent DX, composition, patterns
├── Live                       — builder wrapping LiveSessionBuilder (~50 methods)
├── AgentBuilder               — copy-on-write immutable agent builder (~30 setters)
├── Composable                 — Agent | Pipeline | FanOut | Loop | Fallback
├── Operators                  — >> | * / for composition
├── Patterns                   — review_loop, cascade, fan_out_merge, supervised, map_over
├── S/C/P/M/T/A modules       — state, context, prompt, middleware, tools, artifacts composition
└── Testing                    — check_contracts() for data-flow validation
```

---

## 2. Delta Map — Every Change Required, By File

### 2.1 gemini-genai-rs (L0) — Wire Protocol Additions

L0 remains a thin, fast, allocation-minimal wire layer. All intelligence lives
in L1 and L2. However, the API behavior study (`gemini-genai-api-behavior.md`)
reveals several wire-level features that need L0 support. These are pure protocol
additions — no application logic.

| Proposed Feature | L0 Impact | Rationale |
|-----------------|-----------|-----------|
| CallbackMode | None | Processor-level concern, not wire-level |
| ToolExecutionMode | None | `FunctionCallingBehavior` wire types already exist |
| Computed state | None | Application state, not protocol state |
| Phase machine | None | Application logic, not session state |
| Watchers | None | Application triggers, not protocol events |
| Temporal patterns | None | Application timing, not transport timing |
| **Session Resumption** | **New** | New server message `sessionResumptionUpdate` → new `SessionEvent` variant |
| **Server Transcription** | **New** | `inputTranscription`/`outputTranscription` fields in `serverContent` |
| **GoAway payload** | **Verify** | `goAway.timeLeft` → ensure `SessionEvent::GoAway(Duration)` parses the field |
| **Setup config fields** | **New** | VAD, affective dialog, proactive audio, media resolution, compression, resumption |

#### 2.1.1 New SessionEvent Variants

```rust
// Verify/add to SessionEvent enum:
SessionEvent::GoAway { time_left: Duration },
SessionEvent::SessionResumptionUpdate {
    session_id: String,
    resumable: bool,
    new_handle: String,
},
SessionEvent::InputTranscription { text: String },
SessionEvent::OutputTranscription { text: String },
```

**Note**: `InputTranscription` and `OutputTranscription` arrive in `serverContent`
alongside (or instead of) `modelTurn`. The codec must parse these new fields and
emit them as separate events so L1 can route them independently.

#### 2.1.2 SessionConfig — New Setup Fields

```rust
// New fields on SessionConfig / generation_config:
pub struct SessionConfig {
    // ... existing fields ...

    // Context window compression
    pub context_window_compression: Option<ContextWindowCompression>,

    // Session resumption
    pub session_resumption: Option<SessionResumption>,

    // Audio transcription (empty struct = enabled)
    pub input_audio_transcription: bool,
    pub output_audio_transcription: bool,

    // Affective dialog (native audio models only)
    pub enable_affective_dialog: bool,

    // Media resolution for video frames
    pub media_resolution: Option<MediaResolution>,

    // VAD configuration
    pub realtime_input_config: Option<RealtimeInputConfig>,

    // Proactive audio (native audio models only)
    pub proactive_audio: bool,
}

pub struct ContextWindowCompression {
    pub trigger_tokens: u32,         // 5,000–128,000
    pub sliding_window: SlidingWindow,
}

pub struct SlidingWindow {
    pub target_tokens: u32,          // 0–128,000
}

pub struct SessionResumption {
    pub handle: Option<String>,      // None on first connect, Some on resume
    pub transparent: bool,
}

pub enum MediaResolution {
    Low,
    Medium,
    High,
}

pub struct RealtimeInputConfig {
    pub automatic_activity_detection: ActivityDetection,
}

pub struct ActivityDetection {
    pub disabled: bool,
    pub start_of_speech_sensitivity: Option<Sensitivity>,
    pub end_of_speech_sensitivity: Option<Sensitivity>,
    pub prefix_padding_ms: Option<u32>,
    pub silence_duration_ms: Option<u32>,
    pub voice_activity_timeout: Option<Duration>,
}

pub enum Sensitivity {
    Low,
    High,
}
```

#### 2.1.3 Server Message Parsing Updates

The `JsonCodec` (or equivalent) must parse these new fields from server messages:

```rust
// In serverContent parsing:
if let Some(input_tx) = server_content.get("inputTranscription") {
    // Emit SessionEvent::InputTranscription { text }
}
if let Some(output_tx) = server_content.get("outputTranscription") {
    // Emit SessionEvent::OutputTranscription { text }
}

// New top-level server message:
if let Some(resumption) = msg.get("sessionResumptionUpdate") {
    // Emit SessionEvent::SessionResumptionUpdate { session_id, resumable, new_handle }
}
```

**Total L0 delta**: ~200 lines (new types + config serialization + parsing).

### 2.2 gemini-adk-rs (L1) — The Runtime Engine

L1 is where most of the heavy implementation lives. The changes are organized
by subsystem.

#### 2.2.1 `state.rs` — State Enhancements

**Current**: 480 lines. `State` struct with DashMap, delta tracking, 3 prefixes.

**Delta**:

| Change | Type | Lines Est. |
|--------|------|-----------|
| Add `session()` prefix accessor | Modify | +10 |
| Add `turn()` prefix accessor | Modify | +10 |
| Add `bg()` prefix accessor | Modify | +10 |
| Add `derived()` prefix accessor (read-only) | Modify | +20 |
| Add `snapshot_values(&self, keys: &[&str]) -> HashMap<String, Value>` | New method | +15 |
| Add `diff_values(&self, prev: &HashMap<String, Value>, keys: &[&str]) -> Vec<(String, Value, Value)>` | New method | +20 |
| Add `clear_prefix(&self, prefix: &str)` | New method | +10 |
| Add `recompute` hook placeholder (see computed module) | Modify | +5 |

**Total delta**: ~100 lines added. **No breaking changes** — existing API untouched.

The `snapshot_values` and `diff_values` methods are the foundation for watchers:
snapshot state before processing, diff after, fire watchers for changed keys.

```rust
// New methods on State
impl State {
    pub fn session(&self) -> PrefixedState<'_> { PrefixedState::new(self, "session:") }
    pub fn turn(&self) -> PrefixedState<'_> { PrefixedState::new(self, "turn:") }
    pub fn bg(&self) -> PrefixedState<'_> { PrefixedState::new(self, "bg:") }
    pub fn derived(&self) -> ReadOnlyPrefixedState<'_> { ReadOnlyPrefixedState::new(self, "derived:") }

    pub fn snapshot_values(&self, keys: &[&str]) -> HashMap<String, Value> { /* ... */ }
    pub fn diff_values(&self, prev: &HashMap<String, Value>, keys: &[&str]) -> Vec<(String, Value, Value)> { /* ... */ }
    pub fn clear_prefix(&self, prefix: &str) { /* ... */ }
}
```

#### 2.2.2 NEW: `live/computed.rs` — Computed State Engine

**Current**: Does not exist.

**New module**: ~200 lines.

```rust
/// A computed state variable: pure function of other state keys.
pub struct ComputedVar {
    pub key: String,
    pub dependencies: Vec<String>,
    pub compute: Arc<dyn Fn(&State) -> Option<Value> + Send + Sync>,
}

/// Registry of computed variables with dependency-ordered evaluation.
pub struct ComputedRegistry {
    vars: Vec<ComputedVar>,           // topologically sorted
    dep_index: HashMap<String, Vec<usize>>, // key → indices that depend on it
}

impl ComputedRegistry {
    pub fn new() -> Self;

    /// Register a computed variable. Panics if cycle detected.
    pub fn register(&mut self, var: ComputedVar);

    /// Recompute all variables in dependency order. Returns keys that changed.
    pub fn recompute(&self, state: &State) -> Vec<String>;

    /// Recompute only variables affected by the given changed keys.
    pub fn recompute_affected(&self, state: &State, changed: &[String]) -> Vec<String>;

    /// Validate dependency graph (no cycles, no missing deps). Called once at build time.
    pub fn validate(&self) -> Result<(), String>;
}
```

**Performance**: Topological sort happens once at registration (build time).
`recompute()` iterates the sorted list, calling each function. For N computed
vars each taking < 1ms, total cost is < N ms. Typical: 3-10 vars, < 10ms.

**Concurrency**: `recompute()` is called on the control lane task only (single
thread). No concurrent access — all DashMap reads are lock-free, writes go
through the control lane's sequential execution.

#### 2.2.3 NEW: `live/phase.rs` — Phase Machine

**Current**: Does not exist. Phase transitions are ad-hoc in instruction_template.

**New module**: ~350 lines.

```rust
/// A conversation phase with instruction, tools, and transitions.
pub struct Phase {
    pub name: String,
    pub instruction: PhaseInstruction,
    pub tools_enabled: Option<Vec<String>>,  // None = all tools
    pub guard: Option<Arc<dyn Fn(&State) -> bool + Send + Sync>>,
    pub on_enter: Option<Arc<dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync>>,
    pub on_exit: Option<Arc<dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync>>,
    pub transitions: Vec<Transition>,
    pub terminal: bool,
}

pub enum PhaseInstruction {
    Static(String),
    Dynamic(Arc<dyn Fn(&State) -> String + Send + Sync>),
}

pub struct Transition {
    pub target: String,
    pub guard: Arc<dyn Fn(&State) -> bool + Send + Sync>,
}

/// Phase machine: evaluates transitions, manages entry/exit lifecycle.
pub struct PhaseMachine {
    phases: HashMap<String, Phase>,
    current: String,
    initial: String,
    history: Vec<PhaseTransition>,
}

pub struct PhaseTransition {
    pub from: String,
    pub to: String,
    pub turn: u32,
    pub timestamp: Instant,
}

impl PhaseMachine {
    pub fn new(initial: &str) -> Self;
    pub fn add_phase(&mut self, phase: Phase);
    pub fn current(&self) -> &str;
    pub fn current_phase(&self) -> Option<&Phase>;
    pub fn history(&self) -> &[PhaseTransition];

    /// Evaluate transitions from current phase. Returns target if transition fires.
    /// Does NOT execute entry/exit — caller does that.
    pub fn evaluate(&self, state: &State) -> Option<&str>;

    /// Execute transition: on_exit(current), update current, on_enter(target).
    /// Returns the instruction for the new phase.
    pub async fn transition(
        &mut self,
        target: &str,
        state: &State,
        writer: &Arc<dyn SessionWriter>,
    ) -> Option<String>;

    /// Get active tools filter for current phase. None = all tools allowed.
    pub fn active_tools(&self) -> Option<&[String]>;

    /// Validate: all transition targets exist, initial phase exists.
    pub fn validate(&self) -> Result<(), String>;
}
```

**Concurrency**: The phase machine is owned by the control lane task. All
evaluation and transitions happen sequentially within the TurnComplete handler.
`on_enter`/`on_exit` are async — they can await I/O (DB lookups, API calls)
without blocking the fast lane.

**Tool filtering**: When `tools_enabled` is set, the processor injects a
`before_tool_call` check that rejects calls to tools not in the active set.
This happens before the user's `on_tool_call` callback. The model receives an
error response and learns which tools are available.

#### 2.2.4 NEW: `live/watcher.rs` — State Change Watchers

**Current**: Does not exist.

**New module**: ~250 lines.

```rust
pub enum WatchPredicate {
    Changed,
    ChangedTo(Value),
    ChangedFrom(Value),
    CrossedAbove(f64),
    CrossedBelow(f64),
    BecameTrue,
    BecameFalse,
    Custom(Arc<dyn Fn(&Value, &Value) -> bool + Send + Sync>),
}

pub struct Watcher {
    pub key: String,
    pub predicate: WatchPredicate,
    pub action: Arc<dyn Fn(Value, Value, State) -> BoxFuture<()> + Send + Sync>,
    pub blocking: bool,
}

pub struct WatcherRegistry {
    watchers: Vec<Watcher>,
    /// Keys that any watcher observes — used to scope snapshot/diff.
    observed_keys: HashSet<String>,
}

impl WatcherRegistry {
    pub fn new() -> Self;
    pub fn add(&mut self, watcher: Watcher);
    pub fn observed_keys(&self) -> &HashSet<String>;

    /// Evaluate all watchers given old/new state snapshots.
    /// Returns (blocking_actions, concurrent_actions).
    pub fn evaluate(
        &self,
        diffs: &[(String, Value, Value)],
        state: &State,
    ) -> (Vec<BoxFuture<()>>, Vec<BoxFuture<()>>);
}
```

**Performance**: `snapshot_values` before extractors, `diff_values` after
computed vars. Only keys that have registered watchers are snapshotted.
For 5-10 watched keys, snapshot + diff is < 100μs.

**Concurrency**: Blocking watchers are awaited sequentially on the control lane.
Concurrent watchers are `tokio::spawn`'d. The semaphore from the callback-mode
design applies to concurrent watcher tasks as well.

#### 2.2.5 NEW: `live/temporal.rs` — Temporal Pattern Detection

**Current**: Does not exist.

**New module**: ~300 lines.

```rust
pub trait PatternDetector: Send + Sync {
    fn check(&self, state: &State, event: Option<&SessionEvent>, now: Instant) -> bool;
    fn reset(&self);
}

pub struct TemporalPattern {
    pub name: String,
    pub detector: Box<dyn PatternDetector>,
    pub action: Arc<dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync>,
    pub cooldown: Option<Duration>,
    last_triggered: parking_lot::Mutex<Option<Instant>>,
}

pub struct TemporalRegistry {
    patterns: Vec<TemporalPattern>,
}

/// Sustained condition: true for duration.
pub struct SustainedDetector {
    condition: Arc<dyn Fn(&State) -> bool + Send + Sync>,
    duration: Duration,
    became_true_at: parking_lot::Mutex<Option<Instant>>,
}

/// Rate detector: N events in window.
pub struct RateDetector {
    filter: Arc<dyn Fn(&SessionEvent) -> bool + Send + Sync>,
    count: u32,
    window: Duration,
    timestamps: parking_lot::Mutex<VecDeque<Instant>>,
}

/// Turn count: condition true for N consecutive turns.
pub struct TurnCountDetector {
    condition: Arc<dyn Fn(&State) -> bool + Send + Sync>,
    required: u32,
    consecutive: AtomicU32,
}

/// Consecutive tool failures.
pub struct ConsecutiveFailureDetector {
    tool_name: String,
    threshold: u32,
    consecutive: AtomicU32,
}

impl TemporalRegistry {
    pub fn new() -> Self;
    pub fn add(&mut self, pattern: TemporalPattern);

    /// Check all patterns. Called on control lane events + timer.
    /// Returns actions to execute.
    pub fn check_all(
        &self,
        state: &State,
        event: Option<&SessionEvent>,
    ) -> Vec<BoxFuture<()>>;

    /// Returns true if any pattern needs periodic timer checks.
    pub fn needs_timer(&self) -> bool;
}
```

**Timer architecture**: If any sustained-condition pattern is registered, the
processor spawns a lightweight timer task:

```rust
// Inside spawn_event_processor, if temporal.needs_timer()
let timer_task = tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    loop {
        interval.tick().await;
        let actions = temporal.check_all(&state, None);
        for action in actions {
            tokio::spawn(action); // always concurrent
        }
    }
});
```

**Cost**: One 500ms interval timer. Each tick: iterate patterns (typically 1-5),
each check is an atomic read or DashMap lookup (< 1μs). Total: < 5μs per tick.
This is negligible.

#### 2.2.6 `live/callbacks.rs` — CallbackMode Integration

**Current**: 196 lines. All callbacks are bare Option<handler>.

**Delta**:

| Change | Type | Lines Est. |
|--------|------|-----------|
| Add `CallbackMode` enum | New type | +30 |
| Add `AsyncCallback<T>` type alias | New type | +5 |
| Change all callback fields to `Option<(handler, CallbackMode)>` | Modify | +40 (net) |
| Add forced-mode constants | New | +10 |
| Update Default impl | Modify | +10 |
| Update Debug impl | Modify | +10 |

**Total delta**: ~105 lines. **Breaking change** to EventCallbacks struct fields.
Mitigated by the fluent layer (L2) which is the primary public API.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CallbackMode {
    #[default]
    Blocking,
    Concurrent,
}

pub type AsyncCallback<T> = Arc<dyn Fn(T) -> BoxFuture<()> + Send + Sync>;

pub struct EventCallbacks {
    // Fast lane — default Concurrent
    pub on_audio: Option<(AsyncCallback<Bytes>, CallbackMode)>,
    pub on_text: Option<(AsyncCallback<String>, CallbackMode)>,
    // ... etc for all callbacks
    // Forced blocking (no mode field — always blocking)
    pub on_tool_call: Option<Arc<dyn Fn(Vec<FunctionCall>) -> BoxFuture<Option<Vec<FunctionResponse>>> + Send + Sync>>,
    pub before_tool_response: Option<Arc<dyn Fn(Vec<FunctionResponse>, State) -> BoxFuture<Vec<FunctionResponse>> + Send + Sync>>,
    pub on_turn_boundary: Option<Arc<dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync>>,
    pub instruction_template: Option<Arc<dyn Fn(&State) -> Option<String> + Send + Sync>>,
}
```

#### 2.2.7 `live/processor.rs` — The Central Orchestrator

**Current**: 947 lines. Two tasks (fast lane + control lane), hard-coded
sync/async split.

This is the **most critical file** in the delta. It becomes the orchestrator
for the entire evaluation pipeline.

**Delta**:

| Change | Type | Lines Est. |
|--------|------|-----------|
| Accept new registries (computed, phase, watchers, temporal) | Modify signature | +20 |
| Mode-aware dispatch in fast lane | Modify run_fast_lane | +40 |
| Mode-aware dispatch in control lane | Modify run_control_lane | +40 |
| Semaphore for concurrent tasks | New | +30 |
| Timeout guard for blocking fast-lane | New | +20 |
| Session signal auto-tracking | New block in route/dispatch | +60 |
| Turn-scoped state reset | New in TurnComplete | +10 |
| Expanded TurnComplete pipeline (8 steps) | Modify | +80 |
| Phase machine integration in TurnComplete | New | +40 |
| Watcher evaluation in TurnComplete | New | +30 |
| Temporal pattern check on every control event | New | +20 |
| Timer task spawn for sustained patterns | New | +25 |
| Tool call phase filtering | New in ToolCall handler | +20 |
| Background tool dispatch (ToolExecutionMode) | New in ToolCall handler | +60 |
| Background tool cancellation handling | Modify ToolCallCancelled | +20 |
| Interrupted transcript truncation | New in Interrupted handler | +15 |
| Server transcription routing (InputTranscription/OutputTranscription) | New fast-lane events | +20 |
| GoAway event routing + callback dispatch | New control-lane event | +15 |
| Session resumption update routing + handle persistence | New control-lane event | +20 |
| Server transcription integration in TurnComplete step 2 | Modify | +15 |

**Total delta**: ~600 lines. **Net processor size**: ~1550 lines.

**The expanded TurnComplete pipeline** (replacing the current ~60 lines):

```rust
// Inside run_control_lane, ControlEvent::TurnComplete arm:
async fn handle_turn_complete(
    state: &State,
    writer: &Arc<dyn SessionWriter>,
    transcript: &Option<Arc<Mutex<TranscriptBuffer>>>,
    extractors: &[Arc<dyn TurnExtractor>],
    computed: &Option<ComputedRegistry>,
    phase_machine: &Option<parking_lot::Mutex<PhaseMachine>>,
    watchers: &Option<WatcherRegistry>,
    temporal: &Option<TemporalRegistry>,
    callbacks: &EventCallbacks,
    session_event: &SessionEvent,
) {
    // 1. Reset turn-scoped state
    state.clear_prefix("turn:");

    // 2. Finalize transcript (prefer server-provided transcriptions when available)
    if let Some(buf) = transcript {
        let mut tb = buf.lock();
        // If server transcriptions are enabled, integrate them into the turn
        // record. Server ASR is more accurate than client-side reconstruction.
        if let Some(input_text) = state.session().get::<String>("last_input_transcription") {
            tb.set_input_transcription(&input_text);
        }
        if let Some(output_text) = state.session().get::<String>("last_output_transcription") {
            tb.set_output_transcription(&output_text);
        }
        tb.end_turn();
    }

    // 3. Snapshot watched keys (BEFORE extractors change state)
    let pre_snapshot = watchers.as_ref().map(|w| {
        state.snapshot_values(
            &w.observed_keys().iter().map(|s| s.as_str()).collect::<Vec<_>>()
        )
    });

    // 4. Run extractors (LLM calls — the expensive step)
    for extractor in extractors {
        let window = transcript.as_ref().map(|b| {
            b.lock().window(extractor.window_size()).to_vec()
        }).unwrap_or_default();
        match extractor.extract(&window).await {
            Ok(value) => {
                state.set(extractor.name(), &value);
                if let Some((cb, mode)) = &callbacks.on_extracted {
                    dispatch_callback(cb, (extractor.name().to_string(), value), *mode).await;
                }
            }
            Err(e) => tracing::warn!(extractor = extractor.name(), error = %e, "Extraction failed"),
        }
    }

    // 5. Recompute derived state (deterministic, < 1ms per var)
    if let Some(computed) = computed {
        computed.recompute(state);
    }

    // 6. Evaluate phase transitions (deterministic, < 1ms)
    if let Some(pm) = phase_machine {
        let mut machine = pm.lock();
        if let Some(target) = machine.evaluate(state).map(|s| s.to_string()) {
            let instruction = machine.transition(&target, state, writer).await;
            if let Some(inst) = instruction {
                writer.update_instruction(inst).await.ok();
                // Dedup handled inside PhaseMachine::transition
            }
            state.session().set("phase", machine.current());
            state.session().set("phase_history", &machine.history());
        }
    }

    // 7. Fire watchers (compare pre vs post snapshots)
    if let (Some(watchers), Some(pre)) = (&watchers, pre_snapshot) {
        let post_keys: Vec<&str> = watchers.observed_keys().iter().map(|s| s.as_str()).collect();
        let diffs = state.diff_values(&pre, &post_keys);
        let (blocking, concurrent) = watchers.evaluate(&diffs, state);
        // Blocking watchers: await sequentially
        for action in blocking {
            action.await;
        }
        // Concurrent watchers: spawn
        for action in concurrent {
            tokio::spawn(action);
        }
    }

    // 8. Check temporal patterns
    if let Some(temporal) = temporal {
        for action in temporal.check_all(state, Some(session_event)) {
            tokio::spawn(action);
        }
    }

    // 9. Instruction template (may override phase instruction)
    if let Some(template_fn) = &callbacks.instruction_template {
        if let Some(instruction) = template_fn(state) {
            // Dedup: only send if different from last
            let mut last = shared_state.last_instruction.lock();
            if last.as_deref() != Some(&instruction) {
                writer.update_instruction(&instruction).await.ok();
                *last = Some(instruction);
            }
        }
    }

    // 10. Turn boundary hook (context injection)
    if let Some(boundary) = &callbacks.on_turn_boundary {
        boundary(state.clone(), writer.clone()).await;
    }

    // 11. User turn-complete callback
    if let Some((cb, mode)) = &callbacks.on_turn_complete {
        dispatch_callback(cb, (), *mode).await;
    }

    // 12. Update session signals
    state.session().set("turn_count",
        state.session().get::<u32>("turn_count").unwrap_or(0) + 1);
}
```

**Interrupted handler** — transcript truncation (new):

When the server sends `interrupted = true`, all un-sent generation is discarded
server-side. The `TranscriptBuffer` must reflect this — the current model turn
is incomplete and should be truncated to match what was actually delivered.

```rust
// Inside run_control_lane (or fast lane), Interrupted handler:
SessionEvent::Interrupted => {
    // Truncate the current model turn — only what was already sent is retained
    if let Some(buf) = &transcript {
        buf.lock().truncate_current_model_turn();
    }
    // Existing: set interrupted flag, dispatch callback
    shared_state.interrupted.store(true, Ordering::Release);
    if let Some((cb, mode)) = &callbacks.on_interrupted {
        dispatch_callback(cb, (), *mode).await;
    }
}
```

This requires a new method on `TranscriptBuffer`:

```rust
impl TranscriptBuffer {
    /// Truncate the current model turn in progress. Called on interruption.
    /// The model's partial output that was already sent to the client is retained;
    /// any buffered-but-undelivered content is discarded.
    pub fn truncate_current_model_turn(&mut self) { /* ... */ }
}
```

**Mode-aware dispatch helper** (used by both lanes):

```rust
async fn dispatch_callback<T: Send + 'static>(
    cb: &AsyncCallback<T>,
    data: T,
    mode: CallbackMode,
) {
    match mode {
        CallbackMode::Blocking => {
            cb(data).await;
        }
        CallbackMode::Concurrent => {
            let cb = cb.clone();
            tokio::spawn(async move { cb(data).await; });
        }
    }
}
```

#### 2.2.8 `live/session_signals.rs` — Auto-Tracked Session State

**Current**: Does not exist. Developers track counters manually.

**New module**: ~180 lines.

```rust
/// Session type determines duration limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionType {
    AudioOnly,   // ~15 min limit
    AudioVideo,  // ~2 min limit
}

/// Tracks session-level signals automatically from events.
pub struct SessionSignals {
    state: State,
    connected_at: Option<Instant>,
    last_activity: parking_lot::Mutex<Instant>,
    session_type: SessionType,
    has_video: AtomicBool,
    go_away_at: parking_lot::Mutex<Option<Instant>>,
    /// Latest resumption handle from server (persisted for reconnection).
    latest_resume_handle: parking_lot::Mutex<Option<String>>,
}

impl SessionSignals {
    pub fn new(state: State) -> Self;

    /// Called by the processor on every event. Updates session.* state keys.
    pub fn on_event(&self, event: &SessionEvent) {
        match event {
            SessionEvent::Connected => {
                self.connected_at = Some(Instant::now());
                self.state.session().set("connected_at_ms", 0u64);
            }
            SessionEvent::VoiceActivityStart => {
                self.state.session().set("is_user_speaking", true);
                *self.last_activity.lock() = Instant::now();
            }
            SessionEvent::VoiceActivityEnd => {
                self.state.session().set("is_user_speaking", false);
            }
            SessionEvent::Interrupted => {
                let c: u32 = self.state.session().get("interrupt_count").unwrap_or(0);
                self.state.session().set("interrupt_count", c + 1);
            }
            SessionEvent::Error(_) => {
                let c: u32 = self.state.session().get("error_count").unwrap_or(0);
                self.state.session().set("error_count", c + 1);
            }
            SessionEvent::PhaseChanged(phase) => {
                self.state.session().set("is_model_speaking",
                    *phase == SessionPhase::ModelSpeaking);
            }

            // --- New: Session duration & GoAway ---
            SessionEvent::GoAway { time_left } => {
                *self.go_away_at.lock() = Some(Instant::now());
                self.state.session().set("go_away_time_left_ms", time_left.as_millis() as u64);
                self.state.session().set("go_away_received", true);
            }

            // --- New: Session resumption ---
            SessionEvent::SessionResumptionUpdate { new_handle, resumable, .. } => {
                *self.latest_resume_handle.lock() = Some(new_handle.clone());
                self.state.session().set("resumable", *resumable);
            }

            // --- New: Server transcriptions ---
            SessionEvent::InputTranscription { text } => {
                self.state.session().set("last_input_transcription", text.clone());
            }
            SessionEvent::OutputTranscription { text } => {
                self.state.session().set("last_output_transcription", text.clone());
            }

            _ => {}
        }

        // Track video usage → changes session type & duration limit
        if matches!(event, SessionEvent::VideoChunk(_)) {
            if !self.has_video.swap(true, Ordering::Relaxed) {
                self.state.session().set("session_type", "audio_video");
            }
        }

        // Update silence timer
        let silence = self.last_activity.lock().elapsed().as_millis() as u64;
        self.state.session().set("silence_ms", silence);

        // Update elapsed + remaining budget
        if let Some(at) = self.connected_at {
            let elapsed_ms = at.elapsed().as_millis() as u64;
            self.state.session().set("elapsed_ms", elapsed_ms);

            let limit_ms = match self.session_type() {
                SessionType::AudioOnly  => 15 * 60 * 1000, // 15 min
                SessionType::AudioVideo =>  2 * 60 * 1000, //  2 min
            };
            let remaining = limit_ms.saturating_sub(elapsed_ms);
            self.state.session().set("remaining_budget_ms", remaining);
        }
    }

    pub fn session_type(&self) -> SessionType {
        if self.has_video.load(Ordering::Relaxed) {
            SessionType::AudioVideo
        } else {
            SessionType::AudioOnly
        }
    }

    /// Returns the latest resumption handle for reconnection.
    pub fn latest_resume_handle(&self) -> Option<String> {
        self.latest_resume_handle.lock().clone()
    }
}
```

**Performance**: One DashMap write per event (~50ns). At 25 events/sec audio
rate, this is ~1.25μs/sec. Negligible.

**Placement**: Called in `route_event()` before dispatching to fast/control
lanes. This ensures signals are available to both lanes.

**Duration awareness**: The session automatically detects whether video has been
sent and adjusts `remaining_budget_ms` accordingly (15 min audio-only, 2 min
audio+video). Applications can use `remaining_budget_ms` in computed state,
phase transition guards, or temporal patterns to trigger graceful shutdown.

#### 2.2.9 `live/background_tool.rs` — Non-Blocking Tool Dispatch

**Current**: Does not exist. All tool calls are blocking in the control lane.

**New module**: ~200 lines.

```rust
pub trait ResultFormatter: Send + Sync + 'static {
    fn format_running(&self, call: &FunctionCall) -> Value;
    fn format_result(&self, call: &FunctionCall, result: Result<Value, ToolError>) -> Value;
    fn format_cancelled(&self, call_id: &str) -> Value;
}

pub struct DefaultResultFormatter;
impl ResultFormatter for DefaultResultFormatter { /* defaults */ }

#[derive(Clone, Default)]
pub enum ToolExecutionMode {
    #[default]
    Standard,
    Background {
        formatter: Option<Arc<dyn ResultFormatter>>,
    },
}

/// Tracks in-flight background tools for cancellation.
pub struct BackgroundToolTracker {
    tasks: DashMap<String, (JoinHandle<()>, CancellationToken)>,
}

impl BackgroundToolTracker {
    pub fn new() -> Self;
    pub fn spawn(&self, call_id: String, task: JoinHandle<()>, cancel: CancellationToken);
    pub fn cancel(&self, call_ids: &[String]);
    pub fn active_tool_names(&self) -> Vec<String>;
}
```

**Integration with processor**: In the ToolCall handler, after `on_tool_call`
returns None (no override):

```rust
match dispatcher.execution_mode(&call.name) {
    ToolExecutionMode::Standard => {
        // Current behavior: await dispatch, send response
        let result = dispatcher.call_function(&call.name, call.args.clone()).await;
        // ... send response
    }
    ToolExecutionMode::Background { formatter } => {
        // 1. Send ack immediately
        let fmt = formatter.unwrap_or_else(|| Arc::new(DefaultResultFormatter));
        let ack = FunctionResponse {
            name: call.name.clone(),
            response: fmt.format_running(&call),
            id: call.id.clone(),
        };
        writer.send_tool_response(vec![ack]).await.ok();

        // 2. Spawn background task
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let dispatcher = dispatcher.clone();
            let writer = writer.clone();
            let fmt = fmt.clone();
            let cancel = cancel.clone();
            async move {
                tokio::select! {
                    result = dispatcher.call_function(&call.name, call.args.clone()) => {
                        let response = FunctionResponse {
                            name: call.name.clone(),
                            response: fmt.format_result(&call, result),
                            id: call.id.clone(),
                        };
                        writer.send_tool_response(vec![response]).await.ok();
                    }
                    _ = cancel.cancelled() => {
                        // Cancelled by ToolCallCancelled event
                    }
                }
            }
        });
        tracker.spawn(call.id.clone().unwrap_or_default(), task, cancel);
        // 3. Control lane continues immediately
    }
}
```

#### 2.2.10 `tool.rs` — ToolDispatcher Enhancements

**Current**: 780 lines. No execution mode support, no state access.

**Delta**:

| Change | Type | Lines Est. |
|--------|------|-----------|
| Add `ToolExecutionMode` to tool registry (per-tool) | Modify | +30 |
| Add `register_function_with_mode()` | New method | +15 |
| Add `execution_mode(&self, name: &str) -> ToolExecutionMode` | New method | +10 |
| Add `StatefulToolFunction` trait | New trait | +20 |
| Add `register_stateful()` | New method | +15 |
| Modify `call_function` to pass State for stateful tools | Modify | +20 |

**Total delta**: ~110 lines. Existing methods unchanged — additive only.

```rust
#[async_trait]
pub trait StatefulToolFunction: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Option<Value>;
    async fn call(&self, args: Value, state: &State) -> Result<Value, ToolError>;
}
```

#### 2.2.11 `live/builder.rs` — LiveSessionBuilder Enhancements

**Current**: 108 lines. Accepts config, callbacks, dispatcher, extractors.

**Delta**:

| Change | Type | Lines Est. |
|--------|------|-----------|
| Accept ComputedRegistry | New field + setter | +15 |
| Accept PhaseMachine | New field + setter | +15 |
| Accept WatcherRegistry | New field + setter | +15 |
| Accept TemporalRegistry | New field + setter | +15 |
| Accept BackgroundToolTracker | New field | +5 |
| Pass all registries to spawn_event_processor | Modify connect() | +20 |
| Validate phase machine + computed graph at build time | New in connect() | +15 |
| Validate native audio model constraints (AUDIO-only modality) | New in connect() | +10 |
| Validate affective_dialog/proactive_audio require native model | New in connect() | +10 |

**Total delta**: ~120 lines.

**Build-time validations** (in `connect()`, before WebSocket opens):

```rust
// Native audio model validation
if config.model.contains("native-audio") {
    if config.response_modalities.contains(&Modality::Text) {
        return Err(SetupError::InvalidConfig(
            "Native audio models only support AUDIO output modality, not TEXT".into()
        ));
    }
}

// Feature/model compatibility
if config.enable_affective_dialog && !config.model.contains("native-audio") {
    tracing::warn!("enable_affective_dialog requires a native audio model");
}
if config.proactive_audio && !config.model.contains("native-audio") {
    tracing::warn!("proactive_audio requires a native audio model");
}
```

#### 2.2.12 `live/handle.rs` — LiveHandle Enhancements

**Current**: 103 lines.

**Delta**: +30 lines for new accessors.

```rust
impl LiveHandle {
    // Existing methods unchanged

    // New: phase machine access
    pub fn current_phase(&self) -> Option<String> {
        self.state.session().get("phase")
    }

    // New: background tool tracking
    pub fn active_background_tools(&self) -> Vec<String> {
        self.tracker.active_tool_names()
    }
}
```

#### 2.2.13 `live/mod.rs` — Module Exports

**Current**: 15 lines.

**Delta**: +10 lines for new module declarations and re-exports.

```rust
pub mod background_tool;
pub mod computed;
pub mod phase;
pub mod session_signals;
pub mod temporal;
pub mod watcher;
```

#### 2.2.14 `middleware.rs` — Enhanced Middleware Trait

**Current**: 614 lines. 8 hook methods.

**Delta**: ~100 lines for new hooks. **All new hooks have default empty impls**
— existing middleware implementations continue to compile.

```rust
// Added to Middleware trait (all with default no-op impls):
async fn on_dispatch(&self, _ctx: &InvocationContext, _task: &str) -> DispatchDirective {
    DispatchDirective::Continue
}
async fn on_task_complete(&self, _ctx: &InvocationContext, _task: &str, _result: &Value) {}
async fn on_loop_iteration(&self, _ctx: &InvocationContext, _name: &str, _iter: u32) -> LoopDirective {
    LoopDirective::Continue
}
async fn on_phase_transition(&self, _ctx: &InvocationContext, _from: &str, _to: &str) {}
async fn on_session_event(&self, _ctx: &InvocationContext, _event: &SessionEvent) {}

// New directive enums:
pub enum DispatchDirective { Continue, Cancel }
pub enum LoopDirective { Continue, Break }
```

### 2.3 gemini-adk-fluent-rs (L2) — The Fluent Surface

L2 is primarily additive — new builder methods and types. No existing methods
change.

#### 2.3.1 `live.rs` — Live Builder Extensions

**Current**: ~350 lines, ~50 methods.

**Delta**: ~500 lines for new methods.

```rust
impl Live {
    // --- API configuration (from gemini-genai-api-behavior) ---
    fn vad(self, f: impl FnOnce(VadBuilder) -> VadBuilder) -> Self;
    fn affective_dialog(self) -> Self;
    fn proactive_audio(self) -> Self;
    fn media_resolution(self, resolution: MediaResolution) -> Self;
    fn context_compression(self, f: impl FnOnce(CompressionBuilder) -> CompressionBuilder) -> Self;
    fn session_resumption(self, f: impl FnOnce(ResumptionBuilder) -> ResumptionBuilder) -> Self;
    fn transcribe_input(self) -> Self;
    fn transcribe_output(self) -> Self;
    fn on_go_away(self, f: impl AsyncFn(Duration)) -> Self;
    fn on_resumption_update(self, f: impl AsyncFn(String)) -> Self;  // receives new_handle
    fn on_input_transcription(self, f: impl AsyncFn(String)) -> Self;
    fn on_output_transcription(self, f: impl AsyncFn(String)) -> Self;

    // --- CallbackMode variants (from callback-mode-design) ---
    fn on_audio_blocking(self, f: impl AsyncFn(&Bytes)) -> Self;
    fn on_text_blocking(self, f: impl AsyncFn(&str)) -> Self;
    fn on_turn_complete_concurrent(self, f: impl AsyncFn()) -> Self;
    fn on_connected_concurrent(self, f: impl AsyncFn()) -> Self;
    fn on_error_concurrent(self, f: impl AsyncFn(String)) -> Self;
    fn on_extracted_blocking(self, f: impl AsyncFn(String, Value)) -> Self;
    // ... etc for all non-forced callbacks

    // --- Computed state (from voice-native design) ---
    fn computed(self, key: &str, deps: &[&str], f: impl Fn(&State) -> Option<Value>) -> Self;

    // --- Phase machine (from voice-native design) ---
    fn phase(self, name: &str) -> PhaseBuilder;
    fn initial_phase(self, name: &str) -> Self;

    // --- Watchers (from voice-native design) ---
    fn watch(self, key: &str) -> WatchBuilder;

    // --- Temporal patterns (from voice-native design) ---
    fn when_sustained(self, name: &str, condition: impl Fn(&State) -> bool, duration: Duration) -> TemporalBuilder;
    fn when_rate(self, name: &str, filter: impl Fn(&SessionEvent) -> bool, count: u32, window: Duration) -> TemporalBuilder;
    fn when_turns(self, name: &str, condition: impl Fn(&State) -> bool, turn_count: u32) -> TemporalBuilder;
    fn when_consecutive_failures(self, name: &str, tool_name: &str, count: u32) -> TemporalBuilder;

    // --- Background tools (from callback-mode design) ---
    fn tool_behavior(self, behavior: FunctionCallingBehavior) -> Self;
    fn tool_background(self, name: &str, formatter: impl ResultFormatter) -> Self;

    // --- Stateful tools (from fluent-devex design) ---
    fn tool_with_state(self, name: &str, desc: &str, f: impl Fn(Value, State) -> BoxFuture<Result<Value, ToolError>>) -> Self;

    // --- Agent as tool (from fluent-devex design) ---
    fn agent_tool(self, agent: AgentBuilder, llm: Arc<dyn BaseLlm>) -> Self;
    fn agent_tool_background(self, agent: AgentBuilder, llm: Arc<dyn BaseLlm>) -> Self;

    // --- Live hooks (from fluent-devex design) ---
    fn on_extracted_dispatch(self, entries: impl IntoIterator<Item = (&str, AgentBuilder)>) -> Self;
    fn on_extracted_pipeline(self, pipeline: Composable, llm: Arc<dyn BaseLlm>) -> Self;
    fn hook(self, trigger: LiveTrigger, pipeline: Composable, mode: CallbackMode) -> Self;
}
```

**API config sub-builders** (in `live_builders.rs`):

```rust
pub struct VadBuilder {
    pub start_sensitivity: Option<Sensitivity>,
    pub end_sensitivity: Option<Sensitivity>,
    pub prefix_padding_ms: Option<u32>,
    pub silence_duration_ms: Option<u32>,
    pub voice_activity_timeout: Option<Duration>,
    pub disabled: bool,
}

pub struct CompressionBuilder {
    pub trigger_tokens: u32,
    pub target_tokens: u32,
}

pub struct ResumptionBuilder {
    pub transparent: bool,
    pub on_handle: Option<Arc<dyn Fn(String) + Send + Sync>>,
}
```

#### 2.3.2 NEW: `live_builders.rs` — Sub-Builders for Live

**New file**: ~300 lines.

```rust
/// Builder for a single conversation phase.
pub struct PhaseBuilder { /* wraps Phase, returns Live */ }

/// Builder for a phase transition.
pub struct TransitionBuilder { /* wraps Transition, returns PhaseBuilder */ }

/// Builder for a state watcher.
pub struct WatchBuilder { /* wraps key, returns WatchActionBuilder */ }

/// Builder for watcher action after predicate.
pub struct WatchActionBuilder { /* wraps Watcher, returns Live */ }

/// Builder for a temporal pattern.
pub struct TemporalBuilder { /* wraps TemporalPattern, returns Live */ }

/// Builder for VAD configuration (from API behavior study).
pub struct VadBuilder { /* see Section 2.3.1 */ }

/// Builder for context window compression.
pub struct CompressionBuilder { /* trigger_tokens, target_tokens */ }

/// Builder for session resumption.
pub struct ResumptionBuilder { /* transparent, on_handle callback */ }
```

Each sub-builder accumulates configuration and returns the parent `Live` builder
when completed, maintaining the fluent chain.

#### 2.3.3 `operators.rs` — New Composable Variants

**Current**: ~280 lines. 5 variants in Composable enum.

**Delta**: ~150 lines for new variants.

```rust
pub enum Composable {
    Agent(AgentBuilder),
    Pipeline(Pipeline),
    FanOut(FanOut),
    Loop(Loop),
    Fallback(Fallback),
    Step(FnStep),           // NEW
    Route(RouteBuilder),    // NEW
    Gate(GateBuilder),      // NEW
    Dispatch(DispatchNode), // NEW
    Join(JoinNode),         // NEW
}

pub struct FnStep {
    pub name: String,
    pub handler: Arc<dyn Fn(&mut State) -> BoxFuture<Result<(), AgentError>> + Send + Sync>,
}

pub struct RouteBuilder {
    pub key: String,
    pub branches: Vec<(RoutePredicate, Composable)>,
    pub default: Option<Box<Composable>>,
}

pub struct GateBuilder {
    pub predicate: Arc<dyn Fn(&State) -> bool + Send + Sync>,
    pub then_branch: Box<Composable>,
    pub otherwise_branch: Option<Box<Composable>>,
}

pub struct DispatchNode {
    pub agents: Vec<(String, Composable)>,
    pub max_concurrent: usize,
}

pub struct JoinNode {
    pub names: Vec<String>,
    pub timeout: Option<Duration>,
}
```

#### 2.3.4 `builder.rs` — AgentBuilder Extensions

**Current**: ~350 lines.

**Delta**: ~100 lines for per-agent callback stacks.

```rust
impl AgentBuilder {
    fn before_run(self, f: impl AsyncFn(&State)) -> Self;
    fn after_run(self, f: impl AsyncFn(&State, &str)) -> Self;
    fn before_tool(self, f: impl AsyncFn(&FunctionCall, &State)) -> Self;
    fn after_tool(self, f: impl AsyncFn(&FunctionCall, &Value, &State)) -> Self;
    fn on_error(self, f: impl AsyncFn(&AgentError, &State) -> Option<String>) -> Self;
    fn guard(self, f: impl Fn(&State) -> bool) -> Self;
    fn prompt(self, composite: PromptComposite) -> Self;
    fn preset(self, preset: Preset) -> Self;
}

pub struct Preset {
    pub before_run: Vec<AsyncCallback<State>>,
    pub after_run: Vec<AsyncCallback<(State, String)>>,
    pub before_tool: Vec<AsyncCallback<(FunctionCall, State)>>,
    pub after_tool: Vec<AsyncCallback<(FunctionCall, Value, State)>>,
}
```

#### 2.3.5 `compose/tools.rs` — Tool Composition Extensions

**Current**: ~120 lines.

**Delta**: ~60 lines.

```rust
impl T {
    // Existing methods unchanged
    fn stateful(name: &str, description: &str, handler: impl Fn(Value, State) -> BoxFuture<Result<Value, ToolError>>) -> ToolComposite;
    fn agent(agent: AgentBuilder, llm: Arc<dyn BaseLlm>) -> ToolComposite;
    fn agent_background(agent: AgentBuilder, llm: Arc<dyn BaseLlm>) -> ToolComposite;
}
```

#### 2.3.6 `compose/middleware.rs` — Middleware Extensions

**Current**: ~300 lines.

**Delta**: ~50 lines.

```rust
impl M {
    // Existing methods unchanged
    fn scope(predicate: impl Fn(&str) -> bool, middleware: MiddlewareComposite) -> MiddlewareComposite;
    fn when(condition: impl Fn() -> bool, middleware: MiddlewareComposite) -> MiddlewareComposite;
}
```

---

## 3. Module Interplay — The Dependency Graph

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                          gemini-adk-fluent-rs (L2)                                 │
│                                                                             │
│  Live ─── builds ──→ LiveSessionBuilder (L1)                               │
│   │                                                                         │
│   ├── PhaseBuilder ────→ PhaseMachine (L1)                                 │
│   ├── .computed() ─────→ ComputedRegistry (L1)                             │
│   ├── WatchBuilder ────→ WatcherRegistry (L1)                              │
│   ├── TemporalBuilder ─→ TemporalRegistry (L1)                            │
│   ├── .tool_background ─→ BackgroundToolTracker (L1) + ToolExecutionMode   │
│   ├── .tool_with_state ─→ StatefulToolFunction (L1)                        │
│   ├── .agent_tool ──────→ AgentTool (L1)                                   │
│   ├── ._blocking/concurrent ─→ CallbackMode (L1)                          │
│   │                                                                         │
│   │  Composable extensions:                                                │
│   ├── FnStep ──────→ compiles to FnTextAgent (L1)                          │
│   ├── RouteBuilder ─→ compiles to RouteTextAgent (L1)                      │
│   ├── DispatchNode ─→ compiles to DispatchTextAgent (L1)                   │
│   └── JoinNode ────→ compiles to JoinTextAgent (L1)                        │
│                                                                             │
│  AgentBuilder                                                              │
│   ├── .before_run/after_run → stored as callback stack, wired at .build()  │
│   └── .prompt(P::...) → rendered to instruction string                     │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                             gemini-adk-rs (L1)                                     │
│                                                                             │
│  LiveSessionBuilder                                                        │
│   │ holds:                                                                  │
│   ├── EventCallbacks (with CallbackMode)                                   │
│   ├── ToolDispatcher (with ToolExecutionMode per tool)                     │
│   ├── Vec<TurnExtractor>                                                   │
│   ├── ComputedRegistry ─────────────────────────────┐                      │
│   ├── PhaseMachine ──────────────────────────────────┤                     │
│   ├── WatcherRegistry ──────────────────────────────┤                     │
│   ├── TemporalRegistry ─────────────────────────────┤                     │
│   └── BackgroundToolTracker                          │                     │
│                                                      │                     │
│   .connect() → validates, then calls:               │                     │
│                                                      ▼                     │
│  spawn_event_processor() ◄──────── orchestrates ALL of these               │
│   │                                                                         │
│   ├── Router task: broadcast::Receiver → route to fast/ctrl                │
│   │    └── SessionSignals.on_event() — auto-track counters                 │
│   │                                                                         │
│   ├── Fast Lane task:                                                      │
│   │    ├── Audio/Text/Transcript/VAD/Phase callbacks                       │
│   │    ├── CallbackMode::Blocking → .await with timeout guard              │
│   │    └── CallbackMode::Concurrent → call inline (sync) or spawn          │
│   │                                                                         │
│   ├── Control Lane task:                                                   │
│   │    ├── ToolCall handler                                                │
│   │    │    ├── Phase tool filter (reject disallowed tools)                │
│   │    │    ├── on_tool_call callback (forced Blocking)                    │
│   │    │    ├── ToolExecutionMode::Standard → await dispatch               │
│   │    │    └── ToolExecutionMode::Background → ack + spawn                │
│   │    │                                                                    │
│   │    ├── Interrupted handler                                            │
│   │    │    └── TranscriptBuffer.truncate_current_model_turn()            │
│   │    │                                                                    │
│   │    ├── TurnComplete handler (THE PIPELINE)                             │
│   │    │    ├── 1. clear_prefix("turn:")                                   │
│   │    │    ├── 2. Integrate server transcriptions + end_turn()            │
│   │    │    ├── 3. Snapshot watched keys                                   │
│   │    │    ├── 4. Extractors (LLM calls)                                  │
│   │    │    ├── 5. ComputedRegistry.recompute()                            │
│   │    │    ├── 6. PhaseMachine.evaluate() + transition()                  │
│   │    │    ├── 7. WatcherRegistry.evaluate()                              │
│   │    │    ├── 8. TemporalRegistry.check_all()                            │
│   │    │    ├── 9. instruction_template()                                  │
│   │    │    ├── 10. on_turn_boundary()                                     │
│   │    │    ├── 11. on_turn_complete callback                              │
│   │    │    └── 12. Update session signals (turn_count)                    │
│   │    │                                                                    │
│   │    ├── ToolCallCancelled → BackgroundToolTracker.cancel()              │
│   │    ├── GoAway → on_go_away callback, surface time_left                │
│   │    ├── SessionResumptionUpdate → persist handle, on_resumption_update │
│   │    └── Lifecycle callbacks (Connected, Disconnected, Error)            │
│   │                                                                         │
│   └── Timer task (if temporal patterns need periodic checks):              │
│        └── 500ms interval → TemporalRegistry.check_all()                  │
│                                                                             │
│  State ◄──── read/written by ALL of the above                              │
│   ├── session:* — written by SessionSignals                                │
│   ├── turn:* — reset on TurnComplete                                       │
│   ├── app:* — written by extractors, tools, user callbacks                 │
│   ├── bg:* — written by background agents/tools                            │
│   ├── derived:* — written by ComputedRegistry (read-only to user code)    │
│   └── (no prefix) — backward-compatible flat access                        │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                             gemini-genai-rs (L0)                                   │
│                     WIRE PROTOCOL ADDITIONS ONLY                            │
│                                                                             │
│  broadcast::Sender<SessionEvent> ─── events flow UP to L1 processor        │
│  mpsc::Sender<SessionCommand> ────── commands flow DOWN from L1/L2         │
│  SessionHandle (SessionWriter) ───── L1 writes via Arc<dyn SessionWriter>  │
│  SessionPhase ────────────────────── L1 reads via session.phase()           │
│                                                                             │
│  NEW events emitted:                                                        │
│  ├── GoAway { time_left }                                                  │
│  ├── SessionResumptionUpdate { session_id, resumable, new_handle }         │
│  ├── InputTranscription { text }                                            │
│  └── OutputTranscription { text }                                           │
│                                                                             │
│  NEW config fields serialized in setup:                                     │
│  ├── ContextWindowCompression, SessionResumption                            │
│  ├── ActivityDetection (VAD), MediaResolution, Sensitivity                 │
│  └── enable_affective_dialog, proactive_audio, transcription flags         │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 4. Scale, Performance & Concurrency Architecture

### 4.1 The Concurrency Model

The system has **five concurrent execution contexts**:

```
┌──────────────────────────────────────────────────────────────────┐
│                    Tokio Runtime                                  │
│                                                                  │
│  Task 1: gemini-genai-rs Connection Loop                                │
│           tokio::select! { recv from ws, recv from command_rx }  │
│           Zero application logic. Wire only.                     │
│           Hot path: base64 encode/decode, JSON parse             │
│                                                                  │
│  Task 2: Event Router                                            │
│           recv from broadcast → classify → send to fast/ctrl     │
│           Plus: SessionSignals.on_event()                        │
│           Hot path: match on SessionEvent variant (~10ns)        │
│                                                                  │
│  Task 3: Fast Lane Consumer                                     │
│           recv from fast_tx → dispatch sync callbacks            │
│           Hot path: audio callback (~5μs for PCM copy)          │
│           NEVER awaits anything except blocking fast callbacks   │
│                                                                  │
│  Task 4: Control Lane Consumer                                  │
│           recv from ctrl_tx → dispatch async callbacks           │
│           Owns: TurnComplete pipeline, tool dispatch             │
│           May await: extractors (1-5s), phase on_enter (<1s)    │
│                                                                  │
│  Task 5: Timer (optional, if temporal patterns registered)       │
│           500ms interval → check sustained conditions            │
│           Hot path: 1-5 atomic reads (~10ns each)               │
│                                                                  │
│  Spawned tasks (dynamic):                                        │
│  - Concurrent callbacks (tokio::spawn per invocation)            │
│  - Background tool executions                                    │
│  - Background agent dispatches                                   │
│                                                                  │
└──────────────────────────────────────────────────────────────────┘
```

### 4.2 Critical Path Analysis

The **critical path** for audio latency is:

```
Audio arrives from Gemini
  → L0 connection loop: recv + decode (~200μs)
  → broadcast::send (~50ns)
  → Router: classify as FastEvent::Audio (~10ns)
  → fast_tx.send (~50ns)
  → Fast Lane: check interrupted flag (~5ns)
  → Fast Lane: call on_audio callback (~5μs for typical PCM handling)
  → Audio reaches speaker

Total overhead: < 300μs (well within 40ms audio frame budget)
```

**Nothing in the delta touches this path.** Session signals, computed state,
phase machine, watchers, temporal patterns — all execute on the **control lane**
or timer, never on the fast lane.

The only delta that touches the fast lane is `CallbackMode::Blocking` for audio,
which is an explicit opt-in that the developer chooses with full awareness of
the timing implications.

### 4.3 Data Structure Choices for Performance

| Component | Data Structure | Why |
|-----------|---------------|-----|
| State (inner) | `DashMap<String, Value>` | Lock-free concurrent reads, sharded writes. Fast lane reads don't block control lane writes. |
| State (delta) | `DashMap<String, Value>` | Same as inner. Delta tracking is opt-in. |
| ComputedRegistry (vars) | `Vec<ComputedVar>` | Topologically sorted once at build time. Iteration is cache-friendly. |
| ComputedRegistry (dep_index) | `HashMap<String, Vec<usize>>` | O(1) lookup of affected vars by changed key. |
| PhaseMachine (phases) | `HashMap<String, Phase>` | O(1) lookup by name. Transitions are Vec (small, evaluated linearly). |
| PhaseMachine (history) | `Vec<PhaseTransition>` | Append-only, read by watchers/templates. |
| WatcherRegistry (watchers) | `Vec<Watcher>` | Small (5-10 watchers typical). Linear scan is fastest for this size. |
| WatcherRegistry (observed_keys) | `HashSet<String>` | O(1) membership check for snapshot scoping. |
| TemporalRegistry (patterns) | `Vec<TemporalPattern>` | Small (1-5 patterns). Linear scan. |
| RateDetector (timestamps) | `VecDeque<Instant>` | Sliding window. O(1) push_back + pop_front. |
| BackgroundToolTracker (tasks) | `DashMap<String, (JoinHandle, CancellationToken)>` | Concurrent read/write from control lane + spawned tasks. |
| SessionSignals (last_activity) | `parking_lot::Mutex<Instant>` | Single writer (router), rare reader (temporal). Minimal contention. |
| SharedState (interrupted) | `AtomicBool` | Zero-cost reads from fast lane. Single writer from control lane. |

### 4.4 Memory Budget

| Component | Per-Session Memory | Notes |
|-----------|-------------------|-------|
| State (DashMap) | ~1-10 KB | 20-50 keys typical, each key ~50-200 bytes |
| ComputedRegistry | ~500 bytes | 3-10 vars, each ~50 bytes (pointer + key) |
| PhaseMachine | ~2-5 KB | 3-8 phases, each ~500 bytes (instruction string dominates) |
| WatcherRegistry | ~500 bytes | 5-10 watchers, each ~50 bytes |
| TemporalRegistry | ~1 KB | 1-5 patterns, rate detector holds VecDeque (~200 bytes) |
| TranscriptBuffer | ~10-50 KB | Grows with conversation length. 50 turns × 200 chars = 10KB |
| BackgroundToolTracker | ~200 bytes | 0-3 concurrent background tools |
| SessionSignals | ~100 bytes | Fixed-size counters |
| **Total per session** | **~15-70 KB** | Negligible for a WebSocket session consuming MB/s of audio |

### 4.5 Contention Analysis

The key question: do the new modules introduce lock contention that could
stall the fast lane?

```
                    FAST LANE                    CONTROL LANE
                    (Task 3)                     (Task 4)
                    ─────────                    ────────────
State reads:        DashMap.get() ─── lock-free   DashMap.get() ─── lock-free
State writes:       (never)                       DashMap.insert() ── sharded lock
interrupted flag:   AtomicBool.load() ─ no lock   AtomicBool.store() ─ no lock
TranscriptBuffer:   Mutex.lock() ─ per push       Mutex.lock() ─ per end_turn
SessionSignals:     (never)                       DashMap writes ── sharded
ComputedRegistry:   (never)                       Sequential iteration
PhaseMachine:       (never)                       Mutex.lock() once per TurnComplete
WatcherRegistry:    (never)                       Sequential evaluation
TemporalRegistry:   (never)                       Sequential check
```

**Result**: The fast lane **never acquires a lock** on any new module. It only
reads from DashMap (lock-free) and AtomicBool (lock-free). The single contention
point is `TranscriptBuffer`, which already exists and uses `parking_lot::Mutex`
(< 1μs contention on uncontested lock).

The control lane holds exclusive access to computed, phase, watcher, and
temporal registries. No other task touches them. Zero contention.

### 4.6 Backpressure & Overflow Protection

| Concern | Protection | Mechanism |
|---------|-----------|-----------|
| Concurrent callback accumulation | Semaphore | `MAX_CONCURRENT_TASKS = 64`. `try_acquire()` per spawn. Drop event on failure + log. |
| Background tool accumulation | Task tracking | `BackgroundToolTracker` limits to active tools. ToolCallCancelled cleans up. |
| Broadcast buffer overflow | Lagged reader handling | `recv_event()` in L0 skips lagged events with warning. |
| Control lane queue overflow | Bounded channel | `ctrl_tx` is bounded(64). Router drops if full + logs. |
| Timer task under pressure | Interval skip | `tokio::time::interval` skips missed ticks (no accumulation). |
| Watcher cascades | No re-trigger | Watchers evaluate on pre/post diff. Changes from watchers are visible on **next** TurnComplete, not immediately. No infinite loops. |
| Computed var cascades | Topological order | Single pass through sorted list. Each var computed once. No re-evaluation within same pass. |

### 4.7 Cancellation Safety

Every async operation must handle cancellation gracefully:

| Operation | Cancellation Signal | Behavior |
|-----------|-------------------|----------|
| Extractor LLM call | Session disconnect | `tokio::select! { extract.await, disconnect_rx }`. Partial state write is fine — next turn overwrites. |
| Background tool | `CancellationToken` | `tokio::select! { tool.await, cancel.cancelled() }`. No result sent. |
| Phase on_enter/on_exit | None (short-lived) | Expected to complete in < 1s. If session disconnects, task is dropped. |
| Blocking watcher | None (short-lived) | Same as phase callbacks. |
| Concurrent callback | None (fire-and-forget) | Spawned task runs to completion or panics (caught by tokio). |

### 4.8 Thread Safety Proof

All types that cross task boundaries are `Send + Sync`:

| Type | Send | Sync | How |
|------|------|------|-----|
| State | Yes | Yes | `Arc<DashMap>` (DashMap is Send + Sync) |
| ComputedRegistry | Yes | Yes | `Arc<closure>` is Send + Sync (closure is `Fn + Send + Sync`) |
| PhaseMachine | Yes | No* | Wrapped in `parking_lot::Mutex` for shared access |
| WatcherRegistry | Yes | Yes | `Arc<closure>` closures, immutable after build |
| TemporalRegistry | Yes | Yes* | Internal mutability via atomics and parking_lot::Mutex |
| BackgroundToolTracker | Yes | Yes | DashMap for tasks |
| SessionSignals | Yes | Yes | State + parking_lot::Mutex<Instant> |

*PhaseMachine is mutable (current phase changes). It's wrapped in
`parking_lot::Mutex` and only accessed from the control lane task. The Mutex
is a safety measure, not a contention concern — it's never contested.

---

## 5. Implementation Order & Dependencies

### Phase 0: Wire Protocol (L0, No Dependencies)

Must be completed before any L1/L2 work that depends on new events.

```
┌─────────────────────────────┐  ┌──────────────────────────┐
│  SessionConfig additions    │  │  Server message parsing  │
│  • context_compression      │  │  • sessionResumptionUpdate│
│  • session_resumption       │  │  • inputTranscription    │
│  • input/output_transcription│  │  • outputTranscription   │
│  • enable_affective_dialog  │  │  • goAway.timeLeft       │
│  • media_resolution         │  │                          │
│  • realtime_input_config    │  │  (codec.rs delta)        │
│  • proactive_audio          │  │                          │
│                             │  └──────────────────────────┘
│  (types.rs + config delta)  │
└─────────────────────────────┘
          │                               │
          ▼                               ▼
┌──────────────────────────────────────────────────────────────┐
│  New SessionEvent variants                                    │
│  • GoAway { time_left }                                      │
│  • SessionResumptionUpdate { session_id, resumable, handle } │
│  • InputTranscription { text }                               │
│  • OutputTranscription { text }                              │
│                                                              │
│  (session/mod.rs or events.rs delta)                         │
└──────────────────────────────────────────────────────────────┘
```

**Deliverable**: L0 can parse all known server messages. SessionConfig
serializes all setup fields. No application logic added.

### Phase 1: Foundation (Depends on Phase 0)

These can be built in parallel:

```
┌─────────────────────────────┐  ┌──────────────────────────┐
│  State enhancements         │  │  CallbackMode enum       │
│  • session/turn/bg/derived  │  │  • AsyncCallback<T>      │
│  • snapshot_values          │  │  • Forced mode constants  │
│  • diff_values              │  │  • Update EventCallbacks  │
│  • clear_prefix             │  │                          │
│  • ReadOnlyPrefixedState    │  │  (callbacks.rs)          │
│                             │  │                          │
│  (state.rs)                 │  └──────────────────────────┘
└─────────────────────────────┘
          │                               │
          ▼                               ▼
┌─────────────────────────────┐  ┌──────────────────────────┐
│  SessionSignals             │  │  Mode-aware dispatch     │
│  • Auto-track counters      │  │  • dispatch_callback()   │
│  • Duration awareness       │  │  • Timeout guard         │
│  • GoAway / resumption      │  │  • Semaphore             │
│  • Server transcriptions    │  │                          │
│  • remaining_budget_ms      │  │  (processor.rs delta)    │
│  (session_signals.rs)       │  │                          │
└─────────────────────────────┘  └──────────────────────────┘
```

**Deliverable**: Session signals auto-populate (including duration tracking,
GoAway countdown, resumption handles). Callbacks support Blocking/Concurrent
modes. The existing fluent API works unchanged (sync callbacks treated as
Concurrent).

### Phase 2: Computed State

Depends on Phase 1 (state namespacing for `derived:` prefix).

```
┌─────────────────────────────┐
│  ComputedRegistry           │
│  • register()               │
│  • recompute()              │
│  • validate()               │
│  • Topo-sort at build       │
│                             │
│  (computed.rs)              │
└─────────────────────────────┘
          │
          ▼
┌─────────────────────────────┐  ┌──────────────────────────┐
│  Wire into processor        │  │  Live::computed()        │
│  • Step 5 in TurnComplete   │  │  (live.rs delta)         │
│  (processor.rs delta)       │  │                          │
└─────────────────────────────┘  └──────────────────────────┘
```

### Phase 3: Phase Machine

Depends on Phase 2 (computed state may feed phase transition guards).

```
┌─────────────────────────────┐
│  PhaseMachine               │
│  • Phase, Transition        │
│  • evaluate(), transition() │
│  • active_tools()           │
│  • validate()               │
│                             │
│  (phase.rs)                 │
└─────────────────────────────┘
          │
          ▼
┌─────────────────────────────┐  ┌──────────────────────────┐
│  Wire into processor        │  │  PhaseBuilder            │
│  • Step 6 in TurnComplete   │  │  TransitionBuilder       │
│  • Tool call phase filter   │  │  (live_builders.rs)      │
│  (processor.rs delta)       │  │  + Live::phase()         │
└─────────────────────────────┘  └──────────────────────────┘
```

### Phase 4: Watchers

Depends on Phase 1 (snapshot/diff methods) and Phase 2 (computed state
triggers watcher evaluation).

```
┌─────────────────────────────┐
│  WatcherRegistry            │
│  • Watcher, WatchPredicate  │
│  • evaluate()               │
│                             │
│  (watcher.rs)               │
└─────────────────────────────┘
          │
          ▼
┌─────────────────────────────┐  ┌──────────────────────────┐
│  Wire into processor        │  │  WatchBuilder            │
│  • Step 7 in TurnComplete   │  │  WatchActionBuilder      │
│  (processor.rs delta)       │  │  (live_builders.rs)      │
└─────────────────────────────┘  └──────────────────────────┘
```

### Phase 5: Temporal Patterns

Depends on Phase 1 (session signals for silence_ms, interrupt_count etc).

```
┌─────────────────────────────┐
│  TemporalRegistry           │
│  • PatternDetector trait     │
│  • SustainedDetector        │
│  • RateDetector             │
│  • TurnCountDetector        │
│  • ConsecutiveFailureDetector│
│                             │
│  (temporal.rs)              │
└─────────────────────────────┘
          │
          ▼
┌─────────────────────────────┐  ┌──────────────────────────┐
│  Wire into processor        │  │  TemporalBuilder         │
│  • Step 8 in TurnComplete   │  │  + Live::when_*()        │
│  • Timer task spawn         │  │  (live_builders.rs)      │
│  (processor.rs delta)       │  │                          │
└─────────────────────────────┘  └──────────────────────────┘
```

### Phase 6: Background Tools

Independent of Phases 2-5. Can be built in parallel.

```
┌─────────────────────────────┐  ┌──────────────────────────┐
│  BackgroundToolTracker      │  │  ResultFormatter trait    │
│  • spawn(), cancel()        │  │  ToolExecutionMode enum  │
│  • active_tool_names()      │  │  DefaultResultFormatter  │
│                             │  │                          │
│  (background_tool.rs)       │  │  (background_tool.rs)    │
└─────────────────────────────┘  └──────────────────────────┘
          │                               │
          ▼                               ▼
┌─────────────────────────────┐  ┌──────────────────────────┐
│  ToolDispatcher delta       │  │  Processor ToolCall delta │
│  • register_with_mode()     │  │  • Standard vs Background│
│  • execution_mode()         │  │  • Ack + spawn           │
│  • StatefulToolFunction     │  │  • Cancellation wiring   │
│  (tool.rs delta)            │  │  (processor.rs delta)    │
└─────────────────────────────┘  └──────────────────────────┘
```

### Phase 7: Composition Primitives

Independent of Phases 2-6. Can be built in parallel.

```
┌─────────────────────────────┐  ┌──────────────────────────┐
│  FnStep, RouteBuilder       │  │  DispatchNode, JoinNode  │
│  GateBuilder                │  │  TaskRegistry (L1)       │
│  (operators.rs delta)       │  │  (operators.rs + L1)     │
└─────────────────────────────┘  └──────────────────────────┘
          │                               │
          ▼                               ▼
┌──────────────────────────────────────────────────────────────┐
│  Per-agent callback stacks                                   │
│  Preset bundles                                              │
│  .prompt() integration                                       │
│  (builder.rs delta)                                          │
└──────────────────────────────────────────────────────────────┘
```

### Phase 8: Live Bridge

Depends on Phases 6 and 7 (uses background tools + composition primitives).

```
┌─────────────────────────────┐
│  LiveHook / LiveTrigger     │
│  on_extracted_pipeline()    │
│  on_extracted_dispatch()    │
│  agent_tool()               │
│  tool_background_pipeline() │
│  (live.rs + live_builders)  │
└─────────────────────────────┘
```

---

## 6. Summary — Lines of Code Delta

| Module | File | Change Type | Estimated Lines |
|--------|------|-------------|----------------|
| **gemini-genai-rs (L0)** | | | |
| protocol/types.rs | Existing | Modify | +80 |
| session/mod.rs (or events) | Existing | Modify | +40 |
| transport/codec.rs | Existing | Modify | +50 |
| session/config.rs | Existing | Modify | +30 |
| **L0 Total** | | | **~200** |
| **gemini-adk-rs (L1)** | | | |
| state.rs | Existing | Modify | +100 |
| live/computed.rs | New | Create | ~200 |
| live/phase.rs | New | Create | ~350 |
| live/watcher.rs | New | Create | ~250 |
| live/temporal.rs | New | Create | ~300 |
| live/session_signals.rs | New | Create | ~180 |
| live/background_tool.rs | New | Create | ~200 |
| live/callbacks.rs | Existing | Modify | +105 |
| live/processor.rs | Existing | Modify | +600 |
| live/builder.rs | Existing | Modify | +120 |
| live/handle.rs | Existing | Modify | +30 |
| live/mod.rs | Existing | Modify | +10 |
| tool.rs | Existing | Modify | +110 |
| middleware.rs | Existing | Modify | +100 |
| transcript.rs | Existing | Modify | +45 |
| **L1 Total** | | | **~2,700** |
| **gemini-adk-fluent-rs (L2)** | | | |
| live.rs | Existing | Modify | +500 |
| live_builders.rs | New | Create | ~300 |
| operators.rs | Existing | Modify | +150 |
| builder.rs | Existing | Modify | +100 |
| compose/tools.rs | Existing | Modify | +60 |
| compose/middleware.rs | Existing | Modify | +50 |
| **L2 Total** | | | **~1,160** |
| | | | |
| **Grand Total** | | | **~4,060 lines** |

### New Files Created

| File | Crate | Purpose |
|------|-------|---------|
| `crates/gemini-adk-rs/src/live/computed.rs` | L1 | ComputedVar, ComputedRegistry, topological sort |
| `crates/gemini-adk-rs/src/live/phase.rs` | L1 | Phase, PhaseMachine, PhaseTransition |
| `crates/gemini-adk-rs/src/live/watcher.rs` | L1 | Watcher, WatchPredicate, WatcherRegistry |
| `crates/gemini-adk-rs/src/live/temporal.rs` | L1 | PatternDetector, TemporalPattern, TemporalRegistry, detectors |
| `crates/gemini-adk-rs/src/live/session_signals.rs` | L1 | SessionSignals, auto-tracking, duration awareness, resumption |
| `crates/gemini-adk-rs/src/live/background_tool.rs` | L1 | ResultFormatter, ToolExecutionMode, BackgroundToolTracker |
| `crates/gemini-adk-fluent-rs/src/live_builders.rs` | L2 | PhaseBuilder, WatchBuilder, TemporalBuilder, VadBuilder, CompressionBuilder, ResumptionBuilder |

### Existing Files Modified

| File | Crate | Nature of Change |
|------|-------|-----------------|
| `crates/gemini-genai-rs/src/protocol/types.rs` | L0 | New config types: ContextWindowCompression, SessionResumption, ActivityDetection, MediaResolution, Sensitivity |
| `crates/gemini-genai-rs/src/session/mod.rs` | L0 | New SessionEvent variants: GoAway, SessionResumptionUpdate, InputTranscription, OutputTranscription |
| `crates/gemini-genai-rs/src/transport/codec.rs` | L0 | Parse sessionResumptionUpdate, inputTranscription, outputTranscription from server messages |
| `crates/gemini-adk-rs/src/state.rs` | L1 | Add prefix accessors, snapshot/diff methods |
| `crates/gemini-adk-rs/src/live/callbacks.rs` | L1 | Add CallbackMode, change field types, new callbacks for GoAway/resumption/transcription |
| `crates/gemini-adk-rs/src/live/processor.rs` | L1 | Mode-aware dispatch, expanded TurnComplete pipeline, interrupted transcript truncation, new event routing |
| `crates/gemini-adk-rs/src/live/builder.rs` | L1 | Accept new registries, validate at build time, native audio model checks |
| `crates/gemini-adk-rs/src/live/handle.rs` | L1 | New accessors |
| `crates/gemini-adk-rs/src/live/mod.rs` | L1 | New module declarations |
| `crates/gemini-adk-rs/src/tool.rs` | L1 | StatefulToolFunction, ToolExecutionMode, register_with_mode |
| `crates/gemini-adk-rs/src/middleware.rs` | L1 | New hooks with default impls |
| `crates/gemini-adk-rs/src/transcript.rs` | L1 | `set_input_transcription()`, `set_output_transcription()`, `truncate_current_model_turn()` |
| `crates/gemini-adk-fluent-rs/src/live.rs` | L2 | ~40 new builder methods (API config + callbacks + state/phase/temporal) |
| `crates/gemini-adk-fluent-rs/src/operators.rs` | L2 | New Composable variants |
| `crates/gemini-adk-fluent-rs/src/builder.rs` | L2 | Per-agent callbacks, preset, prompt |
| `crates/gemini-adk-fluent-rs/src/compose/tools.rs` | L2 | T::stateful, T::agent |
| `crates/gemini-adk-fluent-rs/src/compose/middleware.rs` | L2 | M::scope, M::when |

---

## 7. Design Invariants — What Must Always Hold

1. **The fast lane never blocks on any new module.** Session signals, computed
   state, phase machine, watchers, temporal patterns — all execute on the
   control lane or timer task. The audio path remains < 300μs.

2. **The TurnComplete pipeline is sequential within the control lane.** Steps
   3-12 execute in strict order on a single task. No concurrent access to
   computed, phase, or watcher registries during evaluation.

3. **Background tasks cannot write to `derived:*` state.** Only the
   ComputedRegistry writes to derived state, and it runs on the control lane.
   Background agents write to `bg:*` or unprefixed keys.

4. **Watcher evaluation is non-reentrant.** Watchers fire based on pre/post
   snapshot diff. State changes made by watchers are visible on the **next**
   TurnComplete, not the current one. This prevents infinite loops.

5. **Phase transitions are atomic within a turn.** Evaluate → on_exit →
   update phase → on_enter → update instruction. No other TurnComplete step
   runs between these.

6. **Existing code compiles without changes.** All new callback fields have
   defaults. Sync fast-lane callbacks are wrapped as `Concurrent` mode
   automatically. The Live builder's existing methods continue to work.

7. **Validation happens at build time, not runtime.** ComputedRegistry cycle
   detection, PhaseMachine target validation, WatcherRegistry key validation,
   and native audio model compatibility checks all run in `.connect()` before
   the WebSocket opens. Runtime code assumes valid configuration.

8. **L0 remains logic-free.** The wire layer parses and emits — it never
   interprets. New L0 types (ContextWindowCompression, SessionResumption,
   ActivityDetection) are data structures serialized into the setup message
   and deserialized from server responses. No branching logic, no state
   management, no application decisions.

9. **Transcript integrity tracks actual delivery.** On interruption, the
   `TranscriptBuffer` truncates the current model turn to match what was
   actually sent to the client. Server-side discarded generation never
   appears in the transcript. When server transcriptions are enabled, they
   take priority over client-side text reconstruction.

10. **Tool declarations are immutable post-setup.** Phase-based tool filtering
    operates at the runtime level by rejecting disallowed calls with error
    responses — it never attempts to modify the wire-level tool declarations.
    To change tools, start a new session.
