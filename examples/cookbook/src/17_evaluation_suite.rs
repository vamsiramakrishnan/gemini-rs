//! Walk Example 17: Evaluation Suite — E::suite, E::response_match, E::trajectory, E::safety
//!
//! Demonstrates building evaluation suites to assess agent quality. The E namespace
//! provides criteria for scoring agent outputs against expected values. Multiple
//! criteria compose with `|` and can be applied to entire test suites.
//!
//! Features used:
//!   - E::suite (test case builder)
//!   - E::response_match (exact match scoring)
//!   - E::contains_match (substring scoring)
//!   - E::safety (safety check placeholder)
//!   - E::trajectory (tool call sequence validation)
//!   - E::semantic_match (LLM-judged similarity)
//!   - E::hallucination (hallucination detection)
//!   - E::custom (user-defined criteria)
//!   - E::persona (persona-based evaluation)
//!   - `|` operator for criteria composition
//!   - EvalSuite with case builder

use adk_rs_fluent::prelude::*;

fn main() {
    println!("=== Walk 17: Evaluation Suite ===\n");

    // ── Part 1: Individual Evaluation Criteria ───────────────────────────

    println!("--- Part 1: Individual Criteria ---");

    // Exact match — output must equal expected exactly
    let exact = E::response_match();
    println!("  response_match('hello', 'hello'): {}", exact.score("hello", "hello"));
    println!("  response_match('hello', 'world'): {}", exact.score("hello", "world"));
    println!("  response_match(' hello ', 'hello'): {}", exact.score(" hello ", "hello"));

    // Contains match — output must contain expected as substring
    let contains = E::contains_match();
    println!(
        "  contains_match('hello world', 'world'): {}",
        contains.score("hello world", "world")
    );
    println!(
        "  contains_match('hello', 'world'): {}",
        contains.score("hello", "world")
    );

    // Safety — placeholder that always passes
    let safety = E::safety();
    println!("  safety('anything', ''): {}", safety.score("anything", ""));

    // Trajectory — placeholder for tool call validation
    let trajectory = E::trajectory();
    println!(
        "  trajectory('output', 'expected'): {}",
        trajectory.score("output", "expected")
    );

    // Semantic match — placeholder for LLM-judged similarity
    let semantic = E::semantic_match();
    println!(
        "  semantic_match('output', 'expected'): {}",
        semantic.score("output", "expected")
    );

    // Hallucination — placeholder for hallucination detection
    let hallucination = E::hallucination();
    println!(
        "  hallucination('output', 'expected'): {}",
        hallucination.score("output", "expected")
    );

    // ── Part 2: Custom Evaluation Criteria ───────────────────────────────

    println!("\n--- Part 2: Custom Criteria ---");

    // Word count criterion — scores based on output length
    let word_count = E::custom("word_count", |output, _expected| {
        let words = output.split_whitespace().count();
        if (10..=100).contains(&words) {
            1.0
        } else if words >= 5 {
            0.5
        } else {
            0.0
        }
    });
    println!(
        "  word_count('short', ''): {}",
        word_count.score("short", "")
    );
    println!(
        "  word_count('this is a reasonably long output with many words in it and more', ''): {}",
        word_count.score("this is a reasonably long output with many words in it and more", "")
    );

    // JSON validity criterion
    let json_valid = E::custom("json_valid", |output, _expected| {
        match serde_json::from_str::<serde_json::Value>(output) {
            Ok(_) => 1.0,
            Err(_) => 0.0,
        }
    });
    println!(
        "  json_valid('{{\"key\": 1}}', ''): {}",
        json_valid.score(r#"{"key": 1}"#, "")
    );
    println!(
        "  json_valid('not json', ''): {}",
        json_valid.score("not json", "")
    );

    // Similarity ratio criterion (character-level)
    let char_similarity = E::custom("char_similarity", |output, expected| {
        if expected.is_empty() && output.is_empty() {
            return 1.0;
        }
        if expected.is_empty() || output.is_empty() {
            return 0.0;
        }
        let matching = output
            .chars()
            .zip(expected.chars())
            .filter(|(a, b)| a == b)
            .count();
        let max_len = output.len().max(expected.len());
        matching as f64 / max_len as f64
    });
    println!(
        "  char_similarity('hello', 'hello'): {}",
        char_similarity.score("hello", "hello")
    );
    println!(
        "  char_similarity('hello', 'hallo'): {:.2}",
        char_similarity.score("hello", "hallo")
    );

    // ── Part 3: Composing Criteria with `|` ──────────────────────────────

    println!("\n--- Part 3: Composing Criteria ---");

    let composite = E::response_match() | E::contains_match() | E::safety();
    println!("  Composite has {} criteria", composite.len());

    // Score all criteria at once
    let scores = composite.score_all("hello world", "hello");
    println!("  Scoring 'hello world' against 'hello':");
    for (name, score) in &scores {
        println!("    {name}: {score}");
    }

    // ── Part 4: Evaluation Suite Builder ─────────────────────────────────

    println!("\n--- Part 4: Evaluation Suite ---");

    let suite = E::suite()
        .case("What is 2+2?", "4")
        .case("What is the capital of France?", "Paris")
        .case("Is Rust memory safe?", "Yes")
        .case("What color is the sky?", "Blue")
        .criteria(&["response_match", "contains_match", "safety"]);

    println!("  Suite: {} test cases", suite.len());
    println!("  Criteria: {:?}", suite.criteria_names);

    // Run the suite against simulated agent outputs
    let simulated_outputs = ["4", "Paris, France", "Yes, Rust is memory safe", "The sky is Blue"];

    let criteria = E::response_match() | E::contains_match() | E::safety();

    println!("\n  Running suite:");
    let mut total_scores: Vec<f64> = vec![0.0; criteria.criteria.len()];
    let mut case_count = 0;

    for (case, output) in suite.cases.iter().zip(simulated_outputs.iter()) {
        let scores = criteria.score_all(output, &case.expected);
        print!("    Q: {:40} | ", case.prompt);
        for (name, score) in &scores {
            print!("{name}={score:.1} ");
        }
        println!();

        for (i, (_, score)) in scores.iter().enumerate() {
            total_scores[i] += score;
        }
        case_count += 1;
    }

    // Summary
    println!("\n  Average scores:");
    for (i, criterion) in criteria.criteria.iter().enumerate() {
        let avg = total_scores[i] / case_count as f64;
        println!("    {}: {avg:.2}", criterion.name());
    }

    // ── Part 5: Persona-Based Evaluation ─────────────────────────────────

    println!("\n--- Part 5: Persona Evaluation ---");

    let impatient = E::persona(
        "impatient_user",
        "A user who is in a hurry and wants quick, concise answers",
    );
    let confused = E::persona(
        "confused_user",
        "A user who needs extra explanation and hand-holding",
    );

    println!(
        "  impatient_user scores non-empty output: {}",
        impatient.score("Here is your answer", "")
    );
    println!(
        "  impatient_user scores empty output: {}",
        impatient.score("", "")
    );
    println!(
        "  confused_user criterion name: {}",
        confused.name()
    );

    // ── Part 6: Multi-Dimensional Quality Report ─────────────────────────

    println!("\n--- Part 6: Quality Report ---");

    let quality_criteria = E::response_match()
        | E::contains_match()
        | E::safety()
        | E::semantic_match()
        | E::hallucination()
        | E::trajectory();

    let test_output = "The capital of France is Paris, located along the Seine river.";
    let expected = "Paris";

    let scores = quality_criteria.score_all(test_output, expected);
    println!("  Quality report for: \"{test_output}\"");
    println!("  Expected: \"{expected}\"");
    println!("  --------------------------");
    for (name, score) in &scores {
        let bar_len = (score * 20.0) as usize;
        let bar: String = "#".repeat(bar_len);
        let space: String = " ".repeat(20 - bar_len);
        println!("    {name:20} [{bar}{space}] {score:.2}");
    }

    println!("\nDone.");
}
