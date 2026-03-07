//! Declarative conversation phase management.
//!
//! A [`PhaseMachine`] holds named [`Phase`]s and evaluates guard-based
//! transitions. Each phase carries an instruction (static or dynamic),
//! optional tool filters, and async entry/exit callbacks.
//!
//! The machine is owned by the control-lane task; no internal locking is
//! required — `&self` for reads, `&mut self` for mutations.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rs_genai::session::SessionWriter;

use super::transcript::TranscriptWindow;
use super::BoxFuture;
use crate::state::State;

// ── Core types ──────────────────────────────────────────────────────────────

/// What caused a phase transition.
#[derive(Debug, Clone)]
pub enum TransitionTrigger {
    /// A named transition guard returned true during evaluate()
    Guard {
        /// Index of the transition guard that triggered.
        transition_index: usize,
    },
    /// Explicit programmatic transition
    Programmatic {
        /// Source identifier for debugging (e.g., "tool_call", "watcher").
        source: &'static str,
    },
}

/// Instruction source for a phase — either a fixed string or a closure over state.
pub enum PhaseInstruction {
    /// A fixed instruction string.
    Static(String),
    /// A dynamic instruction derived from current state.
    Dynamic(Arc<dyn Fn(&State) -> String + Send + Sync>),
}

impl PhaseInstruction {
    /// Resolve the instruction to a concrete string.
    pub fn resolve(&self, state: &State) -> String {
        match self {
            PhaseInstruction::Static(s) => s.clone(),
            PhaseInstruction::Dynamic(f) => f(state),
        }
    }

    /// Resolve the instruction and apply modifiers, returning the composed instruction.
    pub fn resolve_with_modifiers(&self, state: &State, modifiers: &[InstructionModifier]) -> String {
        let mut instruction = self.resolve(state);
        for modifier in modifiers {
            modifier.apply(&mut instruction, state);
        }
        instruction
    }
}

/// A modifier that transforms a phase instruction based on runtime state.
///
/// Modifiers are evaluated in order during instruction composition.
/// They compose additively — each appends to the instruction built so far.
#[derive(Clone)]
pub enum InstructionModifier {
    /// Append formatted state values: `[Context: key1=val1, key2=val2, ...]`
    StateAppend(Vec<String>),
    /// Append the result of a custom formatter function.
    CustomAppend(Arc<dyn Fn(&State) -> String + Send + Sync>),
    /// Conditionally append text when a predicate is true.
    Conditional {
        /// Predicate that determines whether to append the text.
        predicate: Arc<dyn Fn(&State) -> bool + Send + Sync>,
        /// Text to append when the predicate is true.
        text: String,
    },
}

impl InstructionModifier {
    /// Apply this modifier to a base instruction string, mutating it in place.
    pub fn apply(&self, base: &mut String, state: &State) {
        match self {
            InstructionModifier::StateAppend(keys) => {
                let mut pairs = Vec::with_capacity(keys.len());
                for key in keys {
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

/// A guard-based transition to a named target phase.
pub struct Transition {
    /// Name of the target phase.
    pub target: String,
    /// Guard function — transition fires when this returns `true`.
    pub guard: Arc<dyn Fn(&State) -> bool + Send + Sync>,
    /// Optional human-readable description of when/why this transition fires.
    /// Used by `describe_navigation()` to tell the model what paths are available.
    pub description: Option<String>,
}

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
    /// State keys this phase is responsible for gathering.
    ///
    /// Purely informational — does not affect transitions or enforcement.
    /// The [`ContextBuilder`](super::context_builder::ContextBuilder) reads
    /// these from `session:phase_needs` to append a "[Gathering] key1, key2"
    /// line to the instruction, so the model knows what to focus on.
    pub needs: Vec<String>,
}

impl Phase {
    /// Create a minimal non-terminal phase with a static instruction and defaults.
    pub fn new(name: &str, instruction: &str) -> Self {
        Self {
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
            needs: Vec::new(),
        }
    }
}

/// Record of a single phase transition for history/debugging.
pub struct PhaseTransition {
    /// Phase we left.
    pub from: String,
    /// Phase we entered.
    pub to: String,
    /// Turn number at the time of transition.
    pub turn: u32,
    /// Wall-clock instant of the transition.
    pub timestamp: Instant,
    /// What caused this transition.
    pub trigger: TransitionTrigger,
    /// How long the machine spent in the source phase before transitioning.
    pub duration_in_phase: Duration,
}

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

// ── PhaseMachine ────────────────────────────────────────────────────────────

/// Maximum phase transitions retained in history ring buffer.
const MAX_PHASE_HISTORY: usize = 100;

/// Evaluates transitions and manages phase entry/exit lifecycle.
pub struct PhaseMachine {
    phases: HashMap<String, Phase>,
    current: String,
    initial: String,
    history: VecDeque<PhaseTransition>,
    phase_entered_at: Instant,
}

impl PhaseMachine {
    /// Create a new machine with the given initial phase name.
    ///
    /// The initial phase must be registered via [`add_phase`](Self::add_phase)
    /// before calling [`validate`](Self::validate).
    pub fn new(initial: &str) -> Self {
        Self {
            phases: HashMap::new(),
            current: initial.to_string(),
            initial: initial.to_string(),
            history: VecDeque::new(),
            phase_entered_at: Instant::now(),
        }
    }

    /// Register a phase. Overwrites any existing phase with the same name.
    pub fn add_phase(&mut self, phase: Phase) {
        self.phases.insert(phase.name.clone(), phase);
    }

    /// The name of the current phase.
    pub fn current(&self) -> &str {
        &self.current
    }

    /// A reference to the current [`Phase`], if it exists in the registry.
    pub fn current_phase(&self) -> Option<&Phase> {
        self.phases.get(&self.current)
    }

    /// The transition history (oldest first, capped at 100 entries).
    pub fn history(&self) -> &VecDeque<PhaseTransition> {
        &self.history
    }

    /// Mutable access to the transition history (for testing and internal use).
    pub(crate) fn history_mut(&mut self) -> &mut VecDeque<PhaseTransition> {
        &mut self.history
    }

    /// Generate a structured navigation context block giving the model
    /// awareness of where it is in the conversation flow.
    ///
    /// The output includes the current phase and its goal, recent phase
    /// history, any state keys still needed, and possible transitions.
    pub fn describe_navigation(&self, state: &State) -> String {
        let mut lines = Vec::new();
        lines.push("[Navigation]".to_string());

        // 1. Current phase + goal (first sentence of resolved instruction)
        if let Some(phase) = self.phases.get(&self.current) {
            let resolved = phase.instruction.resolve(state);
            let goal = resolved
                .split('.')
                .next()
                .unwrap_or(&resolved)
                .trim();
            lines.push(format!("Current phase: {} — {}", self.current, goal));

            // 2. Phase history (last 3 entries)
            if !self.history.is_empty() {
                let recent: Vec<String> = self
                    .history
                    .iter()
                    .rev()
                    .take(3)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .map(|h| format!("{} (turn {})", h.from, h.turn))
                    .collect();
                lines.push(format!("Previous: {}", recent.join(", ")));
            }

            // 3. Still needed keys (from phase.needs, filtered by state)
            let missing: Vec<&str> = phase
                .needs
                .iter()
                .filter(|key| !state.contains(key))
                .map(|s| s.as_str())
                .collect();
            if !missing.is_empty() {
                lines.push(format!("Still needed: {}", missing.join(", ")));
            }

            // 4. Possible transitions or terminal
            if phase.terminal {
                lines.push("This is the final phase.".to_string());
            } else if !phase.transitions.is_empty() {
                lines.push("Possible next:".to_string());
                for t in &phase.transitions {
                    if let Some(ref desc) = t.description {
                        lines.push(format!("  → {}: {}", t.target, desc));
                    } else {
                        lines.push(format!("  → {}", t.target));
                    }
                }
            }
        }

        lines.join("\n")
    }

    /// Evaluate transitions from the current phase.
    ///
    /// Returns the target phase name and transition index of the first
    /// transition whose guard returns `true`, or `None` if no transition
    /// fires (or the current phase is terminal / missing).
    ///
    /// This method is **pure** — it does not modify state or execute callbacks.
    pub fn evaluate(&self, state: &State) -> Option<(&str, usize)> {
        let phase = self.phases.get(&self.current)?;
        if phase.terminal {
            return None;
        }
        for (index, transition) in phase.transitions.iter().enumerate() {
            if (transition.guard)(state) {
                // Check target phase guard — if the target phase has a guard
                // that returns false, skip this transition and try the next one.
                if let Some(target_phase) = self.phases.get(&transition.target) {
                    if let Some(ref phase_guard) = target_phase.guard {
                        if !phase_guard(state) {
                            continue;
                        }
                    }
                }
                return Some((&transition.target, index));
            }
        }
        None
    }

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

        // Record history (ring buffer — evict oldest if at capacity).
        if self.history.len() >= MAX_PHASE_HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(PhaseTransition {
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

    /// Returns how long the machine has been in the current phase.
    pub fn current_phase_duration(&self) -> Duration {
        self.phase_entered_at.elapsed()
    }

    /// Active tools filter for the current phase.
    ///
    /// Returns `None` when all tools are allowed, or `Some(slice)` with
    /// the explicitly enabled tool names.
    pub fn active_tools(&self) -> Option<&[String]> {
        self.phases
            .get(&self.current)
            .and_then(|p| p.tools_enabled.as_deref())
    }

    /// Validate the machine configuration.
    ///
    /// Checks:
    /// - At least one phase is registered.
    /// - The initial phase exists.
    /// - Every transition target references an existing phase.
    pub fn validate(&self) -> Result<(), String> {
        if self.phases.is_empty() {
            return Err("no phases registered".to_string());
        }
        if !self.phases.contains_key(&self.initial) {
            return Err(format!(
                "initial phase '{}' not found in registered phases",
                self.initial
            ));
        }
        for phase in self.phases.values() {
            for transition in &phase.transitions {
                if !self.phases.contains_key(&transition.target) {
                    return Err(format!(
                        "phase '{}' has transition to unknown target '{}'",
                        phase.name, transition.target
                    ));
                }
            }
        }
        Ok(())
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::transcript::TranscriptWindow;

    /// Helper: create a minimal non-terminal phase with no callbacks.
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
            needs: Vec::new(),
        }
    }

    /// Helper: create a terminal phase.
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
            needs: Vec::new(),
        }
    }

    /// Helper: empty transcript window for tests.
    fn empty_tw() -> TranscriptWindow {
        TranscriptWindow::new(vec![])
    }

    // ── 1. new + add_phase + current ────────────────────────────────────

    #[test]
    fn new_and_add_phase_and_current() {
        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(simple_phase("greeting", "Say hello"));
        assert_eq!(machine.current(), "greeting");
        assert!(machine.current_phase().is_some());
        assert!(machine.history().is_empty());
    }

    // ── 2. evaluate with single transition that fires ───────────────────

    #[test]
    fn evaluate_single_transition_fires() {
        let state = State::new();
        state.set("ready", true);

        let mut greeting = simple_phase("greeting", "Say hello");
        greeting.transitions.push(Transition {
            target: "main".to_string(),
            guard: Arc::new(|s: &State| s.get::<bool>("ready").unwrap_or(false)),
            description: None,
        });

        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(greeting);
        machine.add_phase(simple_phase("main", "Main phase"));

        assert_eq!(machine.evaluate(&state), Some(("main", 0)));
    }

    // ── 3. evaluate with single transition that does not fire ───────────

    #[test]
    fn evaluate_single_transition_does_not_fire() {
        let state = State::new();
        // "ready" is not set → guard returns false

        let mut greeting = simple_phase("greeting", "Say hello");
        greeting.transitions.push(Transition {
            target: "main".to_string(),
            guard: Arc::new(|s: &State| s.get::<bool>("ready").unwrap_or(false)),
            description: None,
        });

        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(greeting);
        machine.add_phase(simple_phase("main", "Main phase"));

        assert_eq!(machine.evaluate(&state), None);
    }

    // ── 4. evaluate with multiple transitions (first match wins) ────────

    #[test]
    fn evaluate_multiple_transitions_first_match_wins() {
        let state = State::new();
        state.set("escalate", true);
        state.set("done", true);

        let mut greeting = simple_phase("greeting", "Say hello");
        greeting.transitions.push(Transition {
            target: "escalated".to_string(),
            guard: Arc::new(|s: &State| s.get::<bool>("escalate").unwrap_or(false)),
            description: None,
        });
        greeting.transitions.push(Transition {
            target: "farewell".to_string(),
            guard: Arc::new(|s: &State| s.get::<bool>("done").unwrap_or(false)),
            description: None,
        });

        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(greeting);
        machine.add_phase(simple_phase("escalated", "Escalated"));
        machine.add_phase(simple_phase("farewell", "Farewell"));

        // Both guards are true, but "escalated" is declared first (index 0).
        assert_eq!(machine.evaluate(&state), Some(("escalated", 0)));
    }

    // ── 5. evaluate on terminal phase returns None ──────────────────────

    #[test]
    fn evaluate_terminal_phase_returns_none() {
        let state = State::new();
        state.set("anything", true);

        let mut term = terminal_phase("end", "Goodbye");
        // Even if we add a transition, terminal should short-circuit.
        term.transitions.push(Transition {
            target: "other".to_string(),
            guard: Arc::new(|_| true),
            description: None,
        });

        let mut machine = PhaseMachine::new("end");
        machine.add_phase(term);
        machine.add_phase(simple_phase("other", "Other"));

        assert_eq!(machine.evaluate(&state), None);
    }

    // ── 6. transition updates current and records history ───────────────

    #[tokio::test]
    async fn transition_updates_current_and_records_history() {
        let writer: Arc<dyn SessionWriter> = Arc::new(crate::test_helpers::MockWriter);
        let state = State::new();

        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(simple_phase("greeting", "Say hello"));
        machine.add_phase(simple_phase("main", "Main phase instruction"));

        let trigger = TransitionTrigger::Guard { transition_index: 0 };
        let tw = empty_tw();
        let result = machine.transition("main", &state, &writer, 1, trigger, &tw).await;
        assert_eq!(result.as_ref().map(|r| r.instruction.as_str()), Some("Main phase instruction"));
        assert_eq!(machine.current(), "main");
        assert_eq!(machine.history().len(), 1);
        assert_eq!(machine.history()[0].from, "greeting");
        assert_eq!(machine.history()[0].to, "main");
        assert_eq!(machine.history()[0].turn, 1);
        assert!(matches!(
            machine.history()[0].trigger,
            TransitionTrigger::Guard { transition_index: 0 }
        ));
    }

    // ── 7. active_tools returns correct filter ──────────────────────────

    #[test]
    fn active_tools_returns_filter() {
        let mut phase = simple_phase("filtered", "Filtered phase");
        phase.tools_enabled = Some(vec!["search".to_string(), "lookup".to_string()]);

        let mut machine = PhaseMachine::new("filtered");
        machine.add_phase(phase);

        let tools = machine.active_tools().unwrap();
        assert_eq!(tools.len(), 2);
        assert!(tools.contains(&"search".to_string()));
        assert!(tools.contains(&"lookup".to_string()));
    }

    // ── 8. active_tools returns None when no filter set ─────────────────

    #[test]
    fn active_tools_returns_none_when_no_filter() {
        let mut machine = PhaseMachine::new("open");
        machine.add_phase(simple_phase("open", "All tools allowed"));

        assert!(machine.active_tools().is_none());
    }

    // ── 9. validate catches missing initial phase ───────────────────────

    #[test]
    fn validate_catches_missing_initial_phase() {
        let mut machine = PhaseMachine::new("nonexistent");
        machine.add_phase(simple_phase("greeting", "Hi"));

        let err = machine.validate().unwrap_err();
        assert!(err.contains("initial phase 'nonexistent' not found"));
    }

    // ── 10. validate catches invalid transition target ──────────────────

    #[test]
    fn validate_catches_invalid_transition_target() {
        let mut greeting = simple_phase("greeting", "Hi");
        greeting.transitions.push(Transition {
            target: "missing_phase".to_string(),
            guard: Arc::new(|_| true),
            description: None,
        });

        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(greeting);

        let err = machine.validate().unwrap_err();
        assert!(err.contains("unknown target 'missing_phase'"));
    }

    // ── 11. validate succeeds on valid config ───────────────────────────

    #[test]
    fn validate_succeeds_on_valid_config() {
        let mut greeting = simple_phase("greeting", "Hi");
        greeting.transitions.push(Transition {
            target: "main".to_string(),
            guard: Arc::new(|_| true),
            description: None,
        });

        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(greeting);
        machine.add_phase(simple_phase("main", "Main"));

        assert!(machine.validate().is_ok());
    }

    // ── 12. PhaseInstruction::Static resolves correctly ─────────────────

    #[test]
    fn phase_instruction_static_resolves() {
        let state = State::new();
        let instr = PhaseInstruction::Static("You are a helpful assistant.".to_string());
        assert_eq!(instr.resolve(&state), "You are a helpful assistant.");
    }

    // ── 13. PhaseInstruction::Dynamic resolves correctly ────────────────

    #[test]
    fn phase_instruction_dynamic_resolves() {
        let state = State::new();
        state.set("user_name", "Alice");

        let instr = PhaseInstruction::Dynamic(Arc::new(|s: &State| {
            let name: String = s.get("user_name").unwrap_or_default();
            format!("Greet the user named {}.", name)
        }));

        assert_eq!(instr.resolve(&state), "Greet the user named Alice.");
    }

    // ── validate catches empty phases ───────────────────────────────────

    #[test]
    fn validate_catches_no_phases() {
        let machine = PhaseMachine::new("greeting");
        let err = machine.validate().unwrap_err();
        assert!(err.contains("no phases registered"));
    }

    // ── transition to nonexistent target returns None ────────────────────

    #[tokio::test]
    async fn transition_to_nonexistent_target_returns_none() {
        let writer: Arc<dyn SessionWriter> = Arc::new(crate::test_helpers::MockWriter);
        let state = State::new();

        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(simple_phase("greeting", "Hi"));

        let trigger = TransitionTrigger::Programmatic { source: "test" };
        let tw = empty_tw();
        let result = machine.transition("no_such_phase", &state, &writer, 0, trigger, &tw).await;
        assert!(result.is_none());
        // Current phase should remain unchanged.
        assert_eq!(machine.current(), "greeting");
    }

    // ── transition runs on_enter and on_exit callbacks ────────────────────

    #[tokio::test]
    async fn transition_runs_on_enter_and_on_exit_callbacks() {
        let writer: Arc<dyn SessionWriter> = Arc::new(crate::test_helpers::MockWriter);
        let state = State::new();

        let mut greeting = simple_phase("greeting", "Hi");
        greeting.on_exit = Some(Arc::new(|s: State, _w: Arc<dyn SessionWriter>| {
            Box::pin(async move {
                s.set("exited_greeting", true);
            })
        }));

        let mut main = simple_phase("main", "Main");
        main.on_enter = Some(Arc::new(|s: State, _w: Arc<dyn SessionWriter>| {
            Box::pin(async move {
                s.set("entered_main", true);
            })
        }));

        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(greeting);
        machine.add_phase(main);

        let trigger = TransitionTrigger::Programmatic { source: "test" };
        let tw = empty_tw();
        machine.transition("main", &state, &writer, 1, trigger, &tw).await;

        assert_eq!(state.get::<bool>("exited_greeting"), Some(true));
        assert_eq!(state.get::<bool>("entered_main"), Some(true));
    }

    // ── multiple transitions accumulate history ──────────────────────────

    #[tokio::test]
    async fn multiple_transitions_accumulate_history() {
        let writer: Arc<dyn SessionWriter> = Arc::new(crate::test_helpers::MockWriter);
        let state = State::new();

        let mut machine = PhaseMachine::new("a");
        machine.add_phase(simple_phase("a", "Phase A"));
        machine.add_phase(simple_phase("b", "Phase B"));
        machine.add_phase(simple_phase("c", "Phase C"));

        let trigger1 = TransitionTrigger::Guard { transition_index: 0 };
        let tw = empty_tw();
        machine.transition("b", &state, &writer, 1, trigger1, &tw).await;
        let trigger2 = TransitionTrigger::Programmatic { source: "test" };
        machine.transition("c", &state, &writer, 3, trigger2, &tw).await;

        assert_eq!(machine.current(), "c");
        assert_eq!(machine.history().len(), 2);
        assert_eq!(machine.history()[0].from, "a");
        assert_eq!(machine.history()[0].to, "b");
        assert_eq!(machine.history()[0].turn, 1);
        assert!(matches!(
            machine.history()[0].trigger,
            TransitionTrigger::Guard { transition_index: 0 }
        ));
        assert_eq!(machine.history()[1].from, "b");
        assert_eq!(machine.history()[1].to, "c");
        assert_eq!(machine.history()[1].turn, 3);
        assert!(matches!(
            machine.history()[1].trigger,
            TransitionTrigger::Programmatic { source: "test" }
        ));
    }

    // ── dynamic instruction resolved during transition ──────────────────

    #[tokio::test]
    async fn transition_resolves_dynamic_instruction() {
        let writer: Arc<dyn SessionWriter> = Arc::new(crate::test_helpers::MockWriter);
        let state = State::new();
        state.set("topic", "weather");

        let dynamic_phase = Phase {
            name: "dynamic".to_string(),
            instruction: PhaseInstruction::Dynamic(Arc::new(|s: &State| {
                let topic: String = s.get("topic").unwrap_or_default();
                format!("Discuss {}.", topic)
            })),
            tools_enabled: None,
            guard: None,
            on_enter: None,
            on_exit: None,
            transitions: Vec::new(),
            terminal: false,
            modifiers: Vec::new(),
            prompt_on_enter: false,
            on_enter_context: None,
            needs: Vec::new(),
        };

        let mut machine = PhaseMachine::new("start");
        machine.add_phase(simple_phase("start", "Begin"));
        machine.add_phase(dynamic_phase);

        let trigger = TransitionTrigger::Programmatic { source: "test" };
        let tw = empty_tw();
        let result = machine.transition("dynamic", &state, &writer, 1, trigger, &tw).await;
        assert_eq!(result.as_ref().map(|r| r.instruction.as_str()), Some("Discuss weather."));
    }

    // ── phase-level guard blocks transition into guarded phase ─────────

    #[test]
    fn phase_guard_blocks_transition() {
        let state = State::new();
        state.set("ready", true);
        // "verified" is NOT set, so the target phase guard will reject entry.

        let mut greeting = simple_phase("greeting", "Say hello");
        greeting.transitions.push(Transition {
            target: "secure".to_string(),
            guard: Arc::new(|s: &State| s.get::<bool>("ready").unwrap_or(false)),
            description: None,
        });

        // Target phase has a guard that requires "verified" to be true.
        let mut secure = simple_phase("secure", "Secure area");
        secure.guard = Some(Arc::new(|s: &State| {
            s.get::<bool>("verified").unwrap_or(false)
        }));

        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(greeting);
        machine.add_phase(secure);

        // Transition guard fires (ready=true), but target phase guard blocks
        // (verified is not set), so evaluate returns None.
        assert_eq!(machine.evaluate(&state), None);
    }

    #[test]
    fn phase_guard_allows_transition_when_satisfied() {
        let state = State::new();
        state.set("ready", true);
        state.set("verified", true);

        let mut greeting = simple_phase("greeting", "Say hello");
        greeting.transitions.push(Transition {
            target: "secure".to_string(),
            guard: Arc::new(|s: &State| s.get::<bool>("ready").unwrap_or(false)),
            description: None,
        });

        // Target phase guard requires "verified" — which IS set.
        let mut secure = simple_phase("secure", "Secure area");
        secure.guard = Some(Arc::new(|s: &State| {
            s.get::<bool>("verified").unwrap_or(false)
        }));

        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(greeting);
        machine.add_phase(secure);

        // Both transition guard and phase guard pass (index 0).
        assert_eq!(machine.evaluate(&state), Some(("secure", 0)));
    }

    #[test]
    fn phase_guard_skips_to_next_transition() {
        let state = State::new();
        state.set("ready", true);
        // "verified" is NOT set — first target's phase guard will block.

        let mut greeting = simple_phase("greeting", "Say hello");
        // First transition → "secure" (phase guard will block)
        greeting.transitions.push(Transition {
            target: "secure".to_string(),
            guard: Arc::new(|s: &State| s.get::<bool>("ready").unwrap_or(false)),
            description: None,
        });
        // Second transition → "fallback" (no phase guard)
        greeting.transitions.push(Transition {
            target: "fallback".to_string(),
            guard: Arc::new(|s: &State| s.get::<bool>("ready").unwrap_or(false)),
            description: None,
        });

        let mut secure = simple_phase("secure", "Secure area");
        secure.guard = Some(Arc::new(|s: &State| {
            s.get::<bool>("verified").unwrap_or(false)
        }));

        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(greeting);
        machine.add_phase(secure);
        machine.add_phase(simple_phase("fallback", "Fallback"));

        // First transition fires but phase guard blocks → falls through to
        // second transition (index 1) which has no phase guard → returns "fallback".
        assert_eq!(machine.evaluate(&state), Some(("fallback", 1)));
    }

    // ── InstructionModifier tests ──────────────────────────────────────

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

    // ── describe_navigation tests ──────────────────────────────────────

    #[test]
    fn describe_navigation_basic() {
        let state = State::new();
        state.set("caller_name", "Vamsi");

        let mut machine = PhaseMachine::new("greeting");

        let mut greeting = Phase::new("greeting", "Greet the caller warmly and ask who is calling.");
        greeting.transitions.push(Transition {
            target: "identify".to_string(),
            guard: Arc::new(|_| false),
            description: Some("after initial greeting".into()),
        });
        machine.add_phase(greeting);

        let mut identify = Phase::new("identify", "Get the caller's name.");
        identify.needs = vec!["caller_name".into(), "caller_org".into()];
        identify.transitions.push(Transition {
            target: "purpose".to_string(),
            guard: Arc::new(|_| false),
            description: Some("when caller is identified".into()),
        });
        machine.add_phase(identify);

        let nav = machine.describe_navigation(&state);
        assert!(nav.contains("[Navigation]"));
        assert!(nav.contains("Current phase: greeting"));
        assert!(nav.contains("→ identify: after initial greeting"));
    }

    #[test]
    fn describe_navigation_with_history_and_needs() {
        let state = State::new();
        // caller_name is set, caller_org is NOT set
        state.set("caller_name", "Vamsi");

        let mut machine = PhaseMachine::new("identify");

        let greeting = Phase::new("greeting", "Greet caller.");
        machine.add_phase(greeting);

        let mut identify = Phase::new("identify", "Get the caller's name and organization.");
        identify.needs = vec!["caller_name".into(), "caller_org".into()];
        identify.transitions.push(Transition {
            target: "purpose".to_string(),
            guard: Arc::new(|_| false),
            description: Some("when caller is identified".into()),
        });
        machine.add_phase(identify);

        let purpose = Phase::new("purpose", "Ask why they are calling.");
        machine.add_phase(purpose);

        // Simulate history: greeting -> identify at turn 2
        machine.history_mut().push_back(PhaseTransition {
            from: "greeting".to_string(),
            to: "identify".to_string(),
            turn: 2,
            trigger: TransitionTrigger::Guard { transition_index: 0 },
            timestamp: std::time::Instant::now(),
            duration_in_phase: Duration::from_secs(5),
        });

        let nav = machine.describe_navigation(&state);
        assert!(nav.contains("Previous:"), "Should show history");
        assert!(nav.contains("greeting"), "Should mention previous phase");
        assert!(nav.contains("Still needed: caller_org"), "caller_org should be listed as needed (caller_name is set)");
        assert!(!nav.contains("caller_name"), "caller_name should NOT be in still-needed (it's set)");
    }

    #[test]
    fn describe_navigation_terminal_phase() {
        let state = State::new();
        let mut machine = PhaseMachine::new("farewell");

        let mut farewell = Phase::new("farewell", "Say goodbye.");
        farewell.terminal = true;
        machine.add_phase(farewell);

        let nav = machine.describe_navigation(&state);
        assert!(nav.contains("final phase"));
    }
}
