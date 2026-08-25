//! Offline conformance tests embedded in a [`SessionSpec`].
//!
//! A [`SpecTest`] scripts a conversation as data — user turns, tool calls,
//! state writes — and asserts flow state at checkpoints: which steps are done
//! or active, which tools are admitted or blocked, what the state holds. The
//! script replays through the *real* [`FlowMonitor`](gemini_adk_rs::flow::FlowMonitor) with the declared tools'
//! mock semantics, so governance is exercised exactly as a live session would
//! — with no model, no network, and no API key. Run in CI, or scrub through
//! one in the Studio.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use gemini_adk_rs::flow::{Enforcement, FlowMonitor};
use gemini_adk_rs::state::State;

use super::SessionSpec;

/// One scripted event in a [`SpecTest`].
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SimEvent {
    /// A user turn (advances the turn counter and re-latches guards). The
    /// text is documentation — the simulator does not run a model.
    User(String),
    /// The model calls a declared tool. Applies the tool's `set_state` mock
    /// semantics and records a successful completion — unless the flow blocks
    /// it, in which case nothing is recorded (assert with
    /// [`TestExpectation::blocked`]).
    Tool(String),
    /// Write state directly — stands in for extraction filling slots
    /// mid-conversation.
    Set(BTreeMap<String, Value>),
    /// A checkpoint: assert the current flow state.
    Expect(TestExpectation),
}

/// Assertions at a checkpoint. Every listed item must hold; omitted fields
/// are not checked.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TestExpectation {
    /// Steps that must have latched done.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub done: Vec<String>,
    /// Steps that must be active (eligible, not done).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active: Vec<String>,
    /// Tools that must currently be admitted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<String>,
    /// Tools that must currently be blocked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked: Vec<String>,
    /// State keys that must hold exactly these values.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub state: BTreeMap<String, Value>,
    /// Whether the flow must be complete (all `require` steps done).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complete: Option<bool>,
}

/// A named, scripted conformance test embedded in the spec.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SpecTest {
    /// Test name.
    pub name: String,
    /// The scripted events, in order.
    pub script: Vec<SimEvent>,
}

/// The outcome of one scripted event.
#[derive(Debug, Clone, Serialize)]
pub struct TestStepResult {
    /// Event index in the script.
    pub index: usize,
    /// Human-readable event label.
    pub event: String,
    /// Failures at this event (empty = passed). Tool blocks are reported here
    /// when the script called a blocked tool without asserting it.
    pub failures: Vec<String>,
}

/// The outcome of one [`SpecTest`].
#[derive(Debug, Clone, Serialize)]
pub struct TestReport {
    /// Test name.
    pub name: String,
    /// Whether every assertion held.
    pub passed: bool,
    /// Per-event outcomes (only events with failures, plus a summary count).
    pub failures: Vec<TestStepResult>,
    /// Events executed.
    pub events: usize,
}

/// One per-event snapshot of the flow's state during a scripted replay — the
/// Studio's Preview scrubber steps through these, lighting up the DAG exactly
/// as a live session would, with no model and no API key.
#[derive(Debug, Clone, Serialize)]
pub struct SimSnapshot {
    /// Event index in the script (0 = state before any event).
    pub index: usize,
    /// Human-readable event label ("start", "tool: charge_card", …).
    pub event: String,
    /// Assertion failures at this event (empty when none).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
    /// Steps done after this event.
    pub done: Vec<String>,
    /// Whether the flow is complete after this event.
    pub complete: bool,
    /// The full explanation (active steps, admitted/blocked tools, unmet
    /// requirements, per-step guard truth trees) after this event.
    #[serde(flatten)]
    pub explanation: gemini_adk_rs::flow::FlowExplanation,
}

/// Replay one named test and return a snapshot after every event (plus an
/// initial "start" snapshot), for scrubbing. Errors when the flow cannot be
/// built or the test name is unknown.
pub fn trace_test(spec: &SessionSpec, test_name: &str) -> Result<Vec<SimSnapshot>, Vec<String>> {
    let flow = spec.effective_flow()?;
    let test = spec
        .tests
        .iter()
        .find(|t| t.name == test_name)
        .ok_or_else(|| vec![format!("no test named '{test_name}' in the spec")])?;
    Ok(replay(spec, flow, test))
}

/// The shared replay engine: run the script through a fresh monitor, snapshot
/// after every event.
fn replay(
    spec: &SessionSpec,
    flow: gemini_adk_rs::flow::Flow,
    test: &SpecTest,
) -> Vec<SimSnapshot> {
    let state = State::new();
    // Mirror `apply()`: declared defaults are seeded and computed variables
    // recompute after every state change, so guards over derived keys latch
    // exactly as they do live.
    spec.seed_state_defaults(&state);
    spec.recompute_computed(&state);
    let mut monitor = FlowMonitor::new(flow, Enforcement::Enforce);
    monitor.relatch(&state);

    let snapshot = |index: usize,
                    event: String,
                    failures: Vec<String>,
                    monitor: &FlowMonitor,
                    state: &State| SimSnapshot {
        index,
        event,
        failures,
        done: monitor.marking().done.iter().cloned().collect(),
        complete: monitor.is_complete(),
        explanation: monitor.explain(state),
    };

    let mut snapshots = vec![snapshot(0, "start".into(), Vec::new(), &monitor, &state)];
    for (index, event) in test.script.iter().enumerate() {
        let mut failures = Vec::new();
        let label = match event {
            SimEvent::User(text) => {
                spec.recompute_computed(&state);
                monitor.on_turn(&state);
                format!("user: {text}")
            }
            SimEvent::Tool(name) => {
                match monitor.admits_tool(name, &state) {
                    Ok(()) => {
                        spec.apply_tool_state(name, &state);
                        spec.recompute_computed(&state);
                        monitor.on_tool_ok(name, &state);
                    }
                    Err(reason) => {
                        let anticipated = matches!(
                            test.script.get(index + 1),
                            Some(SimEvent::Expect(e)) if e.blocked.iter().any(|t| t == name)
                        );
                        if !anticipated {
                            failures.push(format!("tool '{name}' was blocked: {reason}"));
                        }
                    }
                }
                format!("tool: {name}")
            }
            SimEvent::Set(map) => {
                for (key, value) in map {
                    let _ = state.set(key, value.clone());
                }
                spec.recompute_computed(&state);
                monitor.relatch(&state);
                format!(
                    "set: {}",
                    map.keys().cloned().collect::<Vec<_>>().join(", ")
                )
            }
            SimEvent::Expect(expect) => {
                check(expect, &monitor, &state, &mut failures);
                "expect".to_string()
            }
        };
        snapshots.push(snapshot(index + 1, label, failures, &monitor, &state));
    }
    snapshots
}

/// Run every embedded test in the spec against its effective flow.
pub fn run_tests(spec: &SessionSpec) -> Vec<TestReport> {
    let flow = match spec.effective_flow() {
        Ok(flow) => flow,
        Err(errors) => {
            return spec
                .tests
                .iter()
                .map(|t| TestReport {
                    name: t.name.clone(),
                    passed: false,
                    failures: vec![TestStepResult {
                        index: 0,
                        event: "setup".into(),
                        failures: errors.clone(),
                    }],
                    events: 0,
                })
                .collect();
        }
    };

    spec.tests
        .iter()
        .map(|test| run_one(spec, flow.clone(), test))
        .collect()
}

fn run_one(spec: &SessionSpec, flow: gemini_adk_rs::flow::Flow, test: &SpecTest) -> TestReport {
    let failures: Vec<TestStepResult> = replay(spec, flow, test)
        .into_iter()
        .skip(1) // the "start" snapshot carries no event
        .filter(|s| !s.failures.is_empty())
        .map(|s| TestStepResult {
            index: s.index - 1,
            event: s.event,
            failures: s.failures,
        })
        .collect();

    TestReport {
        name: test.name.clone(),
        passed: failures.is_empty(),
        failures,
        events: test.script.len(),
    }
}

fn check(
    expect: &TestExpectation,
    monitor: &FlowMonitor,
    state: &State,
    failures: &mut Vec<String>,
) {
    let explanation = monitor.explain(state);
    for step in &expect.done {
        if !monitor.marking().done.contains(step) {
            failures.push(format!(
                "expected step '{step}' done; done = [{}]",
                join(&monitor.marking().done.iter().cloned().collect::<Vec<_>>())
            ));
        }
    }
    for step in &expect.active {
        if !explanation.active.contains(step) {
            failures.push(format!(
                "expected step '{step}' active; active = [{}]",
                join(&explanation.active)
            ));
        }
    }
    for tool in &expect.allowed {
        if !explanation.allowed_tools.contains(tool) {
            failures.push(format!(
                "expected tool '{tool}' allowed; allowed = [{}]",
                join(&explanation.allowed_tools)
            ));
        }
    }
    for tool in &expect.blocked {
        if !explanation.blocked_tools.contains_key(tool) {
            failures.push(format!(
                "expected tool '{tool}' blocked; blocked = [{}]",
                join(
                    &explanation
                        .blocked_tools
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                )
            ));
        }
    }
    for (key, expected) in &expect.state {
        let actual = state.get::<Value>(key);
        if actual.as_ref() != Some(expected) {
            failures.push(format!(
                "expected state '{key}' = {expected}; got {}",
                actual.map_or("<absent>".to_string(), |v| v.to_string())
            ));
        }
    }
    if let Some(complete) = expect.complete {
        if monitor.is_complete() != complete {
            failures.push(format!(
                "expected complete = {complete}; got {}",
                monitor.is_complete()
            ));
        }
    }
}

fn join(items: &[String]) -> String {
    items.join(", ")
}

#[cfg(test)]
mod tests {
    use super::super::SessionSpec;
    use serde_json::json;

    fn spec_with_tests() -> SessionSpec {
        SessionSpec::from_value(json!({
            "name": "collections",
            "instruction": "Collect.",
            "tools": [
                {"name": "verify_identity", "set_state": {"identity_verified": true}},
                {"name": "charge_card", "response": {"charged": true}}
            ],
            "flow": {
                "steps": [
                    {"id": "verify", "posture": "Verify.", "allow": ["verify_identity"],
                     "done": {"is_true": "identity_verified"}},
                    {"id": "pay", "after": ["verify"], "posture": "Pay.",
                     "allow": ["charge_card"], "done": {"called_ok": "charge_card"}}
                ],
                "constraints": [
                    {"never_until": {"tool": "charge_card",
                                     "until": {"is_true": "identity_verified"}}},
                    {"require": ["pay"]}
                ]
            },
            "tests": [
                {"name": "happy path", "script": [
                    {"expect": {"active": ["verify"], "blocked": ["charge_card"],
                                "complete": false}},
                    {"tool": "verify_identity"},
                    {"expect": {"done": ["verify"], "active": ["pay"],
                                "allowed": ["charge_card"],
                                "state": {"identity_verified": true}}},
                    {"tool": "charge_card"},
                    {"expect": {"done": ["pay"], "complete": true}}
                ]},
                {"name": "premature charge is blocked", "script": [
                    {"tool": "charge_card"},
                    {"expect": {"blocked": ["charge_card"], "complete": false}}
                ]},
                {"name": "deliberately wrong", "script": [
                    {"expect": {"done": ["pay"]}}
                ]}
            ]
        }))
        .expect("spec parses")
    }

    #[test]
    fn scripted_tests_replay_through_the_real_monitor() {
        let reports = spec_with_tests().run_tests();
        assert_eq!(reports.len(), 3);
        assert!(reports[0].passed, "happy path: {:?}", reports[0].failures);
        assert!(
            reports[1].passed,
            "anticipated block passes: {:?}",
            reports[1].failures
        );
        assert!(!reports[2].passed, "wrong expectation fails");
        assert!(reports[2].failures[0].failures[0].contains("expected step 'pay' done"));
    }

    #[test]
    fn trace_snapshots_every_event() {
        let spec = spec_with_tests();
        let snapshots = super::trace_test(&spec, "happy path").expect("traces");
        // start + 5 script events.
        assert_eq!(snapshots.len(), 6);
        assert_eq!(snapshots[0].event, "start");
        assert!(snapshots[0]
            .explanation
            .active
            .contains(&"verify".to_string()));
        // After verify_identity (event 2), verify is done and pay is active.
        assert!(snapshots[2].done.contains(&"verify".to_string()));
        assert!(snapshots[2].explanation.active.contains(&"pay".to_string()));
        // Final snapshot: complete.
        assert!(snapshots[5].complete);
        assert!(super::trace_test(&spec, "no such test").is_err());
    }

    #[test]
    fn computed_variables_latch_guards_in_replay() {
        let spec = SessionSpec::from_value(json!({
            "instruction": "x",
            "state": {"attempts": {"type": "number", "default": 0}},
            "tools": [{"name": "record_score", "set_state": {"score": 0.9}}],
            "computed": [{"key": "high_risk",
                          "from": {"gt": [{"key": "score"}, {"const": 0.5}]}}],
            "flow": {"steps": [
                {"id": "assess", "posture": "Assess.", "allow": ["record_score"],
                 "done": {"is_true": "high_risk"}},
                {"id": "wrap", "after": ["assess"], "terminal": true}
            ], "constraints": [{"require": ["wrap"]}]},
            "tests": [{"name": "risk computes", "script": [
                {"expect": {"active": ["assess"],
                            "state": {"attempts": 0}}},
                {"tool": "record_score"},
                {"expect": {"done": ["assess", "wrap"], "complete": true,
                            "state": {"derived:high_risk": true}}}
            ]}]
        }))
        .expect("parses");
        let reports = spec.run_tests();
        assert!(
            reports[0].passed,
            "computed guard latches offline: {:?}",
            reports[0].failures
        );
    }

    #[test]
    fn unanticipated_block_is_a_failure() {
        let mut spec = spec_with_tests();
        // Script calls charge_card first with no `blocked` assertion after.
        spec.tests = vec![super::SpecTest {
            name: "unanticipated".into(),
            script: vec![super::SimEvent::Tool("charge_card".into())],
        }];
        let reports = spec.run_tests();
        assert!(!reports[0].passed);
        assert!(reports[0].failures[0].failures[0].contains("was blocked"));
    }
}
