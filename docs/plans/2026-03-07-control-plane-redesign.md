# Control Plane Redesign: From Railroad to Guardrails

**Date**: 2026-03-07
**Scope**: L0 (rs-genai), L1 (rs-adk), L2 (adk-rs-fluent)
**Philosophy**: Let the model lead. Let the SDK observe and occasionally intervene.

---

## Guiding Principle

Every change in this plan follows one rule: **minimize the altitude of intervention**. The Gemini Live API is a model-led conversation runtime. The SDK's job is to be an intelligent observer that fires circuit breakers — not a railroad that forces the conversation onto tracks.

Where Dialogflow says "the flow is the product," we say "the model's conversational intelligence is the product, and the SDK is guardrails plus instrumentation."

---

## Phase 0: Protocol Surface (L0 — rs-genai)

These changes expose raw protocol capabilities that every higher layer needs. No behavioral changes, just visibility.

### 0.1 Surface `generationComplete` as a SessionEvent

**File**: `crates/rs-genai/src/session/mod.rs`

The wire protocol already parses `generation_complete` in `ServerContentPayload` (messages.rs:238), but `connection.rs:403-412` only handles `turn_complete`. We need a new event variant.

```rust
// session/mod.rs — add variant
pub enum SessionEvent {
    // ... existing variants ...
    GenerationComplete,      // NEW: model finished generating (even if interrupted)
    // ... rest ...
}
```

**File**: `crates/rs-genai/src/transport/connection.rs` (~line 403)

```rust
// Handle generation complete — fires BEFORE turn_complete
if content.generation_complete.unwrap_or(false) {
    let _ = event_tx.send(SessionEvent::GenerationComplete);
}

// Handle turn complete (existing)
if content.turn_complete.unwrap_or(false) {
    // ... existing logic ...
}
```

**Why this ordering matters**: `generationComplete` fires when the model's internal generation pipeline stops. This can happen *before* `turnComplete` (normal case) or *without* `turnComplete` (interrupted case). Surfacing it as a separate event lets the control lane distinguish "model finished thinking" from "it's the user's turn now."

**Effort**: ~20 lines across 2 files. Zero breaking changes.

---

### 0.2 Surface `UsageMetadata` for Context Horizon Tracking

The server sends `UsageMetadata` with `total_token_count` on responses. This is the only signal available to infer context window state.

**File**: `crates/rs-genai/src/protocol/messages.rs`

```rust
// Already parsed but not surfaced. Add to ServerContentPayload handling:
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageMetadata {
    pub total_token_count: Option<u32>,
    pub prompt_token_count: Option<u32>,
    pub candidates_token_count: Option<u32>,
    pub cached_content_token_count: Option<u32>,
}
```

**File**: `crates/rs-genai/src/session/mod.rs`

```rust
pub enum SessionEvent {
    // ... existing ...
    UsageMetadata(UsageMetadata),  // NEW: token usage from server
}
```

**File**: `crates/rs-genai/src/transport/connection.rs`

Surface the usage metadata when present in server content messages.

**Why**: This is the foundation for Gap 1 (context horizon tracking). Without knowing token counts, we can't estimate when the server's sliding window will prune history.

**Effort**: ~40 lines across 3 files.

---

### 0.3 Expose `SessionResumptionUpdate.last_consumed_client_message_index`

Already parsed in messages.rs:319-340 but not forwarded. This field tells us which client messages the server has processed — critical for Gap 8 (session resumption state reconciliation).

**File**: `crates/rs-genai/src/session/mod.rs`

```rust
pub enum SessionEvent {
    // Change existing:
    SessionResumeHandle(ResumeInfo),  // was SessionResumeHandle(String)
}

#[derive(Debug, Clone)]
pub struct ResumeInfo {
    pub handle: String,
    pub resumable: bool,
    pub last_consumed_index: Option<String>,
}
```

**Effort**: ~30 lines. Minor breaking change on `SessionResumeHandle` variant (handle in L1 builder).

---

## Phase 1: Context Horizon Awareness (Gap 1)

### The Problem

TranscriptBuffer (50-turn ring buffer, client-side) diverges from the server's actual context window after compression. Phase guards and extractors operate on stale transcript data.

### 1.1 `ContextHorizon` Tracker (L1)

**New file**: `crates/rs-adk/src/live/context_horizon.rs`

```rust
/// Tracks the server's context window state based on token usage signals.
pub struct ContextHorizon {
    /// Compression config from session setup
    trigger_tokens: u32,
    target_tokens: u32,
    /// Latest token count from server UsageMetadata
    last_token_count: AtomicU32,
    /// Estimated oldest surviving turn number
    estimated_horizon: AtomicU32,
    /// Turn number → estimated cumulative token count
    turn_token_estimates: parking_lot::Mutex<VecDeque<(u32, u32)>>,
    /// Keys that were set during turns that may have been pruned
    compression_durable_keys: parking_lot::RwLock<HashSet<String>>,
}
```

**Core logic**:

```rust
impl ContextHorizon {
    /// Called when UsageMetadata arrives (telemetry lane → state)
    pub fn update_token_count(&self, total: u32, current_turn: u32) {
        self.last_token_count.store(total, Ordering::Relaxed);
        // Record (turn, token_count) pair
        // If total > trigger_tokens, estimate which turns were pruned
        // Move estimated_horizon forward
    }

    /// Has a specific turn likely been pruned from server context?
    pub fn is_turn_pruned(&self, turn_number: u32) -> bool {
        turn_number < self.estimated_horizon.load(Ordering::Relaxed)
    }

    /// Register a state key as compression-durable.
    /// When the turn that set this key is pruned, the SDK re-injects
    /// the value via send_client_content as a synthetic context turn.
    pub fn mark_durable(&self, key: &str) {
        self.compression_durable_keys.write().insert(key.to_string());
    }

    /// Returns keys that need re-injection because their origin turn was pruned.
    pub fn pruned_durable_keys(&self, state: &State) -> Vec<(String, Value)> {
        // Check which durable keys were set in turns before the horizon
        // Return (key, current_value) pairs for re-injection
    }
}
```

**Integration point** — `handle_turn_complete()` step 6.5 (new, between transcript window and phase eval):

```rust
// Step 6.5: Context horizon maintenance
if let Some(ref horizon) = self.context_horizon {
    let durable = horizon.pruned_durable_keys(&state);
    if !durable.is_empty() {
        // Re-inject as synthetic context turn
        let parts: Vec<Part> = durable.iter()
            .map(|(k, v)| Part::text(format!("[Persistent context: {}={}]", k, v)))
            .collect();
        let content = Content::from_parts(Role::User, parts);
        writer.send_client_content(vec![content], false).await?;
    }
}
```

### 1.2 Fluent API (L2)

```rust
Live::builder()
    .context_compression(trigger_tokens, target_tokens)  // existing
    .durable_keys(&["verified", "customer_id", "risk_level"])  // NEW
```

**How `.durable_keys()` works**: Registers keys with `ContextHorizon::mark_durable()`. When the server prunes turns that established these keys, the SDK automatically re-injects their current values as synthetic context turns via `send_client_content`. The model never loses awareness of critical state, even in hour-long conversations.

### 1.3 Tradeoff Acknowledgment

- Token estimation is heuristic (we don't have exact per-turn counts from the server). We use average tokens per turn as an estimator, calibrated by periodic `UsageMetadata` signals.
- Re-injection consumes context window tokens. But these are small (key=value pairs), and the alternative — the model contradicting itself about verified state — is worse.
- Developers must explicitly mark keys as durable. This is intentional: it forces thought about what truly needs to survive compression.

**Effort**: ~200 lines new code (context_horizon.rs), ~40 lines integration (processor.rs, live.rs).

---

## Phase 2: Generation Complete Distinction (Gap 2)

### The Problem

`turnComplete` and `generationComplete` are collapsed into one event. Extractors can't access the model's full intended output when interrupted.

### 2.1 `ExtractorTrigger` Enum (L1)

**File**: `crates/rs-adk/src/live/extractor.rs`

```rust
#[derive(Debug, Clone, Copy, Default)]
pub enum ExtractorTrigger {
    #[default]
    OnTurnComplete,
    OnGenerationComplete,
}

#[async_trait]
pub trait TurnExtractor: Send + Sync {
    fn name(&self) -> &str;
    fn window_size(&self) -> usize;
    fn trigger(&self) -> ExtractorTrigger { ExtractorTrigger::OnTurnComplete }  // NEW
    fn should_extract(&self, window: &[TranscriptTurn]) -> bool { true }
    async fn extract(&self, window: &[TranscriptTurn]) -> Result<Value, LlmError>;
}
```

### 2.2 Dual-Trigger Extraction in Processor

**File**: `crates/rs-adk/src/live/processor.rs`

In the control lane event loop, handle `GenerationComplete` as a lightweight extraction point:

```rust
ControlEvent::GenerationComplete => {
    // Run ONLY extractors with trigger == OnGenerationComplete
    // Use current (pre-truncation) transcript — the model's full output
    let window = transcript_buffer.snapshot_window_with_current();
    let gen_extractors: Vec<_> = extractors.iter()
        .filter(|e| matches!(e.trigger(), ExtractorTrigger::OnGenerationComplete))
        .collect();
    if !gen_extractors.is_empty() {
        run_extractors(&gen_extractors, &window, &state, &callbacks).await;
    }
}
```

The key insight: `GenerationComplete` fires *before* `Interrupted` truncates the model's output. So extractors triggered on generation-complete see the full intended response, while turn-complete extractors see the post-truncation version.

### 2.3 `snapshot_window_with_current()` on TranscriptBuffer

**File**: `crates/rs-adk/src/live/transcript.rs`

```rust
/// Returns a window that includes the current in-progress turn (not yet finalized).
/// Used by GenerationComplete extractors to see the model's full output before truncation.
pub fn snapshot_window_with_current(&mut self, n: usize) -> TranscriptWindow {
    let mut turns: Vec<TranscriptTurn> = self.window(n).to_vec();
    if self.has_pending() {
        turns.push(TranscriptTurn {
            turn_number: self.turn_count,
            user: self.current_user.clone(),
            model: self.current_model.clone(),
            tool_calls: self.tool_calls_pending.clone(),
            timestamp: Instant::now(),
        });
    }
    TranscriptWindow::new(turns)
}
```

### 2.4 Fluent API (L2)

```rust
Live::builder()
    .extract_turns::<SentimentState>(llm, "Extract sentiment")  // default: OnTurnComplete
    .extract_on_generation::<FullIntent>(llm, "Extract model's full intent")  // NEW: OnGenerationComplete
```

**Effort**: ~80 lines new code, ~30 lines integration. Non-breaking (default trigger is `OnTurnComplete`).

---

## Phase 3: Proactive Silence Awareness (Gap 3)

### The Problem

When `proactiveAudio` is enabled, the model may choose not to respond. No `TurnComplete` fires. The 17-step pipeline never runs. State freezes.

### 3.1 Soft Turn Detection (L1)

**File**: `crates/rs-adk/src/live/processor.rs`

Add a `SoftTurnDetector` that watches for VAD-end without a subsequent model response:

```rust
struct SoftTurnDetector {
    vad_ended_at: Option<Instant>,
    timeout: Duration,  // default 2s, configurable
}

impl SoftTurnDetector {
    fn on_vad_end(&mut self) {
        self.vad_ended_at = Some(Instant::now());
    }

    fn on_model_response(&mut self) {
        self.vad_ended_at = None;  // Model responded, no soft turn needed
    }

    fn check(&self, now: Instant) -> bool {
        self.vad_ended_at
            .map(|t| now.duration_since(t) >= self.timeout)
            .unwrap_or(false)
    }
}
```

**Integration**: The timer task (currently 500ms for temporal patterns) also checks the soft turn detector:

```rust
// In timer tick handler
if soft_turn_detector.check(Instant::now()) {
    soft_turn_detector.reset();
    handle_soft_turn(&mut transcript_buffer, &state, &extractors,
                     &computed, &phase_machine, &watchers, &temporal,
                     &callbacks, &writer).await;
}
```

### 3.2 `handle_soft_turn()` — Lightweight Pipeline

A soft turn runs a **subset** of the 17-step pipeline:

```rust
async fn handle_soft_turn(/* ... */) {
    // Step 1: Clear turn-scoped state
    state.clear_prefix("turn:");

    // Step 2: Finalize transcript (user input only — no model output)
    transcript_buffer.end_turn();

    // Step 3-4: Snapshot + Run extractors (the user said something worth extracting)
    // ... same as full pipeline ...

    // Step 5: Recompute derived state
    // Step 6: Build transcript window

    // Step 7: Evaluate phase transitions
    // CRITICAL DIFFERENCE: Do NOT send instruction update.
    // The model chose silence; respect that. But DO update state.

    // Step 8-9: Watchers + temporal patterns (state may have changed)

    // Steps 10-13: SKIP instruction updates, context injection, prompt_on_enter
    // The model is intentionally silent. Don't force it to speak.

    // Step 14: Turn boundary hook (developer can decide to inject context)
    // Step 15-16: Callbacks + turn count
}
```

**The key design decision**: Soft turns update state and fire watchers, but do NOT send instruction updates or prompt the model. The model chose silence; the SDK respects that choice. But developers get state updates so they can decide whether to intervene via the turn boundary hook.

### 3.3 Fluent API (L2)

```rust
Live::builder()
    .proactive_audio(true)
    .soft_turn_timeout(Duration::from_secs(2))  // NEW: default 2s
```

### 3.4 Tradeoff

- The 2-second timeout is a heuristic. Too short → spurious soft turns on slow model responses. Too long → delayed state updates during genuine silence.
- The timeout should be calibrated against the session's typical response latency. A future enhancement could auto-calibrate from `SessionTelemetry::avg_response_latency_ms`.
- Soft turns are opt-in: they only activate when `proactive_audio(true)` is set.

**Effort**: ~150 lines (SoftTurnDetector + handle_soft_turn), ~20 lines L2.

---

## Phase 4: Context Injection as Primary Steering (Gap 4)

### The Problem

Instruction updates are a blunt instrument. They replace the entire system prompt and cause the model to re-process context. The SDK should prefer `send_client_content` for tactical steering.

### 4.1 `SteeringMode` — Two Tiers (L1)

**File**: `crates/rs-adk/src/live/phase.rs`

```rust
/// How the phase machine steers the model's behavior.
#[derive(Debug, Clone, Copy, Default)]
pub enum SteeringMode {
    /// Replace system instruction on phase transition. Use for major persona/goal changes.
    #[default]
    InstructionUpdate,
    /// Inject steering context via send_client_content. Lighter weight, works WITH
    /// the model's conversational intelligence instead of overriding it.
    ContextInjection,
    /// Hybrid: instruction update on phase transition, context injection on every turn.
    Hybrid,
}
```

### 4.2 Context Steering in `handle_turn_complete()`

When `SteeringMode::ContextInjection` or `Hybrid`:

```rust
// Step 7.5 (new): Context-based steering
if matches!(steering_mode, SteeringMode::ContextInjection | SteeringMode::Hybrid) {
    let steering_parts = build_steering_context(&state, &phase, &modifiers);
    if !steering_parts.is_empty() {
        let content = Content::model(steering_parts.join("\n"));
        writer.send_client_content(vec![content], false).await?;
    }
}
```

The `build_steering_context()` function converts `InstructionModifier`s into conversational steering:

```rust
fn build_steering_context(state: &State, phase: &Phase, modifiers: &[InstructionModifier]) -> Vec<String> {
    let mut parts = Vec::new();
    for modifier in modifiers {
        match modifier {
            InstructionModifier::StateAppend(keys) => {
                // Instead of "[Context: key=val]" in instruction,
                // produce: "I note that key is currently val."
                let pairs = resolve_state_pairs(state, keys);
                if !pairs.is_empty() {
                    parts.push(format!("Current context: {}", pairs.join(", ")));
                }
            }
            InstructionModifier::Conditional { predicate, text } => {
                if predicate(state) {
                    parts.push(text.clone());
                }
            }
            InstructionModifier::CustomAppend(f) => {
                let text = f(state);
                if !text.is_empty() {
                    parts.push(text);
                }
            }
        }
    }
    parts
}
```

### 4.3 Fluent API (L2)

```rust
Live::builder()
    .steering_mode(SteeringMode::ContextInjection)  // NEW
    .phase("greeting")
        .instruction("Welcome the user")  // Only sent on transition with InstructionUpdate/Hybrid
        .with_state(&["customer_name"])   // Injected as context turn with ContextInjection
        .done()
```

### 4.4 When to Use Which Mode

| Mode | When | Tradeoff |
|------|------|----------|
| `InstructionUpdate` (default) | Major persona shifts. Switching from "greeter" to "troubleshooter". | Latency spike on transition. Model re-processes context. |
| `ContextInjection` | Same persona, different focus. "Now discuss billing" within same agent role. | Consumes context window tokens. Pruned by compression. |
| `Hybrid` | Complex flows. Instruction sets the persona, context steers focus per turn. | Both costs. Best for regulated flows with compliance requirements. |

**Effort**: ~100 lines (SteeringMode + build_steering_context), ~20 lines L2.

---

## Phase 5: Tool Availability Signaling (Gap 6)

### The Problem

Per-phase tool filtering is client-side enforcement. The model doesn't know tools are unavailable until it tries to call one and gets an error.

### 5.1 Proactive Tool Signaling via Context Injection

**File**: `crates/rs-adk/src/live/processor.rs`

In the phase transition path (step 7), after a transition with tool set changes:

```rust
// Step 7.1 (new): Signal tool availability change
if let Some(ref result) = transition_result {
    let new_tools = phase_machine.lock().await.active_tools();
    let prev_tools = state.session().get::<Vec<String>>("active_tools");

    if new_tools != prev_tools.as_deref() {
        // Update state for tracking
        if let Some(tools) = new_tools {
            state.session().set("active_tools", tools.to_vec());
            // Inject advisory context
            let tool_names = tools.join(", ");
            let advisory = Content::model(
                format!("In this phase, I have access to these tools: {}. \
                         I should only use these tools.", tool_names)
            );
            writer.send_client_content(vec![advisory], false).await?;
        }
    }
}
```

### 5.2 Why Advisory, Not Instruction

Putting tool availability in the instruction ("You have access to: X, Y") has two problems:
1. It makes the instruction longer on every phase, consuming persistent context
2. It can conflict with the model's training about tool use

Instead, we inject it as a model-role content turn. The model "remembers" that it decided to only use certain tools. This is more natural and works with the model's conversational intelligence.

The existing client-side enforcement (reject + error response) remains as a safety net. The advisory reduces the frequency of rejections from "every phase transition" to "rare edge case."

### 5.3 Fluent API

No API changes needed. This is automatic when `tools_enabled` is set on a phase and a transition occurs. Opt-out via:

```rust
Live::builder()
    .tool_advisory(false)  // NEW: disable proactive signaling (default: true)
```

**Effort**: ~40 lines in processor.rs, ~10 lines L2.

---

## Phase 6: Conversation Repair Protocol (Gap 7)

### The Problem

Phase `needs` are informational. The SDK can't detect when the conversation is stuck — the model isn't gathering required information, and no one intervenes.

### 6.1 `NeedsFulfillment` Tracker (L1)

**File**: `crates/rs-adk/src/live/needs.rs` (new)

```rust
pub struct NeedsFulfillment {
    /// Phase name → unfulfilled need keys
    unfulfilled: HashMap<String, Vec<String>>,
    /// Phase name → consecutive turns without progress
    stall_count: HashMap<String, u32>,
    /// Configurable thresholds
    nudge_after: u32,     // turns before first nudge (default: 3)
    escalate_after: u32,  // turns before escalation (default: 6)
}

impl NeedsFulfillment {
    /// Called after extractors run. Returns the repair action to take.
    pub fn evaluate(
        &mut self,
        phase: &str,
        needs: &[String],
        state: &State,
    ) -> RepairAction {
        let unfulfilled: Vec<_> = needs.iter()
            .filter(|key| state.get_raw(key).is_none())
            .cloned()
            .collect();

        if unfulfilled.is_empty() {
            self.stall_count.remove(phase);
            return RepairAction::None;
        }

        let count = self.stall_count.entry(phase.to_string()).or_insert(0);
        *count += 1;

        if *count >= self.escalate_after {
            RepairAction::Escalate { unfulfilled }
        } else if *count >= self.nudge_after {
            RepairAction::Nudge { unfulfilled, attempt: *count - self.nudge_after + 1 }
        } else {
            RepairAction::None
        }
    }

    pub fn reset(&mut self, phase: &str) {
        self.stall_count.remove(phase);
    }
}

pub enum RepairAction {
    None,
    Nudge { unfulfilled: Vec<String>, attempt: u32 },
    Escalate { unfulfilled: Vec<String> },
}
```

### 6.2 Integration in `handle_turn_complete()`

After step 7 (phase evaluation), add step 7.2:

```rust
// Step 7.2: Conversation repair check
if let Some(ref mut needs_tracker) = self.needs_fulfillment {
    let current_phase = phase_machine.lock().await;
    if let Some(phase) = current_phase.current_phase() {
        if !phase.needs.is_empty() {
            match needs_tracker.evaluate(current_phase.current(), &phase.needs, &state) {
                RepairAction::Nudge { unfulfilled, attempt } => {
                    let nudge = Content::model(format!(
                        "I still need to collect: {}. Let me ask about these.",
                        unfulfilled.join(", ")
                    ));
                    writer.send_client_content(vec![nudge], false).await?;
                    // Optionally prompt on first nudge
                    if attempt == 1 {
                        writer.send_client_content(vec![], true).await?;
                    }
                }
                RepairAction::Escalate { unfulfilled } => {
                    // Set state flag for phase guard to pick up
                    state.set("repair:escalation", true);
                    state.set("repair:unfulfilled", unfulfilled);
                    // Re-evaluate phase transitions — an escalation guard may fire
                }
                RepairAction::None => {}
            }
        }
    }
}
```

### 6.3 Fluent API (L2)

```rust
Live::builder()
    .repair(RepairConfig::default())  // Enable with defaults (nudge at 3, escalate at 6)
    .repair(RepairConfig::new().nudge_after(2).escalate_after(5))  // Custom
    .phase("gather_info")
        .needs(&["customer_id", "account_number"])  // existing
        .transition("escalation", S::is_true("repair:escalation"))  // developer-defined escape
        .done()
```

### 6.4 Why Gentle Nudging

The nudge is a model-role context injection, not an instruction replacement. The model "remembers" that it decided to ask about the missing information. This works with its conversational intelligence — it will find a natural way to ask, not a robotic "Please provide your customer ID."

Escalation sets a state flag rather than forcing a transition directly. This keeps the phase machine as the single source of truth for transitions. The developer defines an escalation phase with a guard that checks `repair:escalation` — the repair system doesn't presume where to escalate to.

**Effort**: ~120 lines (needs.rs), ~40 lines integration.

---

## Phase 7: Session Persistence (Gap 8)

### The Problem

Client-side state (State, PhaseMachine position, transcript summary) doesn't survive process restarts. The Gemini session is resumable, but the SDK's control plane is lost.

### 7.1 `SessionPersistence` Trait (L1)

**File**: `crates/rs-adk/src/live/persistence.rs` (new)

```rust
/// Serializable snapshot of the control plane state.
#[derive(Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// All state key-value pairs (DashMap → HashMap serialization)
    pub state: HashMap<String, Value>,
    /// Current phase name
    pub phase: String,
    /// Turn count
    pub turn_count: u32,
    /// Summary of recent transcript (not full transcript — closures aren't serializable)
    pub transcript_summary: String,
    /// Resume handle from the Gemini server
    pub resume_handle: Option<String>,
    /// Timestamp
    pub saved_at: chrono::DateTime<chrono::Utc>,
}

/// Trait for persisting session state across process restarts.
#[async_trait]
pub trait SessionPersistence: Send + Sync {
    async fn save(&self, session_id: &str, snapshot: &SessionSnapshot) -> Result<(), Box<dyn std::error::Error>>;
    async fn load(&self, session_id: &str) -> Result<Option<SessionSnapshot>, Box<dyn std::error::Error>>;
    async fn delete(&self, session_id: &str) -> Result<(), Box<dyn std::error::Error>>;
}
```

### 7.2 Built-in Implementations

```rust
/// File-system persistence (good for development)
pub struct FsPersistence {
    dir: PathBuf,
}

/// In-memory persistence (good for tests)
pub struct MemoryPersistence {
    store: Arc<DashMap<String, SessionSnapshot>>,
}
```

### 7.3 Integration in `handle_turn_complete()`

After step 16 (turn count increment), add step 17:

```rust
// Step 17: Persist session state (if configured)
if let Some(ref persistence) = self.persistence {
    let snapshot = SessionSnapshot {
        state: state.to_hashmap(),
        phase: phase_machine.lock().await.current().to_string(),
        turn_count: state.session().get::<u32>("turn_count").unwrap_or(0),
        transcript_summary: transcript_buffer.format_window(5),
        resume_handle: shared.resume_handle.lock().clone(),
        saved_at: chrono::Utc::now(),
    };
    // Fire-and-forget to avoid blocking the control lane
    let p = persistence.clone();
    let sid = session_id.clone();
    tokio::spawn(async move {
        if let Err(e) = p.save(&sid, &snapshot).await {
            tracing::warn!("Session persistence failed: {}", e);
        }
    });
}
```

### 7.4 Restore on Reconnect

```rust
Live::builder()
    .persistence(FsPersistence::new("/tmp/sessions"))  // NEW
    .session_resume(true)
    .connect_vertex(project, location, token)
    .await?;
```

On `connect()`, if persistence has a saved snapshot and session_resume is enabled:

1. Load snapshot
2. Restore State from `snapshot.state`
3. Set PhaseMachine to `snapshot.phase` (the phase graph is re-registered by the developer — only the position is restored)
4. Inject transcript summary via `send_client_content`:
   ```rust
   let summary = Content::user(format!(
       "[Session resumed. Previous conversation summary:\n{}]",
       snapshot.transcript_summary
   ));
   writer.send_client_content(vec![summary], false).await?;
   ```

### 7.5 What CAN'T Be Persisted

- **Phase closures** (guards, on_enter, on_exit): These are registered by the application at startup. Persistence only restores the position, not the graph.
- **Exact transcript**: We persist a formatted summary, not the raw TranscriptBuffer. The summary is re-injected as context.
- **Watcher/temporal pattern state**: Reset on restart. Counters start fresh.

This is the same pattern as Dialogflow: the flow definition is static code, only the session position + state is persisted.

**Effort**: ~200 lines (persistence.rs), ~60 lines integration.

---

## Phase 8: Affective Signal Proxy (Gap 5)

### The Problem

The model detects and responds to emotional tone (via `affectiveDialog`), but this intelligence is trapped — the SDK can't observe it.

### 8.1 Affect-Aware Extraction

Rather than adding a parallel audio analysis pipeline (expensive, latency-adding), we use the model's own affective responses as a proxy signal.

**File**: `crates/rs-adk/src/live/extractor.rs`

```rust
/// Built-in extractor that infers user affect from the model's empathetic responses.
/// When affectiveDialog is enabled, the model shifts tone in response to user emotion.
/// We detect these shifts via output transcription analysis.
pub struct AffectProxyExtractor {
    llm: Arc<dyn BaseLlm>,
    /// Empathy markers that trigger extraction
    markers: Vec<String>,
}

impl AffectProxyExtractor {
    pub fn new(llm: Arc<dyn BaseLlm>) -> Self {
        Self {
            llm,
            markers: vec![
                "understand".into(), "sorry".into(), "hear you".into(),
                "frustrat".into(), "concern".into(), "difficult".into(),
                "appreciate".into(), "patience".into(),
            ],
        }
    }
}

#[async_trait]
impl TurnExtractor for AffectProxyExtractor {
    fn name(&self) -> &str { "affect" }
    fn window_size(&self) -> usize { 3 }

    fn should_extract(&self, window: &[TranscriptTurn]) -> bool {
        // Only extract when model output contains empathy markers
        window.last()
            .map(|t| {
                let lower = t.model.to_lowercase();
                self.markers.iter().any(|m| lower.contains(m))
            })
            .unwrap_or(false)
    }

    async fn extract(&self, window: &[TranscriptTurn]) -> Result<Value, LlmError> {
        // OOB LLM call to classify the affect shift
        // Returns: { "user_affect": "frustrated|calm|anxious|neutral",
        //            "intensity": 0.0-1.0,
        //            "model_response_tone": "empathetic|neutral|upbeat" }
    }
}
```

### 8.2 Fluent API (L2)

```rust
Live::builder()
    .affective_dialog(true)
    .affect_extraction(llm)  // NEW: enable proxy-based affect extraction
    .watch("affect:user_affect")
        .changed_to(json!("frustrated"))
        .then(|_, _, state| async move {
            state.set("risk_level", "high");
        })
```

### 8.3 Tradeoff

This is inferring user emotion from model behavior — indirect and not always accurate. But:
- Zero additional latency on the hot path (extraction is OOB)
- Zero additional cost for audio analysis (reuses existing transcript)
- The `should_extract()` gate means the LLM call only fires when empathy markers are detected
- Good enough for triggering guardrails (risk escalation, supervisor handoff)

**Effort**: ~80 lines (AffectProxyExtractor), ~15 lines L2.

---

## Implementation Sequence

The phases are ordered by dependency and impact:

```
Phase 0: Protocol Surface (L0)              ← Foundation for everything
  ├── 0.1 GenerationComplete event
  ├── 0.2 UsageMetadata event
  └── 0.3 ResumeInfo struct

Phase 1: Context Horizon (L1/L2)            ← Depends on 0.2
Phase 2: Generation Complete (L1/L2)         ← Depends on 0.1
Phase 3: Soft Turns (L1/L2)                  ← Independent
Phase 4: Context Steering (L1/L2)            ← Independent
Phase 5: Tool Advisory (L1/L2)               ← Depends on Phase 4 pattern
Phase 6: Conversation Repair (L1/L2)         ← Independent
Phase 7: Session Persistence (L1/L2)         ← Depends on 0.3
Phase 8: Affect Extraction (L1/L2)           ← Independent
```

**Parallelizable groups**:
- Group A (sequential): Phase 0 → Phase 1, Phase 2, Phase 7
- Group B (parallel with A): Phase 3, Phase 4 → Phase 5
- Group C (parallel with A+B): Phase 6, Phase 8

---

## Line Count Estimates

| Phase | New Files | New Lines | Modified Lines | Breaking Changes |
|-------|-----------|-----------|----------------|------------------|
| 0.1 | 0 | 20 | 10 | None |
| 0.2 | 0 | 40 | 15 | None |
| 0.3 | 0 | 30 | 20 | Minor (SessionResumeHandle variant) |
| 1 | 1 (context_horizon.rs) | 200 | 40 | None |
| 2 | 0 | 80 | 30 | None (new default) |
| 3 | 0 | 150 | 20 | None |
| 4 | 0 | 100 | 20 | None |
| 5 | 0 | 40 | 10 | None |
| 6 | 1 (needs.rs) | 120 | 40 | None |
| 7 | 1 (persistence.rs) | 200 | 60 | None |
| 8 | 0 | 80 | 15 | None |
| **Total** | **3** | **~1060** | **~280** | **1 minor** |

---

## What This Plan Does NOT Do

1. **Does not add a parallel audio analysis pipeline for affect detection.** The proxy approach is 80/20 — if customers need real prosodic analysis, that's a separate feature.

2. **Does not make the phase machine observational-only.** The full railroad-to-guardrails shift is a multi-release journey. This plan adds the guardrails posture as an option (`SteeringMode::ContextInjection`) without removing the railroad (`SteeringMode::InstructionUpdate`).

3. **Does not add lazy extraction.** The "only extract when phase guards need information" optimization is architecturally sound but requires a dependency graph between phase guards and extractors that adds significant complexity. Deferred to a future iteration.

4. **Does not add watcher-first control surface.** The analysis suggests watchers should be the primary control mechanism. This plan strengthens watchers (affect proxy, needs fulfillment) but doesn't deprecate the phase machine. That's a philosophical shift that needs community feedback.

5. **Does not add `prefixTurns` support.** The Gemini API supports pinning content at the start of the context window (surviving compression). This would be the ideal mechanism for durable keys (Phase 1), but it's not in the current wire protocol types. When the API stabilizes this feature, we should migrate `ContextHorizon::mark_durable()` to use it instead of synthetic context injection.

---

## Testing Strategy

Each phase should include:

1. **Unit tests**: For new structs (ContextHorizon, SoftTurnDetector, NeedsFulfillment, SessionSnapshot serialization)
2. **Integration tests**: Using `MockTransport` to simulate server behavior (GenerationComplete events, UsageMetadata, proactive silence)
3. **Cookbook validation**: Run existing cookbooks (debt-collection, voice-chat) to verify no regression
4. **New cookbook**: `cookbooks/long-conversation/` — a multi-phase, hour-long conversation that exercises context compression, session resumption, and conversation repair

---

## The Meta-Architecture After This Plan

```
                Model Intelligence
                      ↑
                      | (leads)
                      |
    ┌─────────────────┼─────────────────┐
    │              SDK Layer             │
    │                                    │
    │  Observe          Occasionally     │
    │  ┌──────────┐     Intervene        │
    │  │ Extractors│    ┌────────────┐   │
    │  │ Watchers  │    │ Nudges     │   │
    │  │ Affect    │    │ Tool       │   │
    │  │ Horizon   │    │  Advisory  │   │
    │  │ Telemetry │    │ Phase      │   │
    │  └──────────┘    │  Guards    │   │
    │                   │ Repair     │   │
    │                   └────────────┘   │
    │                                    │
    │  Persist         Circuit-Break     │
    │  ┌──────────┐    ┌────────────┐   │
    │  │ State     │    │ Escalation │   │
    │  │ Phase pos │    │ Risk gates │   │
    │  │ Transcript│    │ Compliance │   │
    │  │ Resume    │    │  override  │   │
    │  └──────────┘    └────────────┘   │
    └────────────────────────────────────┘
                      |
                      | (serves)
                      ↓
                 End User
```

The model leads. The SDK watches. Occasionally nudges. Only overrides when compliance demands it.
