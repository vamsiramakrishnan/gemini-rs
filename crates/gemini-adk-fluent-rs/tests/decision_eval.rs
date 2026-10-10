//! Jev against labelled decisions, with no Live session.
//!
//! Each case in `fixtures/decisions/cases.json` is a conversation (agent and
//! caller lines, tool results), some state, and the expected answer to one or
//! more questions from the fixture's question bank: consent, intents, picks
//! among offered options, the next step, frustration, and judging a finished
//! call. Cases include speech-recognition noise, other languages, long calls,
//! prompt injection and replies to a different question.
//!
//! Every case goes through the same path a session uses: a `decide` spec entry
//! compiled to a [`DecisionExtractor`](gemini_adk_rs::decision::DecisionExtractor)
//! over [`GatewayDecisionModel`], with the language-model fallback off so the
//! numbers are Jev's alone. Each case runs twice: with the last exchange
//! (window 2) and with the whole call (every turn).
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
use gemini_adk_fluent_rs::spec::DecideSpec;
use gemini_adk_rs::State;
use gemini_adk_rs::decision::GatewayDecisionModel;
use gemini_adk_rs::live::extractor::TurnExtractor;
use gemini_adk_rs::live::transcript::{ToolCallSummary, TranscriptTurn};

#[derive(Deserialize)]
struct Fixture {
    questions: BTreeMap<String, Value>,
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

struct Outcome {
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

fn judge(expected: &Value, decided: &Value, uncertain: bool) -> Verdict {
    if uncertain {
        return Verdict::Unsure;
    }
    let key = |v: &Value| match v {
        Value::Object(m) => m.get("id").cloned().unwrap_or(Value::Null),
        other => other.clone(),
    };
    let right = match expected {
        Value::Array(range) => {
            let (lo, hi) = (range[0].as_f64().unwrap(), range[1].as_f64().unwrap());
            decided.as_f64().is_some_and(|s| s >= lo && s <= hi)
        }
        other => key(decided) == *other,
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
    bank: &BTreeMap<String, Value>,
    model: Arc<GatewayDecisionModel>,
) -> Result<Vec<Outcome>, String> {
    let turns = transcript(&case.turns);
    let size = if window == "exchange" { 2 } else { turns.len() };
    let mut questions = Map::new();
    for id in case.expect.keys() {
        let mut q = bank
            .get(id)
            .cloned()
            .ok_or_else(|| format!("no question {id}"))?;
        let boolean = q["type"] == "boolean";
        q["promote"] = json!({ "to": id, "write_false": boolean });
        questions.insert(id.clone(), q);
    }
    let spec: DecideSpec = serde_json::from_value(json!({
        "name": "eval",
        "window": size,
        "facts": case.facts.keys().collect::<Vec<_>>(),
        "questions": questions,
        "fallback": "none",
        "timeout_ms": 15_000,
    }))
    .map_err(|e| format!("{}: {e}", case.id))?;
    let extractor = spec.compile(model, None);

    let state = State::new();
    for (k, v) in case.state.iter().chain(case.facts.iter()) {
        let _ = state.set(k, v.clone());
    }
    let start = turns.len().saturating_sub(size);
    let value = extractor
        .extract_with_state(&turns[start..], &state)
        .await
        .map_err(|e| format!("{}: {e}", case.id))?;
    let meta = &value["_decision"];
    let uncertain: Vec<&str> = meta["uncertain"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    Ok(case
        .expect
        .iter()
        .map(|(id, expected)| {
            let decided = value.get(id).cloned().unwrap_or(Value::Null);
            Outcome {
                case: case.id.clone(),
                category: case.category.clone(),
                window,
                question: id.clone(),
                expected: expected.clone(),
                verdict: judge(expected, &decided, uncertain.contains(&id.as_str())),
                decided,
                raw: meta["answers"][id].clone(),
                ms: meta["ms"].as_u64().unwrap_or(0),
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

fn render(outcomes: &[Outcome], errors: &[String]) -> String {
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
                let (Some(p), Some(want)) = (o.raw["probability"].as_f64(), o.expected.as_bool())
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
    let mut misses: Vec<&Outcome> = outcomes
        .iter()
        .filter(|o| o.verdict != Verdict::Right)
        .collect();
    misses.sort_by(|a, b| (b.verdict, &a.case, a.window).cmp(&(a.verdict, &b.case, b.window)));
    for o in misses {
        let raw = match o.raw.get("probability") {
            Some(p) => format!("P(true) {p}"),
            None => format!(
                "{} (confidence {})",
                o.raw
                    .get("choice")
                    .or(o.raw.get("score"))
                    .unwrap_or(&Value::Null),
                o.raw.get("confidence").unwrap_or(&Value::Null)
            ),
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
    let bank = Arc::new(fixture.questions);
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
            let (case, bank, model, sem) =
                (case.clone(), bank.clone(), model.clone(), semaphore.clone());
            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire_owned().await.unwrap();
                run_case(case, window, &bank, model).await
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
            match q["type"].as_str() {
                Some("boolean") => assert!(expected.is_boolean(), "{}: {id}", case.id),
                Some("choice") => assert!(
                    expected.is_string() || expected.is_null(),
                    "{}: {id}",
                    case.id
                ),
                Some("score") => assert!(expected.is_array(), "{}: {id}", case.id),
                other => panic!("{id}: unknown type {other:?}"),
            }
        }
    }
    // Every question compiles as a spec entry.
    let questions: Map<String, Value> = fixture.questions.into_iter().collect();
    let spec: DecideSpec =
        serde_json::from_value(json!({ "name": "eval", "questions": questions })).unwrap();
    assert!(
        spec.problems(&Default::default()).is_empty(),
        "{:?}",
        spec.problems(&Default::default())
    );
}
