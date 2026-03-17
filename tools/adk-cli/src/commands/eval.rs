use crate::manifest;
use serde::{Deserialize, Serialize};
use std::path::Path;

// ---------------------------------------------------------------------------
// Eval set file format
// ---------------------------------------------------------------------------

/// Top-level structure of a `.evalset.json` file.
#[derive(Debug, Deserialize)]
pub struct EvalSetFile {
    /// Human-readable name for this evaluation set.
    #[serde(default)]
    pub name: String,
    /// The evaluation cases to run.
    pub cases: Vec<EvalCase>,
}

/// A single evaluation case.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct EvalCase {
    /// Identifier for this case.
    pub id: String,
    /// User input(s) to send to the agent.
    pub inputs: Vec<String>,
    /// Expected outputs or reference answers.
    #[serde(default)]
    pub expected: Vec<String>,
    /// Per-case metadata/tags.
    #[serde(default)]
    pub tags: Vec<String>,
}

// ---------------------------------------------------------------------------
// Test config format
// ---------------------------------------------------------------------------

/// Scoring configuration loaded from `test_config.json`.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct TestConfig {
    /// Minimum passing score (0.0 - 1.0).
    #[serde(default = "default_threshold")]
    pub pass_threshold: f64,
    /// Evaluator criteria.
    #[serde(default)]
    pub criteria: Vec<Criterion>,
}

fn default_threshold() -> f64 {
    0.7
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct Criterion {
    pub name: String,
    pub weight: f64,
    #[serde(default)]
    pub description: String,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            pass_threshold: default_threshold(),
            criteria: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct CaseResult {
    id: String,
    passed: bool,
    score: f64,
    agent_output: String,
    expected: Vec<String>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run evaluations from an eval set against the agent.
pub async fn run(
    agent_dir: &str,
    evalset_path: &str,
    config_file: Option<&str>,
    print_detailed_results: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    // Load agent manifest
    let dir = Path::new(agent_dir);
    let agent = manifest::load_manifest(&dir.join("agent.toml"))?;
    println!("Evaluating agent: {} (model: {})", agent.name, agent.model);

    // Load eval set
    let evalset_content = std::fs::read_to_string(evalset_path)?;
    let evalset: EvalSetFile = serde_json::from_str(&evalset_content)?;
    println!(
        "Eval set: {} ({} cases)",
        if evalset.name.is_empty() {
            evalset_path
        } else {
            &evalset.name
        },
        evalset.cases.len()
    );

    // Load test config
    let config = if let Some(path) = config_file {
        let content = std::fs::read_to_string(path)?;
        serde_json::from_str::<TestConfig>(&content)?
    } else {
        TestConfig::default()
    };

    println!("Pass threshold: {:.0}%", config.pass_threshold * 100.0);
    println!("---");

    // Run each case
    let mut results: Vec<CaseResult> = Vec::new();
    let mut passed = 0usize;

    for case in &evalset.cases {
        let agent_output = run_eval_case(&agent, case).await;
        let score = score_case(&config, case, &agent_output);
        let case_passed = score >= config.pass_threshold;
        if case_passed {
            passed += 1;
        }

        results.push(CaseResult {
            id: case.id.clone(),
            passed: case_passed,
            score,
            agent_output,
            expected: case.expected.clone(),
        });
    }

    // Print summary
    let total = results.len();
    println!(
        "\nResults: {passed}/{total} passed ({:.0}%)",
        (passed as f64 / total as f64) * 100.0
    );

    if print_detailed_results {
        println!("\n{:<20} {:<8} {:<8} {}", "CASE", "PASS", "SCORE", "OUTPUT");
        println!("{}", "-".repeat(80));
        for r in &results {
            let status = if r.passed { "PASS" } else { "FAIL" };
            let output_preview: String = r.agent_output.chars().take(40).collect();
            println!(
                "{:<20} {:<8} {:<8.2} {}",
                r.id, status, r.score, output_preview
            );
        }
    }

    if passed < total {
        std::process::exit(1);
    }

    Ok(())
}

/// Execute all inputs for a single eval case and return the final agent output.
async fn run_eval_case(agent: &manifest::AgentManifest, case: &EvalCase) -> String {
    // TODO: Wire up actual LLM call through the agent runtime.
    // For now return a placeholder.
    let _ = (agent, case);
    "(placeholder — LLM integration pending)".to_string()
}

/// Score a case result against the expected outputs using the test config criteria.
fn score_case(_config: &TestConfig, case: &EvalCase, _agent_output: &str) -> f64 {
    // TODO: Implement scoring evaluators (exact match, LLM judge, semantic similarity, etc.)
    if case.expected.is_empty() {
        // No expected output defined — auto-pass
        1.0
    } else {
        // Placeholder: always return 0 when there are expected outputs
        0.0
    }
}
