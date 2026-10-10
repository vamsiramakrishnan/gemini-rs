//! Jev against labelled decisions, with no Live session.
//!
//! Each case in `fixtures/decisions/cases.json` is a conversation (agent and
//! caller lines, tool results), some state, and the expected answer to one or
//! more questions from the fixture's question bank: consent, intents, picks
//! among offered options, the next step, frustration, and judging a finished
//! call. Cases include speech-recognition noise, other languages, long calls,
//! prompt injection and replies to a different question.
//!
//! Every case goes through the same path a session uses: the fixture's
//! questions are `decisions` spec entries compiled into one
//! [`Decisions`] bank over [`GatewayDecisionModel`], and each case is asked
//! with [`Decisions::ask`] over the case's [`Conversation`], its facts as the
//! active stages' grounding. The answers are read back from state as a
//! `decided` guard reads them. Each case runs twice: with the last exchange
//! (two turns) and with the whole call.
//!
//! ```text
//! cargo test -p gemini-adk-fluent-rs --test decision_eval -- --ignored --nocapture
//! ```
//!
//! Reads `AI_GATEWAY_API_KEY` from the environment or `.env.local`. Prints
//! right / unsure / wrong per category and window, latency, and a threshold
//! sweep over the boolean answers; the report lands in
//! `target/tmp/decision-eval/`. `DECISION_EVAL_ONLY` (comma-separated id
//! prefixes) runs a subset.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use common::env::env_or_local;
use gemini_adk_fluent_rs::spec::{DecisionKind, DecisionSpec};
use gemini_adk_rs::State;
use gemini_adk_rs::decision::{Conversation, Decisions, GatewayDecisionModel};
use gemini_adk_rs::flow::{DecisionRecord, Outcome};
use gemini_adk_rs::live::transcript::{ToolCallSummary, TranscriptTurn};

#[derive(Deserialize)]
struct Fixture {
    questions: BTreeMap<String, DecisionSpec>,
    cases: Vec<Case>,
}

#[derive(Deserialize, Clone)]
struct Case {
    id: String,
    category: String,
    /// `["agent", text]`, `["caller", text]` or `["tool", name, result]`.
    turns: Vec<Value>,
    #[serde(default)]
    state: Map<String, Value>,
    #[serde(default)]
    facts: Map<String, Value>,
    /// Question id to the expected answer: a boolean, an option key (`null`
    /// for "none of these"), or `[low, high]` for a score.
    expect: BTreeMap<String, Value>,
}

/// The conversation as the runtime records it: a turn starts with the
/// caller's words, then any tool calls, then the agent's reply.
fn transcript(entries: &[Value]) -> Vec<TranscriptTurn> {
    let fresh = |n: usize| TranscriptTurn {
        turn_number: n as u32,
        user: String::new(),
        model: String::new(),
        tool_calls: Vec::new(),
        timestamp: std::time::Instant::now(),
    };
    let mut turns = Vec::new();
    let mut current = fresh(0);
    for entry in entries {
        let role = entry[0].as_str().unwrap_or_default();
        let text = entry[1].as_str().unwrap_or_default();
        match role {
            "caller" => {
                if !current.user.is_empty()
                    || !current.model.is_empty()
                    || !current.tool_calls.is_empty()
                {
                    turns.push(std::mem::replace(&mut current, fresh(turns.len() + 1)));
                }
                current.user = text.to_string();
            }
            "agent" => {
                if !current.model.is_empty() {
                    current.model.push(' ');
                }
                current.model.push_str(text);
            }
            "tool" => current.tool_calls.push(ToolCallSummary {
                name: text.to_string(),
                args_summary: "{}".into(),
                result_summary: entry[2].to_string(),
            }),
            other => panic!("unknown role {other}"),
        }
    }
    turns.push(current);
    turns
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Verdict {
    Right,
    Unsure,
    Wrong,
}

struct Row {
    case: String,
    category: String,
    window: &'static str,
    question: String,
    expected: Value,
    verdict: Verdict,
    decided: Value,
    raw: Value,
    ms: u64,
}

/// What a recorded answer decided: a boolean, an option key (`null` for
/// none of these), or a score; `None` when unsure.
fn decided(record: &DecisionRecord) -> Option<Value> {
    match record.outcome {
        Outcome::Yes => Some(json!(true)),
        Outcome::No => Some(json!(false)),
        Outcome::None => Some(Value::Null),
        Outcome::Chosen | Outcome::Scored => Some(record.value.clone()),
        Outcome::Unsure => None,
    }
}

fn judge(expected: &Value, decided: Option<&Value>) -> Verdict {
    let Some(decided) = decided else {
        return Verdict::Unsure;
    };
    let right = match expected {
        Value::Array(range) => {
            let (lo, hi) = (range[0].as_f64().unwrap(), range[1].as_f64().unwrap());
            decided.as_f64().is_some_and(|s| s >= lo && s <= hi)
        }
        other => decided == other,
    };
    if right {
        Verdict::Right
    } else {
        Verdict::Wrong
    }
}

async fn run_case(
    case: Case,
    window: &'static str,
    decisions: &Decisions,
) -> Result<Vec<Row>, String> {
    let turns = transcript(&case.turns);
    let size = if window == "exchange" { 2 } else { turns.len() };
    let state = State::new();
    for (k, v) in case.state.iter().chain(case.facts.iter()) {
        let _ = state.set(k, v.clone());
    }
    let grounding = case.facts.iter().map(|(k, v)| match v {
        Value::String(s) => format!("{k}: {s}"),
        other => format!("{k}: {other}"),
    });
    let start = turns.len().saturating_sub(size);
    let conversation = Conversation::from_turns(&turns[start..]).with_context(grounding);
    let ids = case.expect.keys().cloned().collect();
    let round = decisions.ask(&ids, &conversation, &state).await;
    if let Some(error) = round.error {
        return Err(format!("{}: {error}", case.id));
    }
    Ok(case
        .expect
        .iter()
        .map(|(id, expected)| {
            let record = DecisionRecord::current(&state, id);
            let decided = record.as_ref().and_then(decided);
            Row {
                case: case.id.clone(),
                category: case.category.clone(),
                window,
                question: id.clone(),
                expected: expected.clone(),
                verdict: judge(expected, decided.as_ref()),
                decided: decided.unwrap_or(json!("unsure")),
                raw: record.map_or(Value::Null, |r| {
                    json!({ "outcome": r.outcome, "value": r.value, "confidence": r.confidence })
                }),
                ms: round.latency.as_millis() as u64,
            }
        })
        .collect())
}

fn percentile(mut v: Vec<u64>, q: f64) -> u64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[((v.len() - 1) as f64 * q).round() as usize]
}

fn render(outcomes: &[Row], errors: &[String]) -> String {
    let mut out = String::from("# Jev decision eval\n\n");
    let windows = ["exchange", "history"];
    // Per category.
    let mut cats: BTreeMap<&str, BTreeMap<&str, [usize; 3]>> = BTreeMap::new();
    for o in outcomes {
        let c = cats
            .entry(&o.category)
            .or_default()
            .entry(o.window)
            .or_default();
        c[o.verdict as usize] += 1;
    }
    out.push_str("| Category | Exchange (right / unsure / wrong) | History (right / unsure / wrong) |\n|---|---|---|\n");
    let mut totals = [[0usize; 3]; 2];
    for (cat, by) in &cats {
        let cell = |w: &str| {
            by.get(w)
                .map_or("-".into(), |c| format!("{} / {} / {}", c[0], c[1], c[2]))
        };
        for (i, w) in windows.iter().enumerate() {
            if let Some(c) = by.get(w) {
                for k in 0..3 {
                    totals[i][k] += c[k];
                }
            }
        }
        out.push_str(&format!(
            "| {cat} | {} | {} |\n",
            cell("exchange"),
            cell("history")
        ));
    }
    out.push_str(&format!(
        "| **all** | **{} / {} / {}** | **{} / {} / {}** |\n\n",
        totals[0][0], totals[0][1], totals[0][2], totals[1][0], totals[1][1], totals[1][2]
    ));

    // Latency.
    for w in windows {
        let ms: Vec<u64> = outcomes
            .iter()
            .filter(|o| o.window == w)
            .map(|o| o.ms)
            .collect();
        out.push_str(&format!(
            "- {w}: p50 {} ms, p90 {} ms, max {} ms\n",
            percentile(ms.clone(), 0.5),
            percentile(ms.clone(), 0.9),
            percentile(ms, 1.0)
        ));
    }
    if !errors.is_empty() {
        out.push_str(&format!("- errors: {}\n", errors.len()));
    }

    // Threshold sweep over the boolean answers.
    out.push_str("\n## Boolean threshold sweep\n\nDecided when `P(true) >= t` (yes) or `<= 1 - t` (no).\n\n| t | Exchange: decided / wrong | History: decided / wrong |\n|---|---|---|\n");
    for t in [0.6, 0.7, 0.8, 0.85, 0.9, 0.95] {
        let row = |w: &str| {
            let mut decided = 0;
            let mut wrong = 0;
            let mut n = 0;
            for o in outcomes.iter().filter(|o| o.window == w) {
                let (Some(p), Some(want)) = (o.raw["confidence"].as_f64(), o.expected.as_bool())
                else {
                    continue;
                };
                n += 1;
                if p >= t || p <= 1.0 - t {
                    decided += 1;
                    if (p >= t) != want {
                        wrong += 1;
                    }
                }
            }
            format!("{decided}/{n} / {wrong}")
        };
        out.push_str(&format!(
            "| {t} | {} | {} |\n",
            row("exchange"),
            row("history")
        ));
    }

    // What was not right.
    out.push_str("\n## Not right\n\n| Case | Window | Question | Expected | Decided | Raw answer |\n|---|---|---|---|---|---|\n");
    let mut misses: Vec<&Row> = outcomes
        .iter()
        .filter(|o| o.verdict != Verdict::Right)
        .collect();
    misses.sort_by(|a, b| (b.verdict, &a.case, a.window).cmp(&(a.verdict, &b.case, b.window)));
    for o in misses {
        let raw = if o.expected.is_boolean() {
            format!("P(true) {}", o.raw["confidence"])
        } else {
            format!("{} (certainty {})", o.raw["value"], o.raw["confidence"])
        };
        out.push_str(&format!(
            "| {} {} | {} | {} | {} | {} | {} |\n",
            if o.verdict == Verdict::Wrong {
                "❌"
            } else {
                "❔"
            },
            o.case,
            o.window,
            o.question,
            o.expected,
            o.decided,
            raw
        ));
    }
    for e in errors {
        out.push_str(&format!("\nerror: {e}"));
    }
    out
}

#[tokio::test]
#[ignore = "calls Vercel AI Gateway; needs AI_GATEWAY_API_KEY and Jev access"]
async fn jev_against_labelled_decisions() {
    let Some(key) = env_or_local("AI_GATEWAY_API_KEY") else {
        eprintln!("AI_GATEWAY_API_KEY not set; skipping");
        return;
    };
    let fixture: Fixture =
        serde_json::from_str(include_str!("fixtures/decisions/cases.json")).expect("fixture");
    let only: Option<Vec<String>> = std::env::var("DECISION_EVAL_ONLY")
        .ok()
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect());
    let model = Arc::new(
        GatewayDecisionModel::new(GatewayDecisionModel::JEV, key)
            .with_timeout(Duration::from_secs(15)),
    );
    // One bank for every case, as one bank serves every session.
    let decisions = Arc::new(
        fixture
            .questions
            .iter()
            .fold(Decisions::new(model), |d, (id, q)| {
                d.question(id, q.compile())
            })
            .with_timeout(Duration::from_secs(15)),
    );
    let semaphore = Arc::new(tokio::sync::Semaphore::new(8));
    let mut handles = Vec::new();
    for case in fixture.cases {
        if only
            .as_ref()
            .is_some_and(|o| !o.iter().any(|p| case.id.starts_with(p.as_str())))
        {
            continue;
        }
        for window in ["exchange", "history"] {
            let (case, decisions, sem) = (case.clone(), decisions.clone(), semaphore.clone());
            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire_owned().await.unwrap();
                run_case(case, window, &decisions).await
            }));
        }
    }
    let mut outcomes = Vec::new();
    let mut errors = Vec::new();
    for h in handles {
        match h.await.expect("task") {
            Ok(o) => outcomes.extend(o),
            Err(e) => errors.push(e),
        }
    }
    let report = render(&outcomes, &errors);
    println!("\n{report}");
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("decision-eval");
    std::fs::create_dir_all(&dir).unwrap();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let path = dir.join(format!("report-{stamp}.md"));
    std::fs::write(&path, &report).unwrap();
    let rows: Vec<Value> = outcomes
        .iter()
        .map(|o| {
            json!({ "case": o.case, "category": o.category, "window": o.window,
                    "question": o.question, "expected": o.expected,
                    "verdict": format!("{:?}", o.verdict), "decided": o.decided,
                    "raw": o.raw, "ms": o.ms })
        })
        .collect();
    std::fs::write(
        dir.join(format!("outcomes-{stamp}.json")),
        serde_json::to_string_pretty(&rows).unwrap(),
    )
    .unwrap();
    println!("report: {}", path.display());
}

#[test]
fn the_fixture_is_well_formed() {
    let fixture: Fixture =
        serde_json::from_str(include_str!("fixtures/decisions/cases.json")).expect("fixture");
    let mut ids = std::collections::BTreeSet::new();
    for case in &fixture.cases {
        assert!(ids.insert(case.id.clone()), "duplicate id {}", case.id);
        let turns = transcript(&case.turns);
        assert!(
            !turns.last().unwrap().user.is_empty(),
            "{}: the last turn must be the caller's",
            case.id
        );
        for (id, expected) in &case.expect {
            let q = fixture
                .questions
                .get(id)
                .unwrap_or_else(|| panic!("{}: no question {id}", case.id));
            let fits = match &q.kind {
                DecisionKind::Boolean { .. } => expected.is_boolean(),
                DecisionKind::Choice { .. } => expected.is_string() || expected.is_null(),
                DecisionKind::Score { .. } => expected.is_array(),
            };
            assert!(fits, "{}: {id} expects {expected}", case.id);
        }
    }
    // Every question is a valid spec entry.
    for (id, q) in &fixture.questions {
        assert!(q.problems(id).is_empty(), "{:?}", q.problems(id));
    }
}
