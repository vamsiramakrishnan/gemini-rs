//! Turn-end extraction latency against the Gemini API, with no Live session.
//!
//! ```text
//! cargo test -p gemini-adk-fluent-rs --test extraction_latency -- --ignored --nocapture
//! ```
//!
//! Replays the pharmacy fixture's `extract` entries over the transcript of a
//! live run's turn whose end took 49 s ("I need a refill of my Lisinopril"),
//! through the same [`LlmExtractor`] path a session uses: thinking budget 0,
//! dropped if the model rejects it, one retry on a transient error. Each
//! round runs every extractor at once, as the turn pipeline does, and the
//! round's wall time is what the turn (and any tool call behind it) waits.
//! `EXTRACTION_PARALLEL` rounds run at a time, as the live harness runs
//! sessions.
//!
//! | Variable | Default | |
//! |---|---|---|
//! | `EXTRACTION_MODELS` | `gemini-flash-latest,gemini-3.5-flash-lite` | Comma-separated text models |
//! | `EXTRACTION_ROUNDS` | `30` | Rounds per model |
//! | `EXTRACTION_PARALLEL` | `3` | Rounds at a time |
//! | `EXTRACTION_THINKING_BUDGET` | the extractor's (0, then 64 if rejected) | Thinking budget sent |
//!
//! Prints, per model, the round time's p50 / p90 / p99 / max, how many
//! rounds took over 5 and 10 s, errors, and each underlying API call's
//! latency, so a slow round shows whether one call was slow or a call was
//! retried; and how many rounds extracted the turn correctly (the caller's
//! name and date of birth, Lisinopril, and no caller signal set).

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use gemini_adk_fluent_rs::spec::ExtractSpec;
use gemini_adk_rs::State;
use gemini_adk_rs::live::extractor::{LlmExtractor, TurnExtractor};
use gemini_adk_rs::live::transcript::TranscriptTurn;
use gemini_adk_rs::llm::{BaseLlm, GeminiLlm, GeminiLlmParams, LlmError, LlmRequest, LlmResponse};
use serde_json::Value;

/// One underlying API call: how long, and how it ended.
#[derive(Clone, Debug)]
struct Call {
    ms: u128,
    outcome: String,
}

/// Times every call the extractor makes, so a slow round can be told apart
/// into one slow call or a rejected-and-resent one.
struct Timed {
    inner: GeminiLlm,
    calls: parking_lot::Mutex<Vec<Call>>,
}

#[async_trait]
impl BaseLlm for Timed {
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse, LlmError> {
        let started = Instant::now();
        let budget = request.thinking_budget;
        let result = self.inner.generate(request).await;
        let outcome = match &result {
            Ok(_) => format!("ok (budget {budget:?})"),
            Err(e) => format!("{e} (budget {budget:?})"),
        };
        self.calls.lock().push(Call {
            ms: started.elapsed().as_millis(),
            outcome,
        });
        result
    }
}

/// The pharmacy-refill call up to the turn whose end took 49 s.
fn transcript() -> Vec<TranscriptTurn> {
    let turn = |n: u32, user: &str, model: &str| TranscriptTurn {
        turn_number: n,
        user: user.into(),
        model: model.into(),
        tool_calls: Vec::new(),
        timestamp: Instant::now(),
    };
    vec![
        turn(
            0,
            "",
            "Thank you for calling Corner Pharmacy. This call is recorded. You are speaking with an \
             automated assistant. May I have your full name and date of birth, please?",
        ),
        turn(
            1,
            "Hi, I'm John Carter. Date of birth, November 5th, 1972.",
            "",
        ),
        turn(
            2,
            "I need a refill of my lisinopril.",
            "Thank you, Mr. Carter. I found two prescriptions for you: Lisinopril and \
             Atorvastatin. Which one would you like to refill today?",
        ),
    ]
}

fn extract_specs() -> Vec<ExtractSpec> {
    let doc: Value =
        serde_json::from_str(include_str!("fixtures/live/pharmacy.json")).expect("fixture");
    serde_json::from_value(doc["extract"].clone()).expect("extract entries")
}

fn percentile(sorted: &[u128], q: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

struct Round {
    ms: u128,
    per_extractor: Vec<(String, u128, Result<Value, String>)>,
}

/// Whether a round read the turn right: John Carter, 1972-11-05,
/// Lisinopril, and none of the caller signals (the caller only named a
/// medication).
fn correct(round: &Round) -> bool {
    let get = |name: &str| {
        round
            .per_extractor
            .iter()
            .find(|(n, _, _)| n == name)
            .and_then(|(_, _, r)| r.as_ref().ok())
            .cloned()
            .unwrap_or(Value::Null)
    };
    let identity = get("caller_identity");
    let choice = get("prescription_choice");
    let signals = get("caller_signals");
    let name = identity["patient_name"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    name.contains("john")
        && name.contains("carter")
        && identity["date_of_birth"] == "1972-11-05"
        && choice["medication"]
            .as_str()
            .is_some_and(|m| m.to_lowercase().contains("lisinopril"))
        && signals
            .as_object()
            .is_none_or(|m| m.values().all(|v| v != &Value::Bool(true)))
}

async fn round(extractors: &[Arc<LlmExtractor>], turns: &[TranscriptTurn]) -> Round {
    let state = State::new();
    let started = Instant::now();
    let runs = extractors.iter().map(|e| {
        let state = state.clone();
        async move {
            let t = Instant::now();
            let window = &turns[turns.len().saturating_sub(e.window_size())..];
            let result = e.extract_with_state(window, &state).await;
            (
                e.name().to_string(),
                t.elapsed().as_millis(),
                result.map_err(|e| e.to_string()),
            )
        }
    });
    let per_extractor = futures_util::future::join_all(runs).await;
    Round {
        ms: started.elapsed().as_millis(),
        per_extractor,
    }
}

#[tokio::test]
#[ignore = "calls the Gemini API; needs GEMINI_API_KEY"]
async fn turn_end_extraction_latency() {
    if std::env::var("GEMINI_API_KEY").is_err() {
        eprintln!("GEMINI_API_KEY not set; skipping");
        return;
    }
    let models: Vec<String> = std::env::var("EXTRACTION_MODELS")
        .unwrap_or_else(|_| "gemini-flash-latest,gemini-3.5-flash-lite".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let rounds: usize = std::env::var("EXTRACTION_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let parallel: usize = std::env::var("EXTRACTION_PARALLEL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let budget: Option<u32> = std::env::var("EXTRACTION_THINKING_BUDGET")
        .ok()
        .and_then(|v| v.parse().ok());
    let turns = Arc::new(transcript());
    let specs = extract_specs();

    for model in models {
        let llm = Arc::new(Timed {
            inner: GeminiLlm::try_new(GeminiLlmParams {
                model: Some(model.clone()),
                ..Default::default()
            })
            .expect("model"),
            calls: parking_lot::Mutex::new(Vec::new()),
        });
        let extractors: Arc<Vec<Arc<LlmExtractor>>> = Arc::new(
            specs
                .iter()
                .map(|e| {
                    Arc::new(
                        LlmExtractor::new(
                            e.name.clone(),
                            llm.clone(),
                            e.instruction.clone(),
                            e.window,
                        )
                        .with_schema(e.schema.clone())
                        .with_min_words(3)
                        .with_thinking_budget(budget.or(Some(0))),
                    )
                })
                .collect(),
        );
        let semaphore = Arc::new(tokio::sync::Semaphore::new(parallel));
        let started = Instant::now();
        let handles: Vec<_> = (0..rounds)
            .map(|_| {
                let (extractors, turns, sem) =
                    (extractors.clone(), turns.clone(), semaphore.clone());
                tokio::spawn(async move {
                    let _permit = sem.acquire_owned().await.unwrap();
                    round(&extractors, &turns).await
                })
            })
            .collect();
        let mut results = Vec::new();
        for h in handles {
            results.push(h.await.expect("round"));
        }
        let wall = started.elapsed();

        let mut times: Vec<u128> = results.iter().map(|r| r.ms).collect();
        times.sort_unstable();
        let errors: Vec<String> = results
            .iter()
            .flat_map(|r| r.per_extractor.iter())
            .filter_map(|(name, _, r)| r.as_ref().err().map(|e| format!("{name}: {e}")))
            .collect();
        let right = results.iter().filter(|r| correct(r)).count();
        let calls = llm.calls.lock().clone();
        let mut call_ms: Vec<u128> = calls.iter().map(|c| c.ms).collect();
        call_ms.sort_unstable();
        let over = |s: u128| times.iter().filter(|t| **t > s * 1000).count();
        println!("\n## {model}\n");
        println!(
            "rounds {rounds} ({parallel} at a time, {:.0} s): round p50 {} / p90 {} / p99 {} / max {} ms; over 5 s: {}, over 10 s: {}; extractor errors: {}",
            wall.as_secs_f64(),
            percentile(&times, 0.5),
            percentile(&times, 0.9),
            percentile(&times, 0.99),
            times.last().copied().unwrap_or(0),
            over(5),
            over(10),
            errors.len()
        );
        println!("correct rounds: {right}/{rounds}");
        if let Some(r) = results.iter().find(|r| !correct(r)).or(results.first()) {
            for (name, _, value) in &r.per_extractor {
                println!(
                    "  {} {name}: {value:?}",
                    if correct(r) { "e.g." } else { "wrong" }
                );
            }
        }
        println!(
            "API calls {}: p50 {} / p90 {} / max {} ms",
            calls.len(),
            percentile(&call_ms, 0.5),
            percentile(&call_ms, 0.9),
            call_ms.last().copied().unwrap_or(0)
        );
        let mut outcomes: std::collections::BTreeMap<String, usize> = Default::default();
        for c in &calls {
            *outcomes
                .entry(c.outcome.chars().take(120).collect())
                .or_default() += 1;
        }
        for (outcome, n) in outcomes {
            println!("  {n:>4} × {outcome}");
        }
        let mut slow: Vec<&Call> = calls.iter().filter(|c| c.ms > 5000).collect();
        slow.sort_by_key(|c| std::cmp::Reverse(c.ms));
        for c in slow.iter().take(10) {
            println!("  slow call: {} ms, {}", c.ms, c.outcome);
        }
        for e in errors.iter().take(5) {
            println!("  error: {e}");
        }
        // Per-extractor times, to see whether one schema is the slow one.
        let mut by_name: std::collections::BTreeMap<String, Vec<u128>> = Default::default();
        for (name, ms, _) in results.iter().flat_map(|r| r.per_extractor.iter()) {
            by_name.entry(name.clone()).or_default().push(*ms);
        }
        for (name, mut v) in by_name {
            v.sort_unstable();
            println!(
                "  {name}: p50 {} / p90 {} / max {} ms",
                percentile(&v, 0.5),
                percentile(&v, 0.9),
                v.last().copied().unwrap_or(0)
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
