# Context Construction Redesign — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Eliminate the double-instruction-send bug, add `prompt_on_enter` / `on_enter_context` / `TranscriptWindow` / `InstructionModifier` to the phase machine, modularize processor.rs, surface new APIs in L2 fluent builders, and rewrite demos.

**Architecture:** L0 (rs-genai) is untouched. L1 (rs-adk) gains new types in `phase.rs`, a `TranscriptWindow` type in `transcript.rs`, and processor.rs is split into composable functions. L2 (adk-rs-fluent) gains new `PhaseBuilder` methods and `P` module extensions. Demos adopt per-phase modifiers replacing global `instruction_template`.

**Tech Stack:** Rust, tokio, serde_json, rs-genai (L0 wire), rs-adk (L1 runtime), adk-rs-fluent (L2 fluent)

---

## Task 1: Add `TranscriptWindow` to transcript.rs

**Files:**
- Modify: `crates/rs-adk/src/live/transcript.rs`
- Modify: `crates/rs-adk/src/live/mod.rs` (re-export)

**Step 1: Add `TranscriptWindow` struct after `TranscriptBuffer` (after line 186)**

```rust
/// A read-only snapshot of recent transcript turns for context construction.
///
/// Cheap to create (clone of ~5 small structs). Used by `on_enter_context`
/// callbacks to reference recent conversation without holding the buffer lock.
#[derive(Debug, Clone)]
pub struct TranscriptWindow {
    turns: Vec<TranscriptTurn>,
}

impl TranscriptWindow {
    /// Create a window from a slice of turns.
    pub fn new(turns: Vec<TranscriptTurn>) -> Self {
        Self { turns }
    }

    /// The turns in this window.
    pub fn turns(&self) -> &[TranscriptTurn] {
        &self.turns
    }

    /// Format all turns as human-readable text for LLM consumption.
    pub fn formatted(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        for turn in &self.turns {
            if !turn.user.is_empty() {
                let _ = writeln!(out, "User: {}", turn.user.trim());
            }
            for tc in &turn.tool_calls {
                let _ = writeln!(out, "[Tool: {}({}) → {}]", tc.name, tc.args_summary, tc.result_summary);
            }
            if !turn.model.is_empty() {
                let _ = writeln!(out, "Assistant: {}", turn.model.trim());
            }
            let _ = writeln!(out);
        }
        out
    }

    /// Last user utterance, if any.
    pub fn last_user(&self) -> Option<&str> {
        self.turns.iter().rev()
            .find(|t| !t.user.is_empty())
            .map(|t| t.user.as_str())
    }

    /// Last model utterance, if any.
    pub fn last_model(&self) -> Option<&str> {
        self.turns.iter().rev()
            .find(|t| !t.model.is_empty())
            .map(|t| t.model.as_str())
    }

    /// Number of turns in this window.
    pub fn len(&self) -> usize {
        self.turns.len()
    }

    /// Whether the window is empty.
    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
    }
}
```

**Step 2: Add `snapshot_window` method to `TranscriptBuffer`**

Add inside `impl TranscriptBuffer` (after `has_pending` method, before the closing `}`):

```rust
    /// Create a `TranscriptWindow` snapshot of the last `n` completed turns.
    ///
    /// This is a cheap clone operation designed for passing to phase callbacks.
    pub fn snapshot_window(&self, n: usize) -> TranscriptWindow {
        TranscriptWindow::new(self.window(n).to_vec())
    }
```

**Step 3: Add re-export in mod.rs**

In `crates/rs-adk/src/live/mod.rs`, change line 36:
```rust
pub use transcript::{ToolCallSummary, TranscriptBuffer, TranscriptTurn, TranscriptWindow};
```

**Step 4: Add tests for TranscriptWindow**

Add to the `#[cfg(test)] mod tests` block in transcript.rs:

```rust
    #[test]
    fn snapshot_window_creates_window() {
        let mut buf = TranscriptBuffer::new();
        buf.push_input("Hello");
        buf.push_output("Hi there!");
        buf.end_turn();
        buf.push_input("How are you?");
        buf.push_output("I'm good!");
        buf.end_turn();

        let window = buf.snapshot_window(5);
        assert_eq!(window.len(), 2);
        assert_eq!(window.last_user(), Some("How are you?"));
        assert_eq!(window.last_model(), Some("I'm good!"));
        assert!(!window.is_empty());
    }

    #[test]
    fn transcript_window_formatted() {
        let mut buf = TranscriptBuffer::new();
        buf.push_input("What's the weather?");
        buf.push_output("It's sunny.");
        buf.end_turn();

        let window = buf.snapshot_window(1);
        let formatted = window.formatted();
        assert!(formatted.contains("User: What's the weather?"));
        assert!(formatted.contains("Assistant: It's sunny."));
    }

    #[test]
    fn transcript_window_empty() {
        let buf = TranscriptBuffer::new();
        let window = buf.snapshot_window(5);
        assert!(window.is_empty());
        assert_eq!(window.len(), 0);
        assert_eq!(window.last_user(), None);
        assert_eq!(window.last_model(), None);
    }
```

**Step 5: Run tests**

Run: `cargo test -p rs-adk transcript`
Expected: All new tests pass, existing tests unaffected.

**Step 6: Commit**

```
feat(rs-adk): add TranscriptWindow for phase callback context access
```

---

## Task 2: Add `InstructionModifier` and new fields to `Phase`

**Files:**
- Modify: `crates/rs-adk/src/live/phase.rs`
- Modify: `crates/rs-adk/src/live/mod.rs` (re-export)

**Step 1: Add `InstructionModifier` enum after `PhaseInstruction` (after line 46)**

```rust
/// A modifier that transforms a phase instruction based on runtime state.
///
/// Modifiers are evaluated in order during instruction composition.
/// They compose additively — each appends to the instruction built so far.
pub enum InstructionModifier {
    /// Append formatted state values: `[Context: key1=val1, key2=val2, ...]`
    StateAppend(Vec<String>),
    /// Append the result of a custom formatter function.
    CustomAppend(Arc<dyn Fn(&State) -> String + Send + Sync>),
    /// Conditionally append text when a predicate is true.
    Conditional {
        predicate: Arc<dyn Fn(&State) -> bool + Send + Sync>,
        text: String,
    },
}

impl InstructionModifier {
    /// Apply this modifier to a base instruction string, returning the modified instruction.
    pub fn apply(&self, base: &mut String, state: &State) {
        match self {
            InstructionModifier::StateAppend(keys) => {
                let mut pairs = Vec::with_capacity(keys.len());
                for key in keys {
                    // Strip common prefixes for display
                    let display_key = key
                        .strip_prefix("derived:")
                        .or_else(|| key.strip_prefix("session:"))
                        .or_else(|| key.strip_prefix("app:"))
                        .or_else(|| key.strip_prefix("user:"))
                        .unwrap_or(key);
                    if let Some(val) = state.get::<serde_json::Value>(key) {
                        match val {
                            serde_json::Value::String(s) => pairs.push(format!("{display_key}={s}")),
                            serde_json::Value::Number(n) => pairs.push(format!("{display_key}={n}")),
                            serde_json::Value::Bool(b) => pairs.push(format!("{display_key}={b}")),
                            other => pairs.push(format!("{display_key}={other}")),
                        }
                    }
                }
                if !pairs.is_empty() {
                    base.push_str("\n\n[Context: ");
                    base.push_str(&pairs.join(", "));
                    base.push(']');
                }
            }
            InstructionModifier::CustomAppend(f) => {
                let text = f(state);
                if !text.is_empty() {
                    base.push_str("\n\n");
                    base.push_str(&text);
                }
            }
            InstructionModifier::Conditional { predicate, text } => {
                if predicate(state) {
                    base.push_str("\n\n");
                    base.push_str(text);
                }
            }
        }
    }
}
```

**Step 2: Import `TranscriptWindow` at the top of phase.rs**

Add to the imports (after line 17):
```rust
use super::transcript::TranscriptWindow;
```

**Step 3: Add new fields to `Phase` struct (lines 57-74)**

Replace the `Phase` struct:

```rust
/// A conversation phase with instruction, tools, and transitions.
pub struct Phase {
    /// Unique name identifying this phase.
    pub name: String,
    /// The instruction (system prompt fragment) for this phase.
    pub instruction: PhaseInstruction,
    /// Tool filter — `None` means all tools are allowed.
    pub tools_enabled: Option<Vec<String>>,
    /// Optional guard: phase can only be entered when this returns `true`.
    pub guard: Option<Arc<dyn Fn(&State) -> bool + Send + Sync>>,
    /// Async callback executed when entering this phase.
    pub on_enter: Option<Arc<dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync>>,
    /// Async callback executed when leaving this phase.
    pub on_exit: Option<Arc<dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync>>,
    /// Ordered list of outbound transitions evaluated by the machine.
    pub transitions: Vec<Transition>,
    /// If `true`, `evaluate()` always returns `None` — no transitions out.
    pub terminal: bool,
    /// Instruction modifiers applied during instruction composition.
    /// Evaluated in order, each appends to the resolved instruction.
    pub modifiers: Vec<InstructionModifier>,
    /// If `true`, send `turnComplete: true` after instruction + context on phase entry,
    /// causing the model to generate a response immediately.
    pub prompt_on_enter: bool,
    /// Optional context injection on phase entry.
    /// Returns Content to send as `client_content` (turnComplete: false).
    /// Gives the model conversational continuity across phase transitions.
    pub on_enter_context: Option<Arc<
        dyn Fn(&State, &TranscriptWindow) -> Option<Vec<rs_genai::prelude::Content>>
            + Send + Sync
    >>,
}
```

**Step 4: Add `resolve_with_modifiers` method to `PhaseInstruction`**

After the existing `resolve` method (line 45):

```rust
    /// Resolve the instruction and apply modifiers, returning the composed instruction.
    pub fn resolve_with_modifiers(&self, state: &State, modifiers: &[InstructionModifier]) -> String {
        let mut instruction = self.resolve(state);
        for modifier in modifiers {
            modifier.apply(&mut instruction, state);
        }
        instruction
    }
```

**Step 5: Update `PhaseMachine::transition()` return type**

Replace the return type. Add a new struct for the transition result:

After `PhaseTransition` struct (after line 90):

```rust
/// Result of a phase transition, carrying the resolved instruction
/// and any context to inject.
pub struct TransitionResult {
    /// The resolved instruction for the new phase (with modifiers applied).
    pub instruction: String,
    /// Optional context content to inject via `send_client_content`.
    pub context: Option<Vec<rs_genai::prelude::Content>>,
    /// Whether to send `turnComplete: true` after instruction + context.
    pub prompt_on_enter: bool,
}
```

Update `PhaseMachine::transition()` signature and implementation (lines 172-222):

```rust
    /// Execute a transition: run `on_exit` for the current phase, update
    /// `current`, run `on_enter` for the new phase, record history, and
    /// return the `TransitionResult` for the new phase.
    ///
    /// Returns `None` if the target phase does not exist.
    pub async fn transition(
        &mut self,
        target: &str,
        state: &State,
        writer: &Arc<dyn SessionWriter>,
        turn: u32,
        trigger: TransitionTrigger,
        transcript_window: &TranscriptWindow,
    ) -> Option<TransitionResult> {
        // Target must exist.
        if !self.phases.contains_key(target) {
            return None;
        }

        let from = self.current.clone();
        let duration_in_phase = self.phase_entered_at.elapsed();

        // Run on_exit for the current phase (if it exists and has callback).
        if let Some(phase) = self.phases.get(&from) {
            if let Some(ref on_exit) = phase.on_exit {
                let fut = on_exit(state.clone(), Arc::clone(writer));
                fut.await;
            }
        }

        // Update current phase.
        self.current = target.to_string();
        self.phase_entered_at = Instant::now();

        // Run on_enter for the new phase.
        if let Some(phase) = self.phases.get(target) {
            if let Some(ref on_enter) = phase.on_enter {
                let fut = on_enter(state.clone(), Arc::clone(writer));
                fut.await;
            }
        }

        // Record history.
        self.history.push(PhaseTransition {
            from,
            to: target.to_string(),
            turn,
            timestamp: Instant::now(),
            trigger,
            duration_in_phase,
        });

        // Build transition result from the new phase.
        let phase = self.phases.get(target)?;
        let instruction = phase.instruction.resolve_with_modifiers(state, &phase.modifiers);
        let context = phase.on_enter_context
            .as_ref()
            .and_then(|f| f(state, transcript_window));
        let prompt_on_enter = phase.prompt_on_enter;

        Some(TransitionResult {
            instruction,
            context,
            prompt_on_enter,
        })
    }
```

**Step 6: Update mod.rs re-exports**

In `crates/rs-adk/src/live/mod.rs`, update line 29:
```rust
pub use phase::{InstructionModifier, Phase, PhaseInstruction, PhaseMachine, PhaseTransition, Transition, TransitionResult, TransitionTrigger};
```

**Step 7: Fix test helpers**

Update all test helper functions in `phase.rs` tests that create `Phase` to include the new fields:

In `simple_phase` (line 277-287):
```rust
    fn simple_phase(name: &str, instruction: &str) -> Phase {
        Phase {
            name: name.to_string(),
            instruction: PhaseInstruction::Static(instruction.to_string()),
            tools_enabled: None,
            guard: None,
            on_enter: None,
            on_exit: None,
            transitions: Vec::new(),
            terminal: false,
            modifiers: Vec::new(),
            prompt_on_enter: false,
            on_enter_context: None,
        }
    }
```

In `terminal_phase` (line 290-301):
```rust
    fn terminal_phase(name: &str, instruction: &str) -> Phase {
        Phase {
            name: name.to_string(),
            instruction: PhaseInstruction::Static(instruction.to_string()),
            tools_enabled: None,
            guard: None,
            on_enter: None,
            on_exit: None,
            transitions: Vec::new(),
            terminal: true,
            modifiers: Vec::new(),
            prompt_on_enter: false,
            on_enter_context: None,
        }
    }
```

Update all `transition()` calls in tests to pass a `&TranscriptWindow`:
```rust
use super::super::transcript::TranscriptWindow;

// In each test that calls transition():
let tw = TranscriptWindow::new(vec![]);
machine.transition("main", &state, &writer, 1, trigger, &tw).await;
```

**Step 8: Add tests for InstructionModifier**

```rust
    #[test]
    fn instruction_modifier_state_append() {
        let state = State::new();
        state.set("emotion", "happy");
        state.set("score", 0.8f64);

        let modifier = InstructionModifier::StateAppend(vec![
            "emotion".to_string(),
            "score".to_string(),
        ]);
        let mut base = "You are an assistant.".to_string();
        modifier.apply(&mut base, &state);
        assert!(base.contains("[Context: emotion=happy, score=0.8]"));
    }

    #[test]
    fn instruction_modifier_conditional_true() {
        let state = State::new();
        state.set("risk", "high");

        let modifier = InstructionModifier::Conditional {
            predicate: Arc::new(|s: &State| {
                s.get::<String>("risk").unwrap_or_default() == "high"
            }),
            text: "IMPORTANT: Use extra empathy.".to_string(),
        };
        let mut base = "Base instruction.".to_string();
        modifier.apply(&mut base, &state);
        assert!(base.contains("IMPORTANT: Use extra empathy."));
    }

    #[test]
    fn instruction_modifier_conditional_false() {
        let state = State::new();
        state.set("risk", "low");

        let modifier = InstructionModifier::Conditional {
            predicate: Arc::new(|s: &State| {
                s.get::<String>("risk").unwrap_or_default() == "high"
            }),
            text: "IMPORTANT: Use extra empathy.".to_string(),
        };
        let mut base = "Base instruction.".to_string();
        modifier.apply(&mut base, &state);
        assert!(!base.contains("IMPORTANT"));
    }

    #[test]
    fn resolve_with_modifiers_composes() {
        let state = State::new();
        state.set("mood", "calm");

        let instr = PhaseInstruction::Static("You are helpful.".to_string());
        let modifiers = vec![
            InstructionModifier::StateAppend(vec!["mood".to_string()]),
        ];
        let result = instr.resolve_with_modifiers(&state, &modifiers);
        assert!(result.starts_with("You are helpful."));
        assert!(result.contains("[Context: mood=calm]"));
    }
```

**Step 9: Run tests**

Run: `cargo test -p rs-adk phase`
Expected: All tests pass.

**Step 10: Commit**

```
feat(rs-adk): add InstructionModifier, TransitionResult, on_enter_context, prompt_on_enter to Phase
```

---

## Task 3: Modularize processor.rs — extract TurnComplete pipeline

**Files:**
- Modify: `crates/rs-adk/src/live/processor.rs`

This is the big refactor. We extract the TurnComplete handler into composable functions AND implement unified instruction composition (eliminating the double-send).

**Step 1: Add `use` for new types at the top of processor.rs**

After line 31, add:
```rust
use super::phase::TransitionResult;
use super::transcript::TranscriptWindow;
use rs_genai::prelude::Content;
```

**Step 2: Extract tool call handler into standalone function**

Add after `run_fast_lane` (after line 377), before `run_control_lane`:

```rust
/// Handle tool calls: phase filtering → user callback → auto-dispatch → interceptor → send.
async fn handle_tool_calls(
    calls: Vec<FunctionCall>,
    callbacks: &EventCallbacks,
    dispatcher: &Option<Arc<ToolDispatcher>>,
    writer: &Arc<dyn SessionWriter>,
    state: &State,
    phase_machine: &Option<tokio::sync::Mutex<PhaseMachine>>,
    background_tracker: &Option<Arc<BackgroundToolTracker>>,
    transcript_buffer: &mut TranscriptBuffer,
) {
    // 0. Phase-scoped tool filtering
    let (allowed_calls, rejected_responses) = if let Some(ref pm) = phase_machine {
        let active_tools = {
            let pm_guard = pm.lock().await;
            pm_guard.active_tools().map(|t| t.to_vec())
        };
        if let Some(active_tools) = active_tools {
            let mut allowed = Vec::new();
            let mut rejected = Vec::new();
            for call in calls {
                if active_tools.iter().any(|t| t == &call.name) {
                    allowed.push(call);
                } else {
                    rejected.push(FunctionResponse {
                        name: call.name.clone(),
                        response: serde_json::json!({
                            "error": format!(
                                "Tool '{}' is not available in the current conversation phase.",
                                call.name
                            )
                        }),
                        id: call.id.clone(),
                    });
                }
            }
            (allowed, rejected)
        } else {
            (calls, Vec::new())
        }
    } else {
        (calls, Vec::new())
    };

    // 1. Check user callback for override
    let responses = if allowed_calls.is_empty() && !rejected_responses.is_empty() {
        Some(rejected_responses.clone())
    } else if let Some(cb) = &callbacks.on_tool_call {
        let mut result = cb(allowed_calls.clone(), state.clone()).await;
        if !rejected_responses.is_empty() {
            let r = result.get_or_insert_with(Vec::new);
            r.extend(rejected_responses.clone());
        }
        result
    } else {
        None
    };

    // 2. Auto-dispatch if no override
    let responses = match responses {
        Some(r) => r,
        None => {
            let mut results: Vec<FunctionResponse> = rejected_responses;
            if let Some(ref disp) = dispatcher {
                for call in &allowed_calls {
                    match disp.call_function(&call.name, call.args.clone()).await {
                        Ok(result) => results.push(FunctionResponse {
                            name: call.name.clone(),
                            response: result,
                            id: call.id.clone(),
                        }),
                        Err(e) => results.push(FunctionResponse {
                            name: call.name.clone(),
                            response: serde_json::json!({"error": e.to_string()}),
                            id: call.id.clone(),
                        }),
                    }
                }
            } else if results.is_empty() {
                #[cfg(feature = "tracing-support")]
                tracing::warn!("Tool call received but no dispatcher or callback registered");
            }
            results
        }
    };

    // 3. Interceptor
    let responses = if let Some(cb) = &callbacks.before_tool_response {
        cb(responses, state.clone()).await
    } else {
        responses
    };

    // 4. Record in transcript
    for resp in &responses {
        let args = allowed_calls
            .iter()
            .find(|c| c.name == resp.name)
            .map(|c| &c.args)
            .unwrap_or(&serde_json::Value::Null);
        transcript_buffer.push_tool_call(resp.name.clone(), args, &resp.response);
    }

    // 5. Send responses
    if !responses.is_empty() {
        if let Err(_e) = writer.send_tool_response(responses).await {
            #[cfg(feature = "tracing-support")]
            tracing::error!("Failed to send tool response: {_e}");
        }
    }
}
```

**Step 3: Extract the TurnComplete handler into a standalone function**

Add after `handle_tool_calls`:

```rust
/// The TurnComplete pipeline — the heart of the control lane.
///
/// Executes the 13-step evaluation pipeline with unified instruction composition:
/// transcript → extractors → computed → phases → watchers → temporal → compose → send.
async fn handle_turn_complete(
    callbacks: &EventCallbacks,
    writer: &Arc<dyn SessionWriter>,
    shared: &SharedState,
    extractors: &[Arc<dyn TurnExtractor>],
    state: &State,
    computed: &Option<ComputedRegistry>,
    phase_machine: &Option<tokio::sync::Mutex<PhaseMachine>>,
    watchers: &Option<WatcherRegistry>,
    temporal: &Option<Arc<TemporalRegistry>>,
    transcript_buffer: &mut TranscriptBuffer,
) {
    // 1. Reset turn-scoped state
    state.clear_prefix("turn:");

    // 2. Finalize transcript (prefer server transcriptions)
    if let Some(input_text) = state.session().get::<String>("last_input_transcription") {
        transcript_buffer.set_input_transcription(&input_text);
    }
    if let Some(output_text) = state.session().get::<String>("last_output_transcription") {
        transcript_buffer.set_output_transcription(&output_text);
    }
    transcript_buffer.end_turn();

    // 3. Snapshot watched keys BEFORE extractors
    let pre_snapshot = watchers.as_ref().map(|w| {
        state.snapshot_values(
            &w.observed_keys().iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        )
    });

    // 4. Run extractors CONCURRENTLY
    if !extractors.is_empty() {
        let extraction_futures: Vec<_> = extractors
            .iter()
            .filter_map(|extractor| {
                let window_size = extractor.window_size();
                let window: Vec<_> = transcript_buffer.window(window_size).to_vec();
                if window.is_empty() {
                    return None;
                }
                let ext = extractor.clone();
                Some(async move {
                    match ext.extract(&window).await {
                        Ok(value) => Some((ext.name().to_string(), value)),
                        Err(_e) => {
                            #[cfg(feature = "tracing-support")]
                            tracing::warn!(extractor = ext.name(), "Extraction failed: {_e}");
                            let _ = _e;
                            None
                        }
                    }
                })
            })
            .collect();

        let results = futures::future::join_all(extraction_futures).await;
        for result in results.into_iter().flatten() {
            let (name, value) = result;
            state.set(&name, &value);
            if let Some(obj) = value.as_object() {
                for (field, val) in obj {
                    state.set(field, val.clone());
                }
            }
            if let Some(cb) = &callbacks.on_extracted {
                cb(name, value).await;
            }
        }
    }

    // 5. Recompute derived state
    if let Some(ref computed) = computed {
        computed.recompute(state);
    }

    // 6. Evaluate phase transitions (NO INSTRUCTION SEND — just accumulate)
    let mut resolved_instruction: Option<String> = None;
    let mut enter_context: Option<Vec<Content>> = None;
    let mut should_prompt_on_enter = false;
    let transcript_window = transcript_buffer.snapshot_window(5);

    if let Some(ref pm) = phase_machine {
        let mut machine = pm.lock().await;
        if let Some((target, transition_index)) =
            machine.evaluate(state).map(|(s, i)| (s.to_string(), i))
        {
            let turn = state.session().get::<u32>("turn_count").unwrap_or(0);
            let trigger = super::phase::TransitionTrigger::Guard { transition_index };
            if let Some(result) = machine
                .transition(&target, state, writer, turn, trigger, &transcript_window)
                .await
            {
                resolved_instruction = Some(result.instruction);
                enter_context = result.context;
                should_prompt_on_enter = result.prompt_on_enter;
            }
            state.session().set("phase", machine.current());
        }
    }

    // 7. Fire watchers
    if let (Some(ref watchers), Some(pre)) = (watchers, pre_snapshot) {
        let post_keys: Vec<&str> = watchers.observed_keys().iter().map(|s| s.as_str()).collect();
        let diffs = state.diff_values(&pre, &post_keys);
        if !diffs.is_empty() {
            let (blocking, concurrent) = watchers.evaluate(&diffs, state);
            for action in blocking {
                action.await;
            }
            for action in concurrent {
                tokio::spawn(action);
            }
        }
    }

    // 8. Check temporal patterns
    if let Some(ref temporal) = temporal {
        let event = SessionEvent::TurnComplete;
        for action in temporal.check_all(state, Some(&event), writer) {
            tokio::spawn(action);
        }
    }

    // 9. Instruction amendment (additive — appends to phase instruction)
    if let Some(ref amendment_fn) = callbacks.instruction_amendment {
        if let Some(amendment_text) = amendment_fn(state) {
            let base = resolved_instruction.clone().unwrap_or_else(|| {
                // No phase transition this turn — use current phase instruction
                if let Some(ref pm) = phase_machine {
                    // We need to try_lock since we're not in an async context for the guard
                    // But we are async here, so we can await
                    // Use a block to limit scope
                    let instruction = futures::executor::block_on(async {
                        let guard = pm.lock().await;
                        guard.current_phase()
                            .map(|p| p.instruction.resolve_with_modifiers(state, &p.modifiers))
                    });
                    instruction.unwrap_or_default()
                } else {
                    String::new()
                }
            });
            resolved_instruction = Some(format!("{base}\n\n{amendment_text}"));
        }
    }

    // 10. Instruction template (full replacement — escape hatch)
    if let Some(ref template) = callbacks.instruction_template {
        if let Some(new_instruction) = template(state) {
            resolved_instruction = Some(new_instruction);
        }
    }

    // 10b. UNIFIED SEND — single instruction write with dedup
    if let Some(instruction) = &resolved_instruction {
        let should_update = {
            let last = shared.last_instruction.lock();
            last.as_deref() != Some(instruction.as_str())
        };
        if should_update {
            *shared.last_instruction.lock() = Some(instruction.clone());
            writer.update_instruction(instruction.clone()).await.ok();
        }
    }

    // 10c. Send on_enter_context content (if phase transitioned)
    if let Some(context) = enter_context {
        if !context.is_empty() {
            writer.send_client_content(context, false).await.ok();
        }
    }

    // 10d. Send turnComplete if prompt_on_enter (triggers model response)
    if should_prompt_on_enter {
        writer.send_client_content(vec![], true).await.ok();
    }

    // 11. Turn boundary hook
    if let Some(cb) = &callbacks.on_turn_boundary {
        cb(state.clone(), writer.clone()).await;
    }

    // 12. User turn-complete callback
    if let Some(cb) = &callbacks.on_turn_complete {
        cb().await;
    }

    // 13. Update session turn count
    let tc: u32 = state.session().get("turn_count").unwrap_or(0);
    state.session().set("turn_count", tc + 1);
}
```

**Step 4: Simplify `run_control_lane` to delegate to extracted functions**

Replace the entire `run_control_lane` body (lines 383-762) — the match arms for ToolCall and TurnComplete now delegate:

```rust
async fn run_control_lane(
    mut rx: mpsc::Receiver<ControlEvent>,
    callbacks: Arc<EventCallbacks>,
    dispatcher: Option<Arc<ToolDispatcher>>,
    writer: Arc<dyn SessionWriter>,
    shared: Arc<SharedState>,
    extractors: Vec<Arc<dyn TurnExtractor>>,
    state: State,
    computed: Option<ComputedRegistry>,
    phase_machine: Option<tokio::sync::Mutex<PhaseMachine>>,
    watchers: Option<WatcherRegistry>,
    temporal: Option<Arc<TemporalRegistry>>,
    background_tracker: Option<Arc<BackgroundToolTracker>>,
) {
    let mut transcript_buffer = TranscriptBuffer::new();

    while let Some(event) = rx.recv().await {
        match event {
            ControlEvent::InputTranscript(text) => {
                transcript_buffer.push_input(&text);
            }
            ControlEvent::OutputTranscript(text) => {
                transcript_buffer.push_output(&text);
            }
            ControlEvent::ToolCall(calls) => {
                handle_tool_calls(
                    calls, &callbacks, &dispatcher, &writer, &state,
                    &phase_machine, &background_tracker, &mut transcript_buffer,
                ).await;
            }
            ControlEvent::ToolCallCancelled(ids) => {
                if let Some(ref tracker) = background_tracker {
                    tracker.cancel(&ids);
                }
                if let Some(ref disp) = dispatcher {
                    disp.cancel_by_ids(&ids).await;
                }
                if let Some(cb) = &callbacks.on_tool_cancelled {
                    cb(ids).await;
                }
            }
            ControlEvent::Interrupted => {
                transcript_buffer.truncate_current_model_turn();
                if let Some(cb) = &callbacks.on_interrupted {
                    cb().await;
                }
                shared.interrupted.store(false, Ordering::Release);
            }
            ControlEvent::TurnComplete => {
                handle_turn_complete(
                    &callbacks, &writer, &shared, &extractors, &state,
                    &computed, &phase_machine, &watchers, &temporal,
                    &mut transcript_buffer,
                ).await;
            }
            ControlEvent::GoAway(time_left) => {
                let duration = time_left
                    .as_deref()
                    .and_then(|s| s.trim_end_matches('s').parse::<u64>().ok())
                    .map(Duration::from_secs)
                    .unwrap_or(Duration::from_secs(60));
                if let Some(cb) = &callbacks.on_go_away {
                    cb(duration).await;
                }
            }
            ControlEvent::Connected => {
                if let Some(cb) = &callbacks.on_connected {
                    cb(writer.clone()).await;
                }
            }
            ControlEvent::Disconnected(reason) => {
                if let Some(cb) = &callbacks.on_disconnected {
                    cb(reason).await;
                }
            }
            ControlEvent::SessionResumeHandle(_handle) => {}
            ControlEvent::Error(err) => {
                if let Some(cb) = &callbacks.on_error {
                    cb(err).await;
                }
            }
        }
    }
}
```

**Step 5: Fix the amendment block**

The `block_on` in the amendment section is wrong — we're already in async context. Replace the amendment block (step 9 in handle_turn_complete) with:

```rust
    // 9. Instruction amendment (additive)
    if let Some(ref amendment_fn) = callbacks.instruction_amendment {
        if let Some(amendment_text) = amendment_fn(state) {
            let base = if let Some(inst) = resolved_instruction.as_ref() {
                inst.clone()
            } else if let Some(ref pm) = phase_machine {
                let guard = pm.lock().await;
                guard.current_phase()
                    .map(|p| p.instruction.resolve_with_modifiers(state, &p.modifiers))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            resolved_instruction = Some(format!("{base}\n\n{amendment_text}"));
        }
    }
```

**Step 6: Run tests**

Run: `cargo test -p rs-adk`
Expected: All existing tests pass. The behavior is identical — only the code structure changed (plus the double-send is eliminated).

**Step 7: Commit**

```
refactor(rs-adk): modularize processor.rs, unify instruction composition into single send
```

---

## Task 4: Update L2 PhaseBuilder with new APIs

**Files:**
- Modify: `crates/adk-rs-fluent/src/live_builders.rs`
- Modify: `crates/adk-rs-fluent/src/live.rs`

**Step 1: Update PhaseBuilder struct and imports in live_builders.rs**

Replace the imports (lines 26-28):
```rust
use rs_adk::live::{
    BoxFuture, InstructionModifier, Phase, PhaseInstruction, Transition, WatchPredicate, Watcher,
};
use rs_adk::live::transcript::TranscriptWindow;
```

Replace `PhaseBuilder` struct (lines 39-49):
```rust
pub struct PhaseBuilder {
    live: Live,
    name: String,
    instruction: Option<PhaseInstruction>,
    tools_enabled: Option<Vec<String>>,
    guard: Option<Arc<dyn Fn(&State) -> bool + Send + Sync>>,
    on_enter: Option<Arc<dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync>>,
    on_exit: Option<Arc<dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync>>,
    transitions: Vec<Transition>,
    terminal: bool,
    modifiers: Vec<InstructionModifier>,
    prompt_on_enter: bool,
    on_enter_context: Option<Arc<
        dyn Fn(&State, &TranscriptWindow) -> Option<Vec<rs_genai::prelude::Content>>
            + Send + Sync
    >>,
}
```

Update `new()` (lines 52-64):
```rust
    pub(crate) fn new(live: Live, name: impl Into<String>) -> Self {
        Self {
            live,
            name: name.into(),
            instruction: None,
            tools_enabled: None,
            guard: None,
            on_enter: None,
            on_exit: None,
            transitions: Vec::new(),
            terminal: false,
            modifiers: Vec::new(),
            prompt_on_enter: false,
            on_enter_context: None,
        }
    }
```

**Step 2: Add new builder methods to PhaseBuilder impl**

After `terminal()` (after line 133), before `done()`:

```rust
    /// Append formatted state values to the instruction for this phase.
    ///
    /// Keys are read from `State` at instruction-composition time and formatted
    /// as `[Context: key1=val1, key2=val2, ...]`.
    ///
    /// ```ignore
    /// .phase("negotiate")
    ///     .instruction(NEGOTIATE_INSTRUCTION)
    ///     .with_state(&["emotional_state", "willingness_to_pay", "derived:call_risk_level"])
    ///     .done()
    /// ```
    pub fn with_state(mut self, keys: &[&str]) -> Self {
        self.modifiers.push(InstructionModifier::StateAppend(
            keys.iter().map(|k| k.to_string()).collect(),
        ));
        self
    }

    /// Conditionally append text to the instruction when a predicate is true.
    ///
    /// ```ignore
    /// .phase("negotiate")
    ///     .instruction(NEGOTIATE_INSTRUCTION)
    ///     .when(|s| s.get::<String>("derived:risk").unwrap_or_default() == "high",
    ///           "IMPORTANT: Use extra empathy.")
    ///     .done()
    /// ```
    pub fn when(
        mut self,
        predicate: impl Fn(&State) -> bool + Send + Sync + 'static,
        text: impl Into<String>,
    ) -> Self {
        self.modifiers.push(InstructionModifier::Conditional {
            predicate: Arc::new(predicate),
            text: text.into(),
        });
        self
    }

    /// Append a custom-formatted string to the instruction.
    pub fn with_context<F>(mut self, f: F) -> Self
    where
        F: Fn(&State) -> String + Send + Sync + 'static,
    {
        self.modifiers.push(InstructionModifier::CustomAppend(Arc::new(f)));
        self
    }

    /// If `true`, send `turnComplete: true` after instruction + context on phase entry,
    /// causing the model to generate a response immediately.
    ///
    /// Use for phases where the model should speak first (e.g., scripted disclosures,
    /// greetings). Default is `false` (model waits for user input).
    ///
    /// ```ignore
    /// .phase("disclosure")
    ///     .instruction(DISCLOSURE_INSTRUCTION)
    ///     .prompt_on_enter(true)  // Model delivers disclosure immediately
    ///     .done()
    /// ```
    pub fn prompt_on_enter(mut self, prompt: bool) -> Self {
        self.prompt_on_enter = prompt;
        self
    }

    /// Set a context injection callback for phase entry.
    ///
    /// Called when a transition into this phase fires. Receives the current
    /// state and a transcript window of recent conversation. Returns optional
    /// `Content` to inject via `send_client_content` (turnComplete: false),
    /// giving the model conversational continuity across phase transitions.
    ///
    /// ```ignore
    /// .phase("inform_debt")
    ///     .on_enter_context(|state, transcript| {
    ///         let name: String = state.get("debtor_name").unwrap_or_default();
    ///         Some(vec![Content::user(format!(
    ///             "[{name}'s identity is verified. Recent:\n{}]",
    ///             transcript.formatted()
    ///         ))])
    ///     })
    ///     .done()
    /// ```
    pub fn on_enter_context<F>(mut self, f: F) -> Self
    where
        F: Fn(&State, &TranscriptWindow) -> Option<Vec<rs_genai::prelude::Content>>
            + Send + Sync + 'static,
    {
        self.on_enter_context = Some(Arc::new(f));
        self
    }
```

**Step 3: Update `done()` to pass new fields**

Replace `done()` (lines 136-151):
```rust
    pub fn done(mut self) -> Live {
        let phase = Phase {
            name: self.name,
            instruction: self
                .instruction
                .unwrap_or(PhaseInstruction::Static(String::new())),
            tools_enabled: self.tools_enabled,
            guard: self.guard,
            on_enter: self.on_enter,
            on_exit: self.on_exit,
            transitions: self.transitions,
            terminal: self.terminal,
            modifiers: self.modifiers,
            prompt_on_enter: self.prompt_on_enter,
            on_enter_context: self.on_enter_context,
        };
        self.live.add_phase(phase);
        self.live
    }
```

**Step 4: Run tests**

Run: `cargo test -p adk-rs-fluent`
Expected: All tests pass. Existing API is unchanged; new methods are additive.

**Step 5: Build the full workspace to check for any compilation errors**

Run: `cargo build --workspace`
Expected: Clean compile. Fix any errors caused by new Phase fields not being populated in other code paths.

**Step 6: Commit**

```
feat(adk-rs-fluent): add with_state, when, prompt_on_enter, on_enter_context to PhaseBuilder
```

---

## Task 5: Fix all compilation errors across workspace

The new `Phase` fields (`modifiers`, `prompt_on_enter`, `on_enter_context`) and the changed `transition()` signature will cause compilation errors in any code that constructs `Phase` directly or calls `transition()`.

**Files to check:**
- `crates/rs-adk/src/live/builder.rs` — may construct Phase
- `apps/adk-web/src/apps/*.rs` — construct phases via L2 builder (should be fine)
- Any direct `PhaseMachine::transition()` callers

**Step 1: Search for all direct Phase constructions**

Run: `cargo build --workspace 2>&1 | head -80`

Fix each error by adding the new fields with defaults:
```rust
modifiers: Vec::new(),
prompt_on_enter: false,
on_enter_context: None,
```

**Step 2: Run full workspace build**

Run: `cargo build --workspace`
Expected: Clean compile.

**Step 3: Run all tests**

Run: `cargo test --workspace`
Expected: All tests pass.

**Step 4: Commit**

```
fix: add default values for new Phase fields across workspace
```

---

## Task 6: Rewrite debt_collection.rs with new primitives

**Files:**
- Modify: `apps/adk-web/src/apps/debt_collection.rs`

This is the showcase rewrite. Replace the global `instruction_template` with per-phase `with_state`/`when` modifiers, add `on_enter_context` for conversational continuity, and add `prompt_on_enter(true)` to the disclosure phase.

**Step 1: Add `prompt_on_enter(true)` to disclosure phase**

Find the disclosure phase builder chain and add:
```rust
.phase("disclosure")
    .instruction(DISCLOSURE_INSTRUCTION)
    .prompt_on_enter(true)
    // ... existing on_enter and transitions ...
```

**Step 2: Add `with_state` and `when` to each phase**

For every phase, add the state context and risk warning that was previously in instruction_template:

```rust
// Common state keys for all phases:
const CONTEXT_KEYS: &[&str] = &[
    "emotional_state",
    "willingness_to_pay",
    "derived:call_risk_level",
    "identity_verified",
    "disclosure_given",
];

// Each phase gets:
    .with_state(CONTEXT_KEYS)
    .when(|s| {
        let risk = s.get::<String>("derived:call_risk_level").unwrap_or_default();
        risk == "high" || risk == "critical"
    }, "IMPORTANT: The caller is showing signs of distress. Use extra empathy. \
        Never threaten, harass, or use deceptive language. If they request to stop \
        being contacted, immediately comply with cease-and-desist requirements.")
```

**Step 3: Add `on_enter_context` to inform_debt and negotiate phases**

```rust
.phase("inform_debt")
    .instruction(INFORM_DEBT_INSTRUCTION)
    .with_state(CONTEXT_KEYS)
    .when(/* risk check */)
    .tools(vec!["lookup_account".into()])
    .on_enter_context({
        move |state, transcript| {
            let name: String = state.get("debtor_name").unwrap_or_default();
            Some(vec![Content::user(format!(
                "[{name}'s identity has been verified. Proceed to inform them \
                 about the debt. Use the lookup_account tool to retrieve details. \
                 Recent conversation:\n{}]",
                transcript.formatted()
            ))])
        }
    })
```

**Step 4: Remove the global `instruction_template` closure**

Delete the entire `.instruction_template(|state| { ... })` block (~40 lines, lines 844-882).

**Step 5: Build and test**

Run: `cargo build -p adk-web`
Expected: Clean compile.

**Step 6: Commit**

```
refactor(examples): rewrite debt_collection with per-phase modifiers, remove instruction_template
```

---

## Task 7: Rewrite support.rs and playbook.rs

**Files:**
- Modify: `apps/adk-web/src/apps/support.rs`
- Modify: `apps/adk-web/src/apps/playbook.rs`

Same pattern as debt_collection: replace `instruction_template` with per-phase `with_state`/`when`, add `on_enter_context` where conversational continuity matters, add `prompt_on_enter` where appropriate.

**Step 1: Apply same pattern to support.rs**

Read the file, identify the instruction_template, and replace with per-phase modifiers.

**Step 2: Apply same pattern to playbook.rs**

Same approach.

**Step 3: Build and test**

Run: `cargo build -p adk-web`
Expected: Clean compile.

**Step 4: Commit**

```
refactor(examples): rewrite support and playbook with per-phase modifiers
```

---

## Task 8: Final verification

**Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: Clean compile, zero warnings about unused fields.

**Step 2: Full test suite**

Run: `cargo test --workspace`
Expected: All tests pass.

**Step 3: Clippy**

Run: `cargo clippy --workspace`
Expected: No warnings on new code.

**Step 4: Commit**

```
chore: final cleanup and verification
```
