//! Offline conformance tests embedded in a [`SessionSpec`].
//!
//! A [`SpecTest`] scripts a conversation as data — user turns, tool calls,
//! state writes — and asserts flow state at checkpoints: which steps are done
//! or active, which tools are admitted or blocked, what the state holds. The
//! script replays through the *real* [`FlowStack`] with the declared tools'
//! mock semantics, so governance is exercised exactly as a live session would
//! — with no model, no network, and no API key. Run in CI, or scrub through
//! one in the Studio.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use gemini_adk_rs::flow::{Enforcement, FlowMonitor, FlowSnapshot, FlowStack};
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
    /// Whether the stack must be complete (all required steps done with no
    /// active overlay, or a terminating overlay ended the conversation).
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
    /// The runtime snapshot shared by live sessions and offline replay.
    #[serde(flatten)]
    pub status: FlowSnapshot,
}

/// Replay one named test and return a snapshot after every event (plus an
/// initial "start" snapshot), for scrubbing. Errors when the flow cannot be
/// built or the test name is unknown.
pub fn trace_test(spec: &SessionSpec, test_name: &str) -> Result<Vec<SimSnapshot>, Vec<String>> {
    if let Some((skill_name, local_name)) = test_name.split_once('/')
        && let Some(skill) = spec.skills.iter().find(|skill| skill.name == skill_name)
    {
        let validation = spec.validate_for_replay();
        if !validation.valid {
            return Err(validation.errors);
        }
        return trace_test(&skill.definition(), local_name);
    }
    let test = spec
        .tests
        .iter()
        .find(|t| t.name == test_name)
        .ok_or_else(|| vec![format!("no test named '{test_name}' in the spec")])?;
    replay(spec, test)
}

/// Compile the same governance stack installed by a live session. Resolvers
/// remain stubs: tests supply state directly and tools use declared mock effects.
fn replay_stack(spec: &SessionSpec) -> Result<FlowStack, Vec<String>> {
    let validation = spec.validate_for_replay();
    if !validation.valid {
        return Err(validation.errors);
    }
    if let Some(conversation) = &spec.conversation {
        crate::conversation::Conversation::from_spec_stubbing_resolvers(conversation.clone())
            .map(|compiled| compiled.stack(Enforcement::Enforce))
            .map_err(|error| vec![format!("conversation: {error}")])
    } else {
        FlowMonitor::try_new(spec.effective_flow()?, Enforcement::Enforce)
            .map(FlowMonitor::into_stack)
            .map_err(|errors| errors.0.iter().map(ToString::to_string).collect())
    }
}

/// The shared replay engine: run the script through a fresh stack, snapshot
/// after every event.
fn replay(spec: &SessionSpec, test: &SpecTest) -> Result<Vec<SimSnapshot>, Vec<String>> {
    let state = State::new();
    // Mirror `apply()`: declared defaults are seeded and computed variables
    // recompute after every state change, so guards over derived keys latch
    // exactly as they do live.
    spec.seed_state_defaults(&state);
    spec.recompute_computed(&state);
    let mut stack = replay_stack(spec)?;
    stack.relatch(&state);

    let snapshot =
        |index: usize, event: String, failures: Vec<String>, stack: &FlowStack, state: &State| {
            SimSnapshot {
                index,
                event,
                failures,
                status: stack.snapshot(state),
            }
        };

    let mut snapshots = vec![snapshot(0, "start".into(), Vec::new(), &stack, &state)];
    for (index, event) in test.script.iter().enumerate() {
        let mut failures = Vec::new();
        let label = match event {
            SimEvent::User(text) => {
                spec.recompute_computed(&state);
                stack.on_turn(&state);
                format!("user: {text}")
            }
            SimEvent::Tool(name) => {
                match stack.admits_tool(name, &state) {
                    Ok(()) => {
                        spec.apply_tool_state(name, &state);
                        spec.recompute_computed(&state);
                        stack.on_tool_ok(name, &state);
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
                stack.relatch(&state);
                format!(
                    "set: {}",
                    map.keys().cloned().collect::<Vec<_>>().join(", ")
                )
            }
            SimEvent::Expect(expect) => {
                check(expect, &stack, &state, &mut failures);
                "expect".to_string()
            }
        };
        snapshots.push(snapshot(index + 1, label, failures, &stack, &state));
    }
    Ok(snapshots)
}

/// Run every embedded test against the same governance stack as live sessions.
pub(crate) fn run_tests(spec: &SessionSpec) -> Vec<TestReport> {
    spec.tests.iter().map(|test| run_one(spec, test)).collect()
}

fn run_one(spec: &SessionSpec, test: &SpecTest) -> TestReport {
    let snapshots = match replay(spec, test) {
        Ok(snapshots) => snapshots,
        Err(errors) => {
            return TestReport {
                name: test.name.clone(),
                passed: false,
                failures: vec![TestStepResult {
                    index: 0,
                    event: "setup".into(),
                    failures: errors,
                }],
                events: 0,
            };
        }
    };
    let failures: Vec<TestStepResult> = snapshots
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

fn check(expect: &TestExpectation, stack: &FlowStack, state: &State, failures: &mut Vec<String>) {
    let explanation = stack.explain(state);
    for step in &expect.done {
        if !stack.marking().done.contains(step) {
            failures.push(format!(
                "expected step '{step}' done; done = [{}]",
                join(&stack.marking().done.iter().cloned().collect::<Vec<_>>())
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
        if stack.admits_tool(tool, state).is_ok() {
            failures.push(format!("expected tool '{tool}' blocked; it was admitted"));
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
    if let Some(complete) = expect.complete
        && stack.is_complete() != complete
    {
        failures.push(format!(
            "expected complete = {complete}; got {}",
            stack.is_complete()
        ));
    }
}

fn join(items: &[String]) -> String {
    items.join(", ")
}

#[cfg(test)]
mod tests {
    use super::super::SessionSpec;
    use serde_json::{Value, json};

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
        assert!(
            snapshots[0]
                .status
                .explanation
                .active
                .contains(&"verify".to_string())
        );
        // After verify_identity (event 2), verify is done and pay is active.
        assert!(snapshots[2].status.done.contains(&"verify".to_string()));
        assert!(
            snapshots[2]
                .status
                .explanation
                .active
                .contains(&"pay".to_string())
        );
        // Final snapshot: complete.
        assert!(snapshots[5].status.complete);
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

    fn conversation_spec(script: Value) -> SessionSpec {
        use crate::conversation::Conversation;
        use gemini_adk_rs::flow::{Guard, Resume};

        let conversation = Conversation::new("support")
            .stage("main")
            .allow(["finish_main"])
            .complete_when(Guard::called_ok("finish_main"))
            .next("main_end", Guard::called_ok("finish_main"))
            .stage("main_end")
            .terminal()
            .require(["main_end"])
            .overlay("faq")
            .trigger(Guard::is_true("intent:faq"))
            .stage("answer")
            .allow(["finish_faq"])
            .complete_when(Guard::called_ok("finish_faq"))
            .next("faq_end", Guard::called_ok("finish_faq"))
            .stage("faq_end")
            .terminal()
            .require(["faq_end"])
            .resume(Resume::Previous)
            .end_overlay()
            .overlay("clarify")
            .trigger(Guard::is_true("intent:clarify"))
            .stage("clarification")
            .allow(["finish_clarify"])
            .complete_when(Guard::called_ok("finish_clarify"))
            .next("clarify_end", Guard::called_ok("finish_clarify"))
            .stage("clarify_end")
            .terminal()
            .require(["clarify_end"])
            .resume(Resume::Previous)
            .end_overlay()
            .overlay("cancel")
            .trigger(Guard::is_true("intent:cancel"))
            .stage("cancelled")
            .terminal()
            .require(["cancelled"])
            .resume(Resume::Terminate)
            .end_overlay()
            .into_spec();
        SessionSpec::from_value(json!({
            "conversation": conversation,
            "tools": [
                {"name": "finish_main"},
                {"name": "finish_faq", "set_state": {"intent:faq": false}},
                {"name": "finish_clarify", "set_state": {"intent:clarify": false}}
            ],
            "tests": [{"name": "conversation", "script": script}]
        }))
        .expect("spec parses")
    }

    #[test]
    fn preview_and_tests_preserve_nested_overlays_and_resume() {
        let spec = conversation_spec(json!([
            {"set": {"intent:faq": true}},
            {"user": "FAQ"},
            {"expect": {"active": ["answer"], "allowed": ["finish_faq"],
                        "complete": false}},
            {"set": {"intent:clarify": true}},
            {"user": "Clarify"},
            {"expect": {"active": ["clarification"], "allowed": ["finish_clarify"],
                        "complete": false}},
            {"tool": "finish_clarify"},
            {"user": "Back to FAQ"},
            {"expect": {"active": ["answer"], "allowed": ["finish_faq"]}},
            {"tool": "finish_faq"},
            {"user": "Back to main"},
            {"expect": {"active": ["main"], "allowed": ["finish_main"],
                        "complete": false}},
            {"tool": "finish_main"},
            {"expect": {"done": ["main", "main_end"], "complete": true}}
        ]));
        let reports = spec.run_tests();
        assert!(reports[0].passed, "{:?}", reports[0].failures);
        let snapshots = super::trace_test(&spec, "conversation").expect("traces");
        assert!(snapshots.iter().all(|s| s.failures.is_empty()));
        assert_eq!(snapshots[2].status.overlay_path, ["faq"]);
        assert_eq!(snapshots[5].status.overlay_path, ["faq", "clarify"]);
        assert_eq!(snapshots[7].status.overlay_path, ["faq", "clarify"]);
        assert_eq!(snapshots[8].status.overlay_path, ["faq"]);
        assert!(snapshots[11].status.overlay_path.is_empty());
        assert!(snapshots.last().expect("final").status.complete);
        let value = serde_json::to_value(&snapshots[5]).expect("serializes");
        assert_eq!(value["overlay_path"], json!(["faq", "clarify"]));
        assert_eq!(value["terminated"], false);
        assert!(value.get("status").is_none(), "runtime fields stay flat");
    }

    #[test]
    fn nested_termination_is_reported_and_blocks_mock_tool_effects() {
        let mut spec = conversation_spec(json!([
            {"set": {"intent:faq": true}},
            {"user": "FAQ"},
            {"set": {"intent:cancel": true}},
            {"user": "Cancel"},
            {"expect": {"complete": false}},
            {"user": "Closing turn"},
            {"expect": {"complete": true, "blocked": ["finish_main"]}},
            {"tool": "finish_main"},
            {"expect": {"complete": true, "blocked": ["finish_main"],
                        "state": {"charged": false}}}
        ]));
        spec.state = serde_json::from_value(json!({
            "charged": {"type": "boolean", "default": false}
        }))
        .expect("state parses");
        spec.tools[0]
            .set_state
            .insert("charged".into(), json!(true));
        let reports = spec.run_tests();
        assert!(reports[0].passed, "{:?}", reports[0].failures);
        let snapshots = super::trace_test(&spec, "conversation").expect("traces");
        assert_eq!(snapshots[4].status.overlay_path, ["faq", "cancel"]);
        assert!(!snapshots[4].status.complete);
        let final_status = &snapshots.last().expect("final").status;
        assert!(final_status.terminated);
        assert!(final_status.complete);
        assert!(final_status.overlay_path.is_empty());
        assert!(final_status.explanation.active.is_empty());
        assert!(final_status.explanation.allowed_tools.is_empty());
    }

    #[test]
    fn overlay_denial_can_be_asserted_for_a_suspended_layers_tool() {
        let mut spec = conversation_spec(json!([
            {"set": {"intent:faq": true, "charged": false}},
            {"user": "FAQ"},
            {"tool": "finish_main"},
            {"expect": {"blocked": ["finish_main"], "state": {"charged": false}}}
        ]));
        spec.tools[0]
            .set_state
            .insert("charged".into(), json!(true));
        let reports = spec.run_tests();
        assert!(reports[0].passed, "{:?}", reports[0].failures);
        let trace = super::trace_test(&spec, "conversation").unwrap();
        assert!(trace.iter().all(|snapshot| snapshot.failures.is_empty()));
    }

    #[test]
    fn replay_uses_conversation_repair_and_set_does_not_count_as_a_turn() {
        use crate::conversation::Conversation;
        use gemini_adk_rs::flow::{Guard, RepairPolicy, reprompt_flag};

        let conversation = Conversation::new("repair")
            .stage("collect")
            .complete_when(Guard::is_true("info"))
            .next("done", Guard::is_true("info"))
            .repair(RepairPolicy::new(2, 3).escalate_to("handoff"))
            .stage("done")
            .terminal()
            .stage("handoff")
            .complete_when(Guard::is_true("handoff_complete"))
            .require(["done"])
            .into_spec();
        let spec = SessionSpec::from_value(json!({
            "conversation": conversation,
            "tests": [{"name": "repair", "script": [
                {"set": {"unrelated": 1}},
                {"set": {"unrelated": 2}},
                {"user": "First turn"},
                {"expect": {"active": ["collect"], "complete": false}},
                {"user": "Second turn"},
                {"expect": {"active": ["collect"],
                            "state": {reprompt_flag("collect"): true}}},
                {"user": "Third turn"},
                {"expect": {"active": ["handoff"],
                            "state": {"repair:collect:escalate": true}}}
            ]}]
        }))
        .expect("parses");
        let reports = spec.run_tests();
        assert!(reports[0].passed, "{:?}", reports[0].failures);
        let snapshots = super::trace_test(&spec, "repair").expect("traces");
        assert!(
            snapshots
                .last()
                .expect("final")
                .status
                .explanation
                .active
                .contains(&"handoff".into())
        );
    }

    #[test]
    fn invalid_specs_fail_preview_and_tests_at_setup() {
        let mut spec = spec_with_tests();
        spec.flow.as_mut().expect("flow").steps[1].after[0].step = "missing".into();
        assert!(!spec.validate().valid);
        assert!(super::trace_test(&spec, "happy path").is_err());
        let reports = spec.run_tests();
        assert!(reports.iter().all(|r| !r.passed && r.events == 0));
        assert!(reports.iter().all(|r| r.failures[0].event == "setup"));
    }

    #[test]
    fn offline_http_tools_keep_mock_effects_without_transport_features() {
        let mut spec = spec_with_tests();
        spec.tools[0].http = Some(
            serde_json::from_value(json!({
                "url": "http://127.0.0.1:1/never-executed"
            }))
            .expect("binding parses"),
        );
        let reports = spec.run_tests();
        assert!(reports[0].passed, "{:?}", reports[0].failures);
        assert!(reports[1].passed, "{:?}", reports[1].failures);
    }

    #[test]
    fn offline_validation_preserves_conflicting_binding_errors() {
        let mut spec = spec_with_tests();
        spec.tools[0].http = Some(
            serde_json::from_value(json!({
                "url": "http://127.0.0.1:1/never-executed"
            }))
            .unwrap(),
        );
        spec.tools[0].mcp = Some("never-executed-tool-server".into());
        let validation = spec.validate_for_replay();
        assert!(!validation.valid);
        assert!(
            validation
                .errors
                .iter()
                .any(|error| error.contains("both an http and an mcp"))
        );
        assert!(super::trace_test(&spec, "happy path").is_err());
        assert!(
            spec.run_tests()
                .iter()
                .all(|report| !report.passed && report.events == 0)
        );
    }
}
