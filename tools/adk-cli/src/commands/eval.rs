use crate::manifest;
use adk_rs_fluent::prelude::*;
use rs_adk::{BaseLlm, GeminiLlm, GeminiLlmParams, LlmRequest};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct EvalSetFile {
    #[allow(dead_code)]
    name: String,
    cases: Vec<EvalCase>,
}

#[derive(Deserialize)]
struct EvalCase {
    id: String,
    inputs: Vec<String>,
    #[serde(default)]
    expected: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct TestConfig {
    #[serde(default = "default_threshold")]
    pass_threshold: f64,
    #[serde(default)]
    criteria: Vec<Criterion>,
}

fn default_threshold() -> f64 {
    0.7
}

#[derive(Deserialize, Clone)]
struct Criterion {
    name: String,
    weight: f64,
    description: String,
}

#[derive(Serialize)]
struct CaseResult {
    id: String,
    passed: bool,
    score: f64,
    agent_output: String,
    expected: Vec<String>,
}

// ── Entry point ──────────────────────────────────────────────────────────────

pub async fn run(
    agent_dir: &str,
    evalset_path: &str,
    config_file: Option<&str>,
    print_detailed_results: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    // Load manifest
    let dir = std::path::Path::new(agent_dir);
    let m = manifest::load_manifest(&dir.join("agent.toml"))?;
    println!("\n  Evaluating: {} ({})\n", m.name, m.model);

    // Load evalset
    let evalset_str = std::fs::read_to_string(evalset_path)?;
    let evalset: EvalSetFile = serde_json::from_str(&evalset_str)?;

    // Load test config
    let config = if let Some(cf) = config_file {
        let s = std::fs::read_to_string(cf)?;
        serde_json::from_str(&s)?
    } else {
        TestConfig {
            pass_threshold: 0.7,
            criteria: vec![],
        }
    };

    // ── Create LLM + agent ───────────────────────────────────────────
    let params = GeminiLlmParams {
        model: Some(m.model.clone()),
        ..Default::default()
    };
    let llm: Arc<dyn BaseLlm> = Arc::new(GeminiLlm::new(params));

    let mut builder = AgentBuilder::new(&m.name).instruction(&m.instruction);

    if let Some(temp) = m.temperature {
        builder = builder.temperature(temp);
    }
    if let Some(budget) = m.thinking {
        builder = builder.thinking(budget);
    }

    for tool in &m.tools {
        builder = match tool.as_str() {
            "google_search" => builder.google_search(),
            "code_execution" => builder.code_execution(),
            "url_context" => builder.url_context(),
            _ => builder,
        };
    }
    let agent = builder.build(llm.clone());

    // ── Create judge LLM (same model, used for scoring) ─────────────
    let judge_params = GeminiLlmParams {
        model: Some("gemini-2.5-flash".into()),
        ..Default::default()
    };
    let judge: Arc<dyn BaseLlm> = Arc::new(GeminiLlm::new(judge_params));

    // ── Run evaluations ──────────────────────────────────────────────
    let total = evalset.cases.len();
    let mut results: Vec<CaseResult> = Vec::with_capacity(total);

    for (i, case) in evalset.cases.iter().enumerate() {
        print!("  [{}/{}] {} ... ", i + 1, total, case.id);

        // Run agent
        let state = State::new();
        let combined_input = case.inputs.join("\n");
        state.set("input", &combined_input);

        let agent_output = match agent.run(&state).await {
            Ok(output) => output,
            Err(e) => {
                println!("ERROR: {}", e);
                results.push(CaseResult {
                    id: case.id.clone(),
                    passed: false,
                    score: 0.0,
                    agent_output: format!("Error: {}", e),
                    expected: case.expected.clone(),
                });
                continue;
            }
        };

        // Score with LLM judge
        let score = score_case(&judge, &agent_output, &case.expected, &config.criteria).await;
        let passed = score >= config.pass_threshold;

        if passed {
            println!("PASS ({:.0}%)", score * 100.0);
        } else {
            println!("FAIL ({:.0}%)", score * 100.0);
        }

        results.push(CaseResult {
            id: case.id.clone(),
            passed,
            score,
            agent_output,
            expected: case.expected.clone(),
        });
    }

    // ── Summary ──────────────────────────────────────────────────────
    let passed_count = results.iter().filter(|r| r.passed).count();
    let avg_score: f64 = if results.is_empty() {
        0.0
    } else {
        results.iter().map(|r| r.score).sum::<f64>() / results.len() as f64
    };

    println!(
        "\n  Results: {}/{} passed ({:.0}%)",
        passed_count,
        total,
        avg_score * 100.0
    );
    println!("  Threshold: {:.0}%\n", config.pass_threshold * 100.0);

    // ── Detailed results ─────────────────────────────────────────────
    if print_detailed_results {
        println!(
            "  {:<16} {:<6} {:<8} OUTPUT (truncated)",
            "CASE", "PASS", "SCORE"
        );
        println!("  {}", "-".repeat(72));
        for r in &results {
            let status = if r.passed { "yes" } else { "NO" };
            let truncated: String = r.agent_output.chars().take(40).collect();
            let truncated = truncated.replace('\n', " ");
            println!(
                "  {:<16} {:<6} {:<8.0}% {}",
                r.id,
                status,
                r.score * 100.0,
                truncated
            );
        }
        println!();
    }

    // ── Exit code ────────────────────────────────────────────────────
    if passed_count < total {
        std::process::exit(1);
    }

    Ok(())
}

// ── LLM judge scoring ────────────────────────────────────────────────────────

async fn score_case(
    judge: &Arc<dyn BaseLlm>,
    output: &str,
    expected: &[String],
    criteria: &[Criterion],
) -> f64 {
    // No expectations = auto-pass
    if expected.is_empty() && criteria.is_empty() {
        return 1.0;
    }

    let criteria_text = if criteria.is_empty() {
        "The output should mention or address the expected keywords/phrases.".to_string()
    } else {
        criteria
            .iter()
            .map(|c| format!("- {} (weight {:.1}): {}", c.name, c.weight, c.description))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let prompt = format!(
        "You are an evaluation judge. Score the agent's output on a scale of 0.0 to 1.0.\n\n\
        AGENT OUTPUT:\n{output}\n\n\
        EXPECTED (keywords/phrases that should appear):\n{expected}\n\n\
        CRITERIA:\n{criteria_text}\n\n\
        Respond with ONLY a single decimal number between 0.0 and 1.0. Nothing else.",
        expected = expected.join(", "),
    );

    let request = LlmRequest::from_text(prompt);
    match judge.generate(request).await {
        Ok(response) => {
            let text = response.text();
            // Extract the first float from the response
            text.trim()
                .parse::<f64>()
                .unwrap_or_else(|_| {
                    // Try to find a float pattern in the response
                    text.split_whitespace()
                        .find_map(|w| {
                            w.trim_matches(|c: char| !c.is_ascii_digit() && c != '.')
                                .parse::<f64>()
                                .ok()
                        })
                        .unwrap_or(0.0)
                })
                .clamp(0.0, 1.0)
        }
        Err(e) => {
            eprintln!("  Judge error: {}", e);
            // Fallback: simple keyword matching
            if expected.is_empty() {
                return 1.0;
            }
            let output_lower = output.to_lowercase();
            let matches = expected
                .iter()
                .filter(|e| output_lower.contains(&e.to_lowercase()))
                .count();
            matches as f64 / expected.len() as f64
        }
    }
}
