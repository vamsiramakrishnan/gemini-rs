//! Model-free conversation simulation.
//!
//! A deterministic harness that drives a [`CompiledConversation`] without any live
//! API: a **fake user** supplies utterances (run through the conversation's
//! recognizers) or sets slots directly, tools succeed on demand or after a
//! latency, and the [`FlowStack`] advances turn by turn. Everything is driven by
//! `State` + guards, so motifs, repair, policies, and digressions become testable
//! in CI — "a flow SDK with simulation is infra; without it, a demo framework".
//!
//! ```no_run
//! # use gemini_adk_fluent_rs::prelude::*;
//! # use gemini_adk_fluent_rs::conversation::Conversation;
//! # use gemini_adk_fluent_rs::simulation::Sim;
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let convo = Conversation::new("booking")
//!     .stage("check").collect(["party_size", "slot"])
//!         .next("confirm", Guard::captured(["party_size", "slot"]))
//!     .stage("confirm").commit("book", Guard::is_true("user_confirmed"))
//!         .next("done", Guard::called_ok("book"))
//!     .stage("done").terminal()
//!     .require(["done"])
//!     .compile()?;
//! let mut sim = Sim::new(&convo, Enforcement::Enforce);
//! sim.user("a table for 4 tomorrow at 7pm").await;
//! assert!(sim.active().contains(&"check".to_string()));
//! assert!(!sim.allowed("book"));            // not confirmed yet
//! sim.set("user_confirmed", true);
//! sim.tool_ok("book");
//! assert!(sim.is_complete());
//! # Ok(())
//! # }
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use gemini_adk_rs::flow::{Enforcement, FlowExplanation};
use gemini_adk_rs::live::{TranscriptTurn, TurnExtractor};
use gemini_adk_rs::state::State;

use crate::conversation::{CompiledConversation, FlowStack};

struct BoundExtractor {
    extractor: Arc<dyn TurnExtractor>,
    /// (field name, state key) — how to promote the returned record into `State`.
    fields: Vec<(String, String)>,
}

/// A deterministic, model-free driver for a compiled conversation.
pub struct Sim {
    stack: FlowStack,
    extractors: Vec<BoundExtractor>,
    state: State,
    turn_no: u32,
    /// Tools scheduled to succeed at a future turn — models tool latency.
    pending_tools: Vec<(String, u32)>,
}

impl Sim {
    /// Build a simulator over `convo` in the given enforcement mode.
    pub fn new(convo: &CompiledConversation, mode: Enforcement) -> Self {
        let extractors = convo
            .all_extractors()
            .into_iter()
            .map(|e| BoundExtractor {
                fields: e.field_state_keys(),
                extractor: e.into_extractor(),
            })
            .collect();
        Self {
            stack: convo.stack(mode),
            extractors,
            state: State::new(),
            turn_no: 0,
            pending_tools: Vec::new(),
        }
    }

    /// Set a state value directly (information a recognizer can't supply, or a
    /// scripted shortcut). Does not advance a turn.
    pub fn set(&self, key: impl Into<String>, value: impl Serialize) -> &Self {
        let _ = self.state.set(key, value);
        self
    }

    /// The fake user speaks: run the conversation's extractors over the utterance
    /// to fill slots (respecting validators), then advance a turn.
    pub async fn user(&mut self, utterance: &str) -> &mut Self {
        let window = [TranscriptTurn {
            turn_number: self.turn_no,
            user: utterance.to_string(),
            model: String::new(),
            tool_calls: Vec::new(),
            timestamp: Instant::now(),
        }];
        for bound in &self.extractors {
            if let Ok(Value::Object(obj)) = bound
                .extractor
                .extract_with_state(&window, &self.state)
                .await
            {
                for (name, key) in &bound.fields {
                    if let Some(v) = obj.get(name)
                        && !v.is_null()
                    {
                        let _ = self.state.set(key.clone(), v.clone());
                    }
                }
            }
        }
        self.advance();
        self
    }

    /// Advance a turn with no new user input (e.g. waiting on a tool/resolver).
    pub fn turn(&mut self) -> &mut Self {
        self.advance();
        self
    }

    /// A tool succeeds now; records it and advances a turn (processing any
    /// digression resume).
    pub fn tool_ok(&mut self, tool: &str) -> &mut Self {
        self.stack.on_tool_ok(tool, &self.state);
        self.advance();
        self
    }

    /// A tool fails or times out. Counts toward the active stage's
    /// `escalate_after_tool_failures`; does not advance a turn.
    pub fn tool_failed(&mut self, tool: &str) -> &mut Self {
        self.stack.observe_tool(tool, false, &self.state);
        self
    }

    /// The user barges in on the model. Counts toward the active stage's
    /// `escalate_after_interruptions`; does not advance a turn.
    pub fn interrupt(&mut self) -> &mut Self {
        self.stack.on_interrupted(&self.state);
        self
    }

    /// Schedule a tool to succeed `after` turns — models tool latency.
    pub fn schedule_tool(&mut self, tool: impl Into<String>, after: u32) -> &mut Self {
        self.pending_tools
            .push((tool.into(), self.turn_no + after.max(1)));
        self
    }

    fn advance(&mut self) {
        self.turn_no += 1;
        // Fire any tools whose latency has elapsed.
        let due: Vec<String> = self
            .pending_tools
            .iter()
            .filter(|(_, at)| *at <= self.turn_no)
            .map(|(t, _)| t.clone())
            .collect();
        self.pending_tools.retain(|(_, at)| *at > self.turn_no);
        for tool in due {
            self.stack.on_tool_ok(&tool, &self.state);
        }
        self.stack.on_turn(&self.state);
    }

    /// Apply one scripted step: drive the conversation, or check an
    /// expectation. `Err` carries why an expectation did not hold. This is
    /// what [`Scenario::run`] does for each step, exposed so another driver
    /// (an interactive session, a binding) shares the exact semantics.
    pub async fn apply(&mut self, step: &SimStep) -> Result<(), String> {
        match step {
            SimStep::User(text) => {
                self.user(text).await;
            }
            SimStep::Set { key, value } => {
                self.set(key.clone(), value.clone());
            }
            SimStep::Remove { key } => {
                let _ = self.state.remove(key);
            }
            SimStep::ToolOk(tool) => {
                self.tool_ok(tool);
            }
            SimStep::ToolFailed(tool) => {
                self.tool_failed(tool);
            }
            SimStep::Interrupt => {
                self.interrupt();
            }
            SimStep::ToolResult { tool, ok } => {
                self.stack.observe_tool(tool, *ok, &self.state);
            }
            SimStep::ScheduleTool { tool, after } => {
                self.schedule_tool(tool.clone(), *after);
            }
            SimStep::Turn => {
                self.turn();
            }
            SimStep::ExpectActive(expected) => {
                let active = self.active();
                for e in expected {
                    if !active.contains(e) {
                        return Err(format!("expected active '{e}', got {active:?}"));
                    }
                }
            }
            SimStep::ExpectDenied(tool) => {
                if self.allowed(tool) {
                    return Err(format!("expected '{tool}' denied, but it was admitted"));
                }
            }
            SimStep::ExpectAllowed(tool) => {
                if !self.allowed(tool) {
                    let why = self.denied().get(tool).cloned().unwrap_or_default();
                    return Err(format!("expected '{tool}' allowed, but denied: {why}"));
                }
            }
            SimStep::ExpectSlot { key, value } => {
                let got = self.state().get_raw(key);
                if got.as_ref() != Some(value) {
                    return Err(format!("expected slot '{key}' = {value}, got {got:?}"));
                }
            }
            SimStep::ExpectComplete => {
                if !self.is_complete() {
                    return Err("expected conversation complete".into());
                }
            }
        }
        Ok(())
    }

    /// Active step ids in the currently-driving layer.
    pub fn active(&self) -> Vec<String> {
        self.stack.explain(&self.state).active
    }

    /// The active digression, if one is suspending the main flow.
    pub fn active_overlay(&self) -> Option<&str> {
        self.stack.active_overlay()
    }

    /// The instructions (stage postures) projected to the model this turn.
    ///
    /// On the turn a digression completes, these are its *closing* stage's —
    /// the safety hand-off's "hand off to a human now", say — which is what a
    /// live session would send. Assert on these to test that a digression is
    /// heard, not merely that it fired.
    pub fn postures(&self) -> Vec<String> {
        self.stack.active_postures(&self.state)
    }

    /// Whether the conversation has ended because a `Resume::Terminate`
    /// digression ran (as opposed to the main flow finishing).
    pub fn is_terminated(&self) -> bool {
        self.stack.is_terminated()
    }

    /// Whether `tool` is admitted right now.
    pub fn allowed(&self, tool: &str) -> bool {
        self.stack.admits_tool(tool, &self.state).is_ok()
    }

    /// Currently-blocked tools, mapped to the reason.
    pub fn denied(&self) -> BTreeMap<String, String> {
        self.stack.explain(&self.state).blocked_tools
    }

    /// Whether the conversation is complete.
    pub fn is_complete(&self) -> bool {
        self.stack.is_complete()
    }

    /// Read a slot value.
    pub fn slot<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.state.get(key)
    }

    /// The active layer's control-plane explanation.
    pub fn explain(&self) -> FlowExplanation {
        self.stack.explain(&self.state)
    }

    /// The simulation state (for custom assertions / slot evidence).
    pub fn state(&self) -> &State {
        &self.state
    }
}

/// One step in a serializable [`Scenario`].
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SimStep {
    /// The fake user speaks (recognizers fill slots), then a turn advances.
    User(String),
    /// Set a state value directly.
    Set {
        /// State key.
        key: String,
        /// Value to store.
        value: Value,
    },
    /// Remove a state value, as the recorded runtime did.
    Remove {
        /// State key.
        key: String,
    },
    /// A tool succeeds now.
    ToolOk(String),
    /// A tool fails (or times out) now; counts toward the active stage's
    /// repair policy. Does not advance a turn.
    ToolFailed(String),
    /// The user barges in on the model; counts toward the active stage's
    /// repair policy. Does not advance a turn.
    Interrupt,
    /// A tool call completes, successfully or not, where the live runtime
    /// records it: the flow observes it without advancing a turn (unlike
    /// [`ToolOk`](Self::ToolOk)). This is what a scenario extracted from a
    /// recording uses.
    ToolResult {
        /// Tool name.
        tool: String,
        /// Whether it succeeded.
        ok: bool,
    },
    /// Schedule a tool to succeed after N turns (latency).
    ScheduleTool {
        /// Tool name.
        tool: String,
        /// Turns to wait.
        after: u32,
    },
    /// Advance a turn with no input.
    Turn,
    /// Assert these step ids are active.
    ExpectActive(Vec<String>),
    /// Assert a tool is currently blocked.
    ExpectDenied(String),
    /// Assert a tool is currently admitted.
    ExpectAllowed(String),
    /// Assert a slot equals a value.
    ExpectSlot {
        /// State key.
        key: String,
        /// Expected value.
        value: Value,
    },
    /// Assert the conversation is complete.
    ExpectComplete,
}

/// A serializable simulation script — a deterministic, model-free test case that
/// can be authored in code or loaded from YAML/JSON.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Scenario {
    /// Scenario name (for diagnostics).
    pub name: String,
    /// The steps to execute, in order.
    pub steps: Vec<SimStep>,
}

/// State keys the runtime owns. A scenario extracted from a recording skips
/// them: the simulator recomputes them rather than being told them.
const RUNTIME_PREFIXES: &[&str] = &[
    "session:",
    "flow:",
    "derived:",
    "repair:",
    "correction:",
    "state_meta:",
    "idempotency:",
    "compensated:",
    "verbatim:",
    "turn:",
    "bg:",
];

fn runtime_owned(key: &str) -> bool {
    RUNTIME_PREFIXES.iter().any(|p| key.starts_with(p))
}

impl Scenario {
    /// Turn a recorded session into a regression scenario: the incident
    /// becomes a test.
    ///
    /// `journal` is the session's mutation journal (see
    /// [`FileJournalSink`](gemini_adk_rs::state::FileJournalSink) and
    /// [`read_journal`](gemini_adk_rs::state::read_journal)), which a
    /// governed session writes as one ordered timeline. The scenario replays
    /// what the application and user contributed and checks what governance
    /// decided:
    ///
    /// - a slot or flag write becomes `Set`;
    /// - a tool the flow admitted becomes `ExpectAllowed`, one it refused
    ///   `ExpectDenied`, and its outcome `ToolResult`;
    /// - every turn boundary where the flow was evaluated (a `flow:active`
    ///   write) becomes a `Turn` followed by `ExpectActive` with the steps
    ///   the session had then.
    ///
    /// Keys the runtime owns (`session:`, `flow:`, `derived:`, repair and
    /// correction signals, …) are not replayed: the simulator must reach them
    /// itself.
    ///
    /// Run the result against the conversation spec in CI. If a change to
    /// the spec alters what that session would have done, the scenario fails
    /// at the step where the two diverge.
    pub fn from_journal(
        name: impl Into<String>,
        journal: &[gemini_adk_rs::state::StateMutation],
    ) -> Self {
        use gemini_adk_rs::flow::{TOOL_CALL_KEY, TOOL_DENIED_KEY, TOOL_RESULT_KEY};

        let tool_of = |v: &Option<Value>| {
            v.as_ref()
                .and_then(|v| v["tool"].as_str())
                .map(str::to_string)
        };
        let mut steps = Vec::new();
        // `None` is a removal (`State::remove`, `clear_prefix`), which must
        // replay as one: a key set to `null` is still present.
        let mut pending: Vec<(String, Option<Value>)> = Vec::new();
        let flush = |pending: &mut Vec<(String, Option<Value>)>, steps: &mut Vec<SimStep>| {
            for (key, value) in pending.drain(..) {
                steps.push(match value {
                    Some(value) => SimStep::Set { key, value },
                    None => SimStep::Remove { key },
                });
            }
        };
        let mut ordered: Vec<&gemini_adk_rs::state::StateMutation> = journal.iter().collect();
        ordered.sort_by_key(|m| m.sequence);
        for m in ordered {
            match m.key.as_str() {
                "flow:active" => {
                    flush(&mut pending, &mut steps);
                    steps.push(SimStep::Turn);
                    let active: Vec<String> = m
                        .new
                        .clone()
                        .and_then(|v| serde_json::from_value(v).ok())
                        .unwrap_or_default();
                    steps.push(SimStep::ExpectActive(active));
                }
                TOOL_CALL_KEY => {
                    if let Some(tool) = tool_of(&m.new) {
                        flush(&mut pending, &mut steps);
                        steps.push(SimStep::ExpectAllowed(tool));
                    }
                }
                TOOL_DENIED_KEY => {
                    if let Some(tool) = tool_of(&m.new) {
                        flush(&mut pending, &mut steps);
                        steps.push(SimStep::ExpectDenied(tool));
                    }
                }
                TOOL_RESULT_KEY => {
                    if let Some(tool) = tool_of(&m.new) {
                        flush(&mut pending, &mut steps);
                        let ok = m.new.as_ref().is_some_and(|v| v["ok"] == true);
                        steps.push(SimStep::ToolResult { tool, ok });
                    }
                }
                key if runtime_owned(key) => {}
                key => {
                    let value = m.new.clone();
                    // Keep the latest value per key, in first-written order.
                    match pending.iter_mut().find(|(k, _)| k == key) {
                        Some(slot) => slot.1 = value,
                        None => pending.push((key.to_string(), value)),
                    }
                }
            }
        }
        flush(&mut pending, &mut steps);
        Self {
            name: name.into(),
            steps,
        }
    }

    /// Run the scenario against `convo`. Returns `Ok(())` if every `Expect*` step
    /// holds, else `Err` with the failing step index and a diagnostic.
    pub async fn run(&self, convo: &CompiledConversation, mode: Enforcement) -> Result<(), String> {
        let mut sim = Sim::new(convo, mode);
        for (i, step) in self.steps.iter().enumerate() {
            sim.apply(step)
                .await
                .map_err(|msg| format!("[{}] step {i} ({step:?}): {msg}", self.name))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::Conversation;
    use gemini_adk_rs::flow::Guard;
    use gemini_adk_rs::frame::{Frame, FrameSpec, SlotRecognizer, SlotSpec};

    struct Booking;
    impl Frame for Booking {
        fn frame() -> FrameSpec {
            FrameSpec {
                name: "booking".into(),
                slots: vec![SlotSpec {
                    recognizer: Some(SlotRecognizer::IntegerNear(vec!["people".into()])),
                    ..SlotSpec::new("party_size")
                }],
            }
        }
    }

    fn booking() -> CompiledConversation {
        Conversation::new("booking")
            .stage("collect")
            .collect_frame::<Booking>()
            .next("confirm", Guard::captured(["party_size"]))
            .stage("confirm")
            .commit("book", Guard::is_true("user_confirmed"))
            .next("done", Guard::called_ok("book"))
            .stage("done")
            .terminal()
            .require(["done"])
            .compile()
            .expect("compiles")
    }

    #[tokio::test]
    async fn fake_user_fills_slots_and_gates_commit() {
        let convo = booking();
        let mut sim = Sim::new(&convo, Enforcement::Enforce);

        assert!(sim.active().contains(&"collect".to_string()));
        assert!(!sim.allowed("book"));

        // The fake user speaks; the recognizer fills party_size.
        sim.user("a table for 4 people").await;
        assert_eq!(sim.slot::<u32>("party_size"), Some(4));
        assert!(sim.active().contains(&"confirm".to_string()));

        // book is gated until confirmation.
        assert!(!sim.allowed("book"));
        sim.set("user_confirmed", true);
        sim.turn();
        assert!(sim.allowed("book"));

        sim.tool_ok("book");
        assert!(sim.is_complete());
    }

    #[tokio::test]
    async fn scenario_runs_and_round_trips() {
        let scenario = Scenario {
            name: "happy_path".into(),
            steps: vec![
                SimStep::ExpectActive(vec!["collect".into()]),
                SimStep::ExpectDenied("book".into()),
                SimStep::User("party of 4 people".into()),
                SimStep::ExpectSlot {
                    key: "party_size".into(),
                    value: serde_json::json!(4),
                },
                SimStep::ExpectActive(vec!["confirm".into()]),
                SimStep::Set {
                    key: "user_confirmed".into(),
                    value: serde_json::json!(true),
                },
                SimStep::Turn,
                SimStep::ExpectAllowed("book".into()),
                SimStep::ToolOk("book".into()),
                SimStep::ExpectComplete,
            ],
        };

        scenario
            .run(&booking(), Enforcement::Enforce)
            .await
            .expect("scenario passes");

        // Scenarios are serializable (authorable as YAML/JSON).
        let json = serde_json::to_string(&scenario).unwrap();
        let back: Scenario = serde_json::from_str(&json).unwrap();
        back.run(&booking(), Enforcement::Enforce)
            .await
            .expect("round-tripped scenario passes");
    }

    #[tokio::test]
    async fn scenario_reports_failed_expectation() {
        let scenario = Scenario {
            name: "bad".into(),
            steps: vec![SimStep::ExpectComplete], // not complete at the start
        };
        let err = scenario
            .run(&booking(), Enforcement::Enforce)
            .await
            .unwrap_err();
        assert!(err.contains("expected conversation complete"));
    }

    #[tokio::test]
    async fn tool_latency_resolves_after_delay() {
        let convo = booking();
        let mut sim = Sim::new(&convo, Enforcement::Enforce);
        sim.user("4 people").await;
        sim.set("user_confirmed", true);
        sim.turn();
        // book completes after 2 turns of latency rather than immediately.
        sim.schedule_tool("book", 2);
        assert!(!sim.is_complete());
        sim.turn();
        assert!(!sim.is_complete());
        sim.turn();
        assert!(sim.is_complete());
    }
}
