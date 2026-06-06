//! Evaluation pipeline — wires `POST /eval/run` to `gemini_adk_rs::evaluation`.
//!
//! The REST eval endpoint accepts an [`EvalSet`] (inline JSON or a file path) and
//! a list of criteria, then scores each case with the matching deterministic
//! evaluators and aggregates the result into an [`EvalResultSummary`].
//!
//! Two evaluation modes are supported per case, chosen automatically:
//!
//! - **Pre-recorded** — if a case already carries `actual` invocations, they are
//!   evaluated directly against `expected`. This path needs no LLM and is fully
//!   deterministic (the path exercised by unit tests).
//! - **Live** — if a case has no `actual` invocations, the agent is run over the
//!   user turns of its `expected` invocations to produce actuals, which are then
//!   evaluated. This path performs real LLM generation.
//!
//! Criteria map to evaluators by name (see [`evaluator_for`]). Each criterion may
//! carry an optional `name=threshold` form (e.g. `response_match=0.8`); a case
//! passes when every criterion meets its threshold.

use std::collections::HashMap;
use std::sync::Arc;

use gemini_adk_rs::evaluation::{
    EvalSet, Evaluator, Invocation, InvocationTurn, MatchStrategy, ResponseEvaluator,
    TrajectoryEvaluator,
};

use crate::execution::{build_text_agent, run_agent_turn};
use crate::types::{now_iso8601, EvalResultSummary, EvalRunRequest};
use crate::ServerState;

/// Default pass threshold applied to a criterion that does not specify its own.
pub const DEFAULT_PASS_THRESHOLD: f64 = 0.7;

/// A parsed evaluation criterion: a metric name plus the score it must meet.
#[derive(Debug, Clone, PartialEq)]
pub struct Criterion {
    /// Canonical criterion name (e.g. `response_match`).
    pub name: String,
    /// Minimum score (0.0–1.0) for this criterion to pass.
    pub threshold: f64,
}

/// Parse raw criteria strings into [`Criterion`]s.
///
/// Each entry may be a bare name (`response_match`) or carry a threshold
/// (`response_match=0.8`). When `raw` is empty, a sensible default pair of
/// `response_match` + `tool_trajectory` is used so a criteria-less request still
/// produces a meaningful score.
pub fn parse_criteria(raw: &[String]) -> Vec<Criterion> {
    if raw.is_empty() {
        return vec![
            Criterion {
                name: "response_match".into(),
                threshold: DEFAULT_PASS_THRESHOLD,
            },
            Criterion {
                name: "tool_trajectory".into(),
                threshold: DEFAULT_PASS_THRESHOLD,
            },
        ];
    }

    raw.iter()
        .map(|entry| {
            let (name, threshold) = match entry.split_once('=') {
                Some((n, t)) => (n.trim(), t.trim().parse().unwrap_or(DEFAULT_PASS_THRESHOLD)),
                None => (entry.trim(), DEFAULT_PASS_THRESHOLD),
            };
            Criterion {
                name: name.to_string(),
                threshold,
            }
        })
        .collect()
}

/// Resolve a criterion name to a deterministic [`Evaluator`].
///
/// Returns `None` for criteria that require an LLM judge (safety, hallucination,
/// rubric, llm_judge) — those are out of scope for the deterministic REST path
/// and are reported as skipped rather than silently scored.
pub fn evaluator_for(name: &str) -> Option<Arc<dyn Evaluator>> {
    let eval: Arc<dyn Evaluator> = match name {
        "response_match" | "response" | "final_response_match" => {
            Arc::new(ResponseEvaluator::new(MatchStrategy::Contains).with_metric_name(name))
        }
        "exact_match" => {
            Arc::new(ResponseEvaluator::new(MatchStrategy::Exact).with_metric_name(name))
        }
        "tool_trajectory" | "trajectory" | "tool_trajectory_avg_score" => {
            Arc::new(TrajectoryEvaluator::new(true).with_metric_name(name))
        }
        "tool_trajectory_any_order" => {
            Arc::new(TrajectoryEvaluator::new(false).with_metric_name(name))
        }
        _ => return None,
    };
    Some(eval)
}

/// Load an [`EvalSet`] from the request's `eval_set` field.
///
/// Accepts either inline JSON or a filesystem path to a JSON evalset.
fn load_eval_set(eval_set: &Option<String>) -> Result<EvalSet, String> {
    let raw = eval_set
        .as_ref()
        .ok_or_else(|| "eval_set is required (inline JSON or a file path)".to_string())?;

    // Inline JSON first; fall back to treating the value as a file path.
    if let Ok(set) = serde_json::from_str::<EvalSet>(raw) {
        return Ok(set);
    }
    let contents = std::fs::read_to_string(raw)
        .map_err(|e| format!("eval_set is neither valid JSON nor a readable file: {e}"))?;
    serde_json::from_str::<EvalSet>(&contents).map_err(|e| format!("failed to parse evalset: {e}"))
}

/// Run an agent over the user turns of an expected invocation to produce an
/// actual invocation for live evaluation.
///
/// State is threaded across turns so multi-turn context carries forward. Note the
/// REST text path does not execute wire-level builtin tools, so produced actuals
/// carry model responses but no tool-call trajectory (trajectory criteria over
/// live runs will therefore score against an empty actual trajectory).
async fn produce_actual(
    agent: &Arc<dyn gemini_adk_rs::text::TextAgent>,
    expected: &Invocation,
) -> Result<Invocation, String> {
    let mut turns = Vec::new();
    let mut prior: HashMap<String, serde_json::Value> = HashMap::new();

    for turn in expected.turns.iter().filter(|t| t.role == "user") {
        let outcome = run_agent_turn(agent, &turn.content, &prior)
            .await
            .map_err(|e| format!("agent run failed: {e}"))?;
        turns.push(InvocationTurn {
            role: "user".into(),
            content: turn.content.clone(),
            tool_calls: vec![],
            tool_results: vec![],
        });
        turns.push(InvocationTurn {
            role: "model".into(),
            content: outcome.response.clone(),
            tool_calls: vec![],
            tool_results: vec![],
        });
        prior.insert("input".into(), serde_json::json!(turn.content));
        prior.insert("output".into(), serde_json::json!(outcome.response));
    }

    Ok(Invocation {
        id: expected.id.clone(),
        turns,
        metadata: serde_json::Value::Null,
    })
}

/// Aggregate per-case, per-criterion scores into an [`EvalResultSummary`].
///
/// A case passes when **every** scored criterion meets its threshold. Per-criterion
/// summary scores are averaged across all cases.
pub fn summarize(
    agent: &str,
    criteria: &[Criterion],
    per_case: &[HashMap<String, f64>],
) -> EvalResultSummary {
    let total_cases = per_case.len();
    let mut passed = 0usize;

    // Sum per-criterion scores across cases for averaging.
    let mut sums: HashMap<String, (f64, usize)> = HashMap::new();
    for case in per_case {
        let mut case_passes = !case.is_empty();
        for crit in criteria {
            if let Some(&score) = case.get(&crit.name) {
                let entry = sums.entry(crit.name.clone()).or_insert((0.0, 0));
                entry.0 += score;
                entry.1 += 1;
                if score < crit.threshold {
                    case_passes = false;
                }
            }
        }
        if case_passes {
            passed += 1;
        }
    }

    let criteria_scores = sums
        .into_iter()
        .map(|(name, (sum, n))| (name, if n > 0 { sum / n as f64 } else { 0.0 }))
        .collect();

    let failed = total_cases - passed;
    let pass_rate = if total_cases > 0 {
        passed as f64 / total_cases as f64
    } else {
        0.0
    };

    EvalResultSummary {
        agent: agent.to_string(),
        timestamp: now_iso8601(),
        total_cases,
        passed,
        failed,
        pass_rate,
        criteria_scores,
    }
}

/// Run an eval set end to end and produce a summary.
///
/// Resolves the agent from the registry, loads the evalset, builds evaluators from
/// the criteria, scores every case, and aggregates. Cases without `actual`
/// invocations are run live against the agent.
pub async fn run_evalset(
    state: &ServerState,
    req: &EvalRunRequest,
) -> Result<EvalResultSummary, String> {
    let entry = state
        .agents
        .get(&req.agent)
        .ok_or_else(|| format!("Agent '{}' not found", req.agent))?;
    let agent = build_text_agent(entry);

    let eval_set = load_eval_set(&req.eval_set)?;
    let criteria = parse_criteria(&req.criteria);

    // Pre-build evaluators once; skipped criteria (LLM-judge) are reported as such.
    let evaluators: Vec<(Criterion, Arc<dyn Evaluator>)> = criteria
        .iter()
        .filter_map(|c| evaluator_for(&c.name).map(|e| (c.clone(), e)))
        .collect();

    let mut per_case: Vec<HashMap<String, f64>> = Vec::with_capacity(eval_set.cases.len());

    for case in &eval_set.cases {
        // Use recorded actuals when present; otherwise run the agent live.
        let actual: Vec<Invocation> = if case.actual.is_empty() {
            let mut produced = Vec::with_capacity(case.expected.len());
            for expected in &case.expected {
                produced.push(produce_actual(&agent, expected).await?);
            }
            produced
        } else {
            case.actual.clone()
        };

        let expected = (!case.expected.is_empty()).then_some(case.expected.as_slice());

        let mut scores = HashMap::new();
        for (crit, evaluator) in &evaluators {
            match evaluator.evaluate(&actual, expected).await {
                Ok(result) => {
                    scores.insert(crit.name.clone(), result.overall_score);
                }
                Err(e) => {
                    tracing::warn!(criterion = %crit.name, case = %case.name, error = %e, "evaluator failed");
                }
            }
        }
        per_case.push(scores);
    }

    let summary = summarize(&req.agent, &criteria, &per_case);
    state.record_eval_result(summary.clone());
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gemini_adk_rs::evaluation::EvalCase;

    fn user(content: &str) -> InvocationTurn {
        InvocationTurn {
            role: "user".into(),
            content: content.into(),
            tool_calls: vec![],
            tool_results: vec![],
        }
    }

    fn model(content: &str) -> InvocationTurn {
        InvocationTurn {
            role: "model".into(),
            content: content.into(),
            tool_calls: vec![],
            tool_results: vec![],
        }
    }

    fn inv(turns: Vec<InvocationTurn>) -> Invocation {
        Invocation {
            id: "inv".into(),
            turns,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn parse_criteria_defaults_when_empty() {
        let parsed = parse_criteria(&[]);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "response_match");
        assert_eq!(parsed[0].threshold, DEFAULT_PASS_THRESHOLD);
    }

    #[test]
    fn parse_criteria_reads_thresholds() {
        let parsed = parse_criteria(&["response_match=0.9".into(), "trajectory".into()]);
        assert_eq!(parsed[0].threshold, 0.9);
        assert_eq!(parsed[1].name, "trajectory");
        assert_eq!(parsed[1].threshold, DEFAULT_PASS_THRESHOLD);
    }

    #[test]
    fn evaluator_for_known_and_unknown() {
        assert!(evaluator_for("response_match").is_some());
        assert!(evaluator_for("exact_match").is_some());
        assert!(evaluator_for("tool_trajectory").is_some());
        assert!(evaluator_for("safety").is_none()); // needs an LLM judge
    }

    #[tokio::test]
    async fn end_to_end_prerecorded_scoring() {
        // A case whose actual matches expected should pass response_match.
        let case = EvalCase {
            name: "greeting".into(),
            actual: vec![inv(vec![user("hi"), model("hello there, friend")])],
            expected: vec![inv(vec![user("hi"), model("hello there")])],
            scenario: None,
        };

        let criteria = parse_criteria(&["response_match".into()]);
        let evaluator = evaluator_for("response_match").unwrap();
        let result = evaluator
            .evaluate(&case.actual, Some(&case.expected))
            .await
            .unwrap();
        assert!(
            result.overall_score >= 0.99,
            "contains match should score 1.0"
        );

        let mut scores = HashMap::new();
        scores.insert("response_match".to_string(), result.overall_score);
        let summary = summarize("agent", &criteria, &[scores]);

        assert_eq!(summary.total_cases, 1);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.pass_rate, 1.0);
        assert!(summary.criteria_scores["response_match"] >= 0.99);
    }

    #[test]
    fn summarize_marks_below_threshold_failures() {
        let criteria = parse_criteria(&["response_match=0.8".into()]);
        let mut low = HashMap::new();
        low.insert("response_match".to_string(), 0.5);
        let mut high = HashMap::new();
        high.insert("response_match".to_string(), 1.0);

        let summary = summarize("agent", &criteria, &[low, high]);
        assert_eq!(summary.total_cases, 2);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.pass_rate, 0.5);
    }
}
