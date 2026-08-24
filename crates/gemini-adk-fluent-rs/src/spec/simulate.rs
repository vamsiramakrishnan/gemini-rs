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
    let state = State::new();
    let mut monitor = FlowMonitor::new(flow, Enforcement::Enforce);
    monitor.relatch(&state);
    let mut failures: Vec<TestStepResult> = Vec::new();

    for (index, event) in test.script.iter().enumerate() {
        let mut step_failures = Vec::new();
        let label = match event {
            SimEvent::User(text) => {
                monitor.on_turn(&state);
                format!("user: {text}")
            }
            SimEvent::Tool(name) => {
                match monitor.admits_tool(name, &state) {
                    Ok(()) => {
                        spec.apply_tool_state(name, &state);
                        monitor.on_tool_ok(name, &state);
                    }
                    Err(reason) => {
                        // A blocked call is only a failure if the script did
                        // not anticipate it: the next event asserting
                        // `blocked` containing this tool absolves it.
                        let anticipated = matches!(
                            test.script.get(index + 1),
                            Some(SimEvent::Expect(e)) if e.blocked.iter().any(|t| t == name)
                        );
                        if !anticipated {
                            step_failures.push(format!("tool '{name}' was blocked: {reason}"));
                        }
                    }
                }
                format!("tool: {name}")
            }
            SimEvent::Set(map) => {
                for (key, value) in map {
                    let _ = state.set(key, value.clone());
                }
                monitor.relatch(&state);
                format!(
                    "set: {}",
                    map.keys().cloned().collect::<Vec<_>>().join(", ")
                )
            }
            SimEvent::Expect(expect) => {
                check(expect, &monitor, &state, &mut step_failures);
                "expect".to_string()
            }
        };
        if !step_failures.is_empty() {
            failures.push(TestStepResult {
                index,
                event: label,
                failures: step_failures,
            });
        }
    }

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
