//! Declarative conversation phase management.
//!
//! A [`PhaseMachine`] holds named [`Phase`]s and evaluates guard-based
//! transitions. Each phase carries an instruction (static or dynamic),
//! optional tool filters, and async entry/exit callbacks.
//!
//! The machine is owned by the control-lane task; no internal locking is
//! required — `&self` for reads, `&mut self` for mutations.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use rs_genai::session::SessionWriter;

use super::BoxFuture;
use crate::state::State;

// ── Core types ──────────────────────────────────────────────────────────────

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
}

/// A guard-based transition to a named target phase.
pub struct Transition {
    /// Name of the target phase.
    pub target: String,
    /// Guard function — transition fires when this returns `true`.
    pub guard: Arc<dyn Fn(&State) -> bool + Send + Sync>,
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
}

// ── PhaseMachine ────────────────────────────────────────────────────────────

/// Evaluates transitions and manages phase entry/exit lifecycle.
pub struct PhaseMachine {
    phases: HashMap<String, Phase>,
    current: String,
    initial: String,
    history: Vec<PhaseTransition>,
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
            history: Vec::new(),
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

    /// The full transition history (oldest first).
    pub fn history(&self) -> &[PhaseTransition] {
        &self.history
    }

    /// Evaluate transitions from the current phase.
    ///
    /// Returns the target phase name of the first transition whose guard
    /// returns `true`, or `None` if no transition fires (or the current
    /// phase is terminal / missing).
    ///
    /// This method is **pure** — it does not modify state or execute callbacks.
    pub fn evaluate(&self, state: &State) -> Option<&str> {
        let phase = self.phases.get(&self.current)?;
        if phase.terminal {
            return None;
        }
        for transition in &phase.transitions {
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
                return Some(&transition.target);
            }
        }
        None
    }

    /// Execute a transition: run `on_exit` for the current phase, update
    /// `current`, run `on_enter` for the new phase, record history, and
    /// return the resolved instruction string for the new phase.
    ///
    /// Returns `None` if the target phase does not exist.
    pub async fn transition(
        &mut self,
        target: &str,
        state: &State,
        writer: &Arc<dyn SessionWriter>,
        turn: u32,
    ) -> Option<String> {
        // Target must exist.
        if !self.phases.contains_key(target) {
            return None;
        }

        let from = self.current.clone();

        // Run on_exit for the current phase (if it exists and has callback).
        if let Some(phase) = self.phases.get(&from) {
            if let Some(ref on_exit) = phase.on_exit {
                let fut = on_exit(state.clone(), Arc::clone(writer));
                fut.await;
            }
        }

        // Update current phase.
        self.current = target.to_string();

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
        });

        // Resolve and return instruction for the new phase.
        self.phases
            .get(target)
            .map(|p| p.instruction.resolve(state))
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
        }
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
        });

        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(greeting);
        machine.add_phase(simple_phase("main", "Main phase"));

        assert_eq!(machine.evaluate(&state), Some("main"));
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
        });
        greeting.transitions.push(Transition {
            target: "farewell".to_string(),
            guard: Arc::new(|s: &State| s.get::<bool>("done").unwrap_or(false)),
        });

        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(greeting);
        machine.add_phase(simple_phase("escalated", "Escalated"));
        machine.add_phase(simple_phase("farewell", "Farewell"));

        // Both guards are true, but "escalated" is declared first.
        assert_eq!(machine.evaluate(&state), Some("escalated"));
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

        let result = machine.transition("main", &state, &writer, 1).await;
        assert_eq!(result, Some("Main phase instruction".to_string()));
        assert_eq!(machine.current(), "main");
        assert_eq!(machine.history().len(), 1);
        assert_eq!(machine.history()[0].from, "greeting");
        assert_eq!(machine.history()[0].to, "main");
        assert_eq!(machine.history()[0].turn, 1);
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

        let result = machine.transition("no_such_phase", &state, &writer, 0).await;
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

        machine.transition("main", &state, &writer, 1).await;

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

        machine.transition("b", &state, &writer, 1).await;
        machine.transition("c", &state, &writer, 3).await;

        assert_eq!(machine.current(), "c");
        assert_eq!(machine.history().len(), 2);
        assert_eq!(machine.history()[0].from, "a");
        assert_eq!(machine.history()[0].to, "b");
        assert_eq!(machine.history()[0].turn, 1);
        assert_eq!(machine.history()[1].from, "b");
        assert_eq!(machine.history()[1].to, "c");
        assert_eq!(machine.history()[1].turn, 3);
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
        };

        let mut machine = PhaseMachine::new("start");
        machine.add_phase(simple_phase("start", "Begin"));
        machine.add_phase(dynamic_phase);

        let result = machine.transition("dynamic", &state, &writer, 1).await;
        assert_eq!(result, Some("Discuss weather.".to_string()));
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
        });

        // Target phase guard requires "verified" — which IS set.
        let mut secure = simple_phase("secure", "Secure area");
        secure.guard = Some(Arc::new(|s: &State| {
            s.get::<bool>("verified").unwrap_or(false)
        }));

        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(greeting);
        machine.add_phase(secure);

        // Both transition guard and phase guard pass.
        assert_eq!(machine.evaluate(&state), Some("secure"));
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
        });
        // Second transition → "fallback" (no phase guard)
        greeting.transitions.push(Transition {
            target: "fallback".to_string(),
            guard: Arc::new(|s: &State| s.get::<bool>("ready").unwrap_or(false)),
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
        // second transition which has no phase guard → returns "fallback".
        assert_eq!(machine.evaluate(&state), Some("fallback"));
    }
}
