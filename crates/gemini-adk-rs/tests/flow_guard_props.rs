//! Property tests for governed-flow tool admission.
//!
//! The property: a gated tool is never admitted while its guard does not
//! hold, whatever the interleaving of state writes, tool calls, tool
//! completions and turn boundaries, and however many other tools are
//! called in between. A second property covers step admission: a tool is
//! admitted only while some active step allows it (or it is ambient), and
//! never before every predecessor of that step is done.

use std::collections::{BTreeMap, BTreeSet};

use gemini_adk_rs::flow::{Enforcement, Flow, FlowMonitor, Guard};
use gemini_adk_rs::state::State;
use proptest::prelude::*;
use serde_json::json;

const KEYS: [&str; 3] = ["k0", "k1", "k2"];
const TOOLS: [&str; 4] = ["t0", "t1", "t2", "t3"];

/// One thing a session can do between tool admissions.
#[derive(Clone, Debug)]
enum Op {
    /// Write a boolean key.
    Set(usize, bool),
    /// The model calls a tool: admission is evaluated, and when admitted the
    /// tool completes successfully.
    Call(usize),
    /// A turn boundary.
    Turn,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0..KEYS.len(), any::<bool>()).prop_map(|(k, v)| Op::Set(k, v)),
        (0..TOOLS.len()).prop_map(Op::Call),
        Just(Op::Turn),
    ]
}

/// A guard over the boolean keys: a small grammar, nested at most two deep.
fn guard() -> impl Strategy<Value = Guard> {
    let leaf = prop_oneof![
        Just(Guard::always()),
        (0..KEYS.len()).prop_map(|k| Guard::is_true(KEYS[k])),
        (0..KEYS.len()).prop_map(|k| Guard::is_set(KEYS[k])),
        (0..TOOLS.len()).prop_map(|t| Guard::called_ok(TOOLS[t])),
    ];
    leaf.prop_recursive(2, 6, 3, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 1..3).prop_map(Guard::all),
            prop::collection::vec(inner.clone(), 1..3).prop_map(Guard::any),
            inner.prop_map(Guard::not),
        ]
    })
}

/// A `never(tool).until(guard)` constraint.
fn never_until() -> impl Strategy<Value = (usize, Guard)> {
    (0..TOOLS.len(), guard())
}

/// A linear flow of 1–4 steps, each allowing a subset of the tools, plus
/// `never/until` constraints. Every step after the first depends on the one
/// before it, so predecessor order is total and easy to check.
#[derive(Clone, Debug)]
struct FlowShape {
    allow: Vec<BTreeSet<usize>>,
    done: Vec<Option<Guard>>,
    never: Vec<(usize, Guard)>,
}

fn flow_shape() -> impl Strategy<Value = FlowShape> {
    (
        prop::collection::vec(
            (
                prop::collection::btree_set(0..TOOLS.len(), 0..=TOOLS.len()),
                prop::option::of(guard()),
            ),
            1..=4,
        ),
        prop::collection::vec(never_until(), 0..=3),
    )
        .prop_map(|(steps, never)| FlowShape {
            allow: steps.iter().map(|(a, _)| a.clone()).collect(),
            done: steps.into_iter().map(|(_, d)| d).collect(),
            never,
        })
}

fn build(shape: &FlowShape) -> Flow {
    let mut builder = Flow::new();
    for (i, allow) in shape.allow.iter().enumerate() {
        builder = builder.step(format!("s{i}"));
        if i > 0 {
            builder = builder.after(format!("s{}", i - 1));
        }
        if !allow.is_empty() {
            builder = builder.allow(allow.iter().map(|&t| TOOLS[t]));
        }
        // A non-terminal step must be able to complete: the flow validator
        // refuses one with no `done` guard, so a missing guard becomes
        // `always()` (the step completes at its first turn).
        match &shape.done[i] {
            Some(done) => builder = builder.done(done.clone()),
            None if i + 1 < shape.allow.len() => builder = builder.done(Guard::always()),
            None => {}
        }
        if i + 1 == shape.allow.len() {
            builder = builder.terminal();
        }
    }
    for (tool, until) in &shape.never {
        builder = builder.never(TOOLS[*tool]).until(until.clone());
    }
    builder.build().expect("a linear flow is always valid")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// `never(tool).until(guard)`: the tool is admitted only while every
    /// such guard on it holds, at the moment of the call.
    #[test]
    fn a_gated_tool_is_never_admitted_before_its_guard_holds(
        shape in flow_shape(),
        ops in prop::collection::vec(op(), 0..40),
    ) {
        let flow = build(&shape);
        let mut monitor = FlowMonitor::new(flow, Enforcement::Enforce);
        let state = State::new();
        for op in ops {
            match op {
                Op::Set(k, v) => {
                    state.set(KEYS[k], json!(v)).expect("a boolean key is serializable");
                }
                Op::Turn => monitor.on_turn(&state),
                Op::Call(t) => {
                    let tool = TOOLS[t];
                    let gates: Vec<&Guard> = shape
                        .never
                        .iter()
                        .filter(|(gated, _)| *gated == t)
                        .map(|(_, g)| g)
                        .collect();
                    let guards_hold = gates.iter().all(|g| monitor.eval(g, &state));
                    let admitted = monitor.admits_tool(tool, &state).is_ok();
                    prop_assert!(
                        !admitted || guards_hold,
                        "{tool} admitted while a never/until guard was false; gates {gates:?}"
                    );
                    if admitted {
                        monitor.on_tool_ok(tool, &state);
                    }
                }
            }
        }
    }

    /// Step admission: an admitted tool is allowed by a step that is active
    /// now, and every predecessor of that step is done.
    #[test]
    fn an_admitted_tool_is_allowed_by_an_active_step_whose_predecessors_are_done(
        shape in flow_shape(),
        ops in prop::collection::vec(op(), 0..40),
    ) {
        let flow = build(&shape);
        let mut monitor = FlowMonitor::new(flow, Enforcement::Enforce);
        let state = State::new();
        for op in ops {
            match op {
                Op::Set(k, v) => {
                    state.set(KEYS[k], json!(v)).expect("a boolean key is serializable");
                }
                Op::Turn => monitor.on_turn(&state),
                Op::Call(t) => {
                    let tool = TOOLS[t];
                    if monitor.admits_tool(tool, &state).is_ok() {
                        let active: BTreeMap<String, Vec<String>> = monitor
                            .active_steps(&state)
                            .into_iter()
                            .map(|s| (s.id.clone(), s.allow.clone()))
                            .collect();
                        // A step's `allow` is a whitelist only while some
                        // active step has one: a step that names no tools
                        // restricts nothing (the flow's documented rule).
                        let restricting = active.values().any(|allow| !allow.is_empty());
                        let allowing: Vec<&String> = active
                            .iter()
                            .filter(|(_, allow)| allow.iter().any(|a| a == tool))
                            .map(|(id, _)| id)
                            .collect();
                        prop_assert!(
                            !restricting || !allowing.is_empty(),
                            "{tool} admitted but no active step allows it; active {active:?}"
                        );
                        for step in allowing {
                            let index: usize = step[1..].parse().unwrap();
                            for earlier in 0..index {
                                prop_assert!(
                                    monitor.marking().done.contains(&format!("s{earlier}")),
                                    "{tool} admitted in {step} while s{earlier} is not done"
                                );
                            }
                        }
                        monitor.on_tool_ok(tool, &state);
                    }
                }
            }
        }
    }

    /// Admission is a pure function of the monitor and the state: asking
    /// twice in a row, with nothing in between, gives the same answer.
    #[test]
    fn admission_is_stable_between_events(
        shape in flow_shape(),
        ops in prop::collection::vec(op(), 0..30),
        probe in 0..TOOLS.len(),
    ) {
        let flow = build(&shape);
        let mut monitor = FlowMonitor::new(flow, Enforcement::Enforce);
        let state = State::new();
        for op in ops {
            match op {
                Op::Set(k, v) => {
                    state.set(KEYS[k], json!(v)).expect("a boolean key is serializable");
                }
                Op::Turn => monitor.on_turn(&state),
                Op::Call(t) => {
                    if monitor.admits_tool(TOOLS[t], &state).is_ok() {
                        monitor.on_tool_ok(TOOLS[t], &state);
                    }
                }
            }
            let first = monitor.admits_tool(TOOLS[probe], &state).is_ok();
            let second = monitor.admits_tool(TOOLS[probe], &state).is_ok();
            prop_assert_eq!(first, second);
        }
    }
}
