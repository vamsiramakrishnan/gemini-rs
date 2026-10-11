//! Jev's latency and reliability through Vercel AI Gateway, from Rust.
//!
//! ```text
//! cargo test -p gemini-adk-fluent-rs --test decision_latency -- --ignored --nocapture
//! ```
//!
//! Reads `AI_GATEWAY_API_KEY` from the environment or `.env.local` at the
//! repository root. Measures one caller turn's decisions:
//!
//! - sequentially, then 10 at a time;
//! - with 1 question and with 8 (Jev answers questions in parallel);
//! - with a short state and with one about ten times longer.
//!
//! Prints p50 / p90 / max latency and the error count for each, and checks
//! that the answers agree with the obvious reading of the turn.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::env::env_or_local;
use gemini_adk_rs::decision::{
    Answer, DecisionModel, DecisionRequest, GatewayDecisionModel, Question,
};
use serde_json::{Value, json};

fn state(padding: usize) -> Value {
    let mut conversation =
        vec![json!({ "agent": "Four at seven tomorrow, under Rossi. Shall I book it?" })];
    for i in 0..padding {
        conversation.insert(
            0,
            json!({ "agent": format!("Earlier turn {i}: we have tables at six, seven and nine tomorrow evening.") }),
        );
    }
    conversation.push(json!({ "caller": "Yes, that's all correct. Please book it." }));
    json!({ "conversation": conversation, "facts": { "party_size": 4, "slot": "tomorrow 19:00" } })
}

fn questions(n: usize) -> Vec<(String, Question)> {
    let mut q = vec![
        (
            "confirmed".to_string(),
            Question::boolean(
                "In their last turn, did the caller agree to the booking that was read back?",
            ),
        ),
        (
            "wants_person".to_string(),
            Question::boolean("Did the caller ask to speak to a person?"),
        ),
        (
            "next".to_string(),
            Question::choice(
                "What should the conversation do next?",
                [
                    ("book", "the caller confirmed: make the booking"),
                    (
                        "read_back_again",
                        "the caller changed or questioned a detail",
                    ),
                    ("handoff", "the caller asked for a person"),
                    ("stay", "nothing decided yet"),
                ],
            ),
        ),
        (
            "frustration".to_string(),
            Question::score(
                "How frustrated is the caller?",
                ["calm", "impatient", "frustrated", "angry"],
            ),
        ),
        (
            "urgent".to_string(),
            Question::boolean("Did the caller describe a medical emergency?"),
        ),
        (
            "changed_detail".to_string(),
            Question::boolean("Did the caller change a detail they gave earlier?"),
        ),
        (
            "asked_question".to_string(),
            Question::boolean("Did the caller ask the agent a question?"),
        ),
        (
            "declined".to_string(),
            Question::boolean("Did the caller decline the booking?"),
        ),
    ];
    q.truncate(n);
    q
}

#[derive(Default)]
struct Stats {
    ms: Vec<u128>,
    errors: Vec<String>,
    wrong: Vec<String>,
}

impl Stats {
    fn line(&self, label: &str) -> String {
        let mut ms = self.ms.clone();
        ms.sort_unstable();
        let at = |q: f64| {
            ms.get(((ms.len().max(1) - 1) as f64 * q).round() as usize)
                .copied()
                .unwrap_or(0)
        };
        format!(
            "{label:34} n={:3}  p50={:5} ms  p90={:5} ms  max={:5} ms  errors={}  wrong={}",
            ms.len(),
            at(0.5),
            at(0.9),
            ms.last().copied().unwrap_or(0),
            self.errors.len(),
            self.wrong.len()
        )
    }
}

async fn one(model: &GatewayDecisionModel, n_questions: usize, padding: usize, stats: &mut Stats) {
    let request = DecisionRequest::new(state(padding), questions(n_questions));
    let started = Instant::now();
    match model.decide(request).await {
        Ok(r) => {
            stats.ms.push(started.elapsed().as_millis());
            if r.answers
                .get("confirmed")
                .and_then(Answer::probability)
                .is_none_or(|p| p < 0.5)
            {
                stats
                    .wrong
                    .push(format!("confirmed: {:?}", r.answers.get("confirmed")));
            }
            if let Some(Answer::Choice { choice, .. }) = r.answers.get("next")
                && choice != "book"
            {
                stats.wrong.push(format!("next: {choice}"));
            }
        }
        Err(e) => stats.errors.push(e.to_string()),
    }
}

#[tokio::test]
#[ignore = "calls Vercel AI Gateway; needs AI_GATEWAY_API_KEY and Jev access"]
async fn jev_latency_and_reliability() {
    let Some(key) = env_or_local("AI_GATEWAY_API_KEY") else {
        eprintln!("AI_GATEWAY_API_KEY not set; skipping");
        return;
    };
    let model = Arc::new(
        GatewayDecisionModel::new(GatewayDecisionModel::JEV, key)
            .with_timeout(Duration::from_secs(15)),
    );
    let mut lines = Vec::new();

    // Warm the connection once, so the first measurement is not the TLS handshake.
    one(&model, 1, 0, &mut Stats::default()).await;

    for (label, n_questions, padding) in [
        ("sequential, 4 questions", 4, 0),
        ("sequential, 1 question", 1, 0),
        ("sequential, 8 questions", 8, 0),
        ("sequential, 4 questions, long state", 4, 40),
    ] {
        let mut stats = Stats::default();
        for _ in 0..12 {
            one(&model, n_questions, padding, &mut stats).await;
        }
        lines.push(stats.line(label));
        for e in stats.errors.iter().chain(stats.wrong.iter()).take(3) {
            lines.push(format!("    {e}"));
        }
    }

    // Ten at once, three rounds.
    let mut stats = Stats::default();
    for _ in 0..3 {
        let mut handles = Vec::new();
        for _ in 0..10 {
            let model = model.clone();
            handles.push(tokio::spawn(async move {
                let mut s = Stats::default();
                one(&model, 4, 0, &mut s).await;
                s
            }));
        }
        for h in handles {
            let s = h.await.unwrap();
            stats.ms.extend(s.ms);
            stats.errors.extend(s.errors);
            stats.wrong.extend(s.wrong);
        }
    }
    lines.push(stats.line("10 concurrent, 4 questions"));
    for e in stats.errors.iter().take(3) {
        lines.push(format!("    {e}"));
    }

    println!(
        "\nJev via AI Gateway ({})\n{}",
        GatewayDecisionModel::JEV,
        lines.join("\n")
    );
}
