//! Model-free conversation simulation.
//!
//! A deterministic harness that drives a [`CompiledConversation`] without any live
//! API: a **fake user** supplies utterances (run through the conversation's
//! recognizers) or sets slots directly, tools succeed on demand or after a
//! latency, and the [`FlowStack`] advances turn by turn. Everything is driven by
//! `State` + guards, so motifs, repair, policies, and digressions become testable
//! in CI — "a flow SDK with simulation is infra; without it, a demo framework".
//!
//! ```ignore
//! let convo = Conversation::new("booking")./* … */.compile()?;
//! let mut sim = Sim::new(&convo, FlowMode::Enforce);
//! sim.user("a table for 4 tomorrow at 7pm").await;
//! assert!(sim.active().contains(&"check".to_string()));
//! assert!(!sim.allowed("book"));            // not confirmed yet
//! sim.set("user_confirmed", true);
//! sim.tool_ok("book");
//! assert!(sim.is_complete());
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

    /// Active step ids in the currently-driving layer.
    pub fn active(&self) -> Vec<String> {
        self.stack.explain(&self.state).active
    }

    /// The active digression, if one is suspending the main flow.
    pub fn active_overlay(&self) -> Option<&str> {
        self.stack.active_overlay()
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// A tool succeeds now.
    ToolOk(String),
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    /// Scenario name (for diagnostics).
    pub name: String,
    /// The steps to execute, in order.
    pub steps: Vec<SimStep>,
}

impl Scenario {
    /// Run the scenario against `convo`. Returns `Ok(())` if every `Expect*` step
    /// holds, else `Err` with the failing step index and a diagnostic.
    pub async fn run(&self, convo: &CompiledConversation, mode: Enforcement) -> Result<(), String> {
        let mut sim = Sim::new(convo, mode);
        for (i, step) in self.steps.iter().enumerate() {
            let fail = |msg: String| Err(format!("[{}] step {i} ({step:?}): {msg}", self.name));
            match step {
                SimStep::User(text) => {
                    sim.user(text).await;
                }
                SimStep::Set { key, value } => {
                    sim.set(key.clone(), value.clone());
                }
                SimStep::ToolOk(tool) => {
                    sim.tool_ok(tool);
                }
                SimStep::ScheduleTool { tool, after } => {
                    sim.schedule_tool(tool.clone(), *after);
                }
                SimStep::Turn => {
                    sim.turn();
                }
                SimStep::ExpectActive(expected) => {
                    let active = sim.active();
                    for e in expected {
                        if !active.contains(e) {
                            return fail(format!("expected active '{e}', got {active:?}"));
                        }
                    }
                }
                SimStep::ExpectDenied(tool) => {
                    if sim.allowed(tool) {
                        return fail(format!("expected '{tool}' denied, but it was admitted"));
                    }
                }
                SimStep::ExpectAllowed(tool) => {
                    if !sim.allowed(tool) {
                        let why = sim.denied().get(tool).cloned().unwrap_or_default();
                        return fail(format!("expected '{tool}' allowed, but denied: {why}"));
                    }
                }
                SimStep::ExpectSlot { key, value } => {
                    let got = sim.state().get_raw(key);
                    if got.as_ref() != Some(value) {
                        return fail(format!("expected slot '{key}' = {value}, got {got:?}"));
                    }
                }
                SimStep::ExpectComplete => {
                    if !sim.is_complete() {
                        return fail("expected conversation complete".into());
                    }
                }
            }
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
