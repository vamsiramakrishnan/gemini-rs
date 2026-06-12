//! # 03 — Composition Algebra: S · P · T · G
//!
//! Four of the composition namespaces, each with its own operator:
//!
//! - **`S::`** state transforms — compose with `>>` (`pick`/`rename`/`merge`/
//!   `compute`/`set`/`drop`/`flatten`/`defaults`/`map`).
//! - **`P::`** prompt sections — compose with `+` (`role`/`task`/`constraint`/
//!   `format`/`example`/`context`/`persona`/`guidelines`), then `.render()`.
//! - **`T::`** tools — compose with `|` (`google_search`/`code_execution`/
//!   `url_context`/`simple`/`mock`/`toolset`).
//! - **`G::`** output guards — compose with `|`, all must pass (`json`/`length`/
//!   `pii`/`topic`/`budget`/`regex`/`custom`).
//!
//! Construction-only (no network), so it runs without credentials.

use gemini_adk_fluent_rs::compose::tools::ToolCompositeEntry;
use gemini_adk_fluent_rs::prelude::*;
use serde_json::json;
use std::sync::Arc;

fn main() {
    println!("=== 03: Composition Algebra (S · P · T · G) ===\n");
    state_transforms();
    prompt_composition();
    tool_composition();
    guards();
    println!("\nDone.");
}

// ─────────────────────────────────────────────────────────────────────────────
// S:: — state transforms (compose with >>)
// ─────────────────────────────────────────────────────────────────────────────
fn state_transforms() {
    println!("── S:: state transforms ───────────────────────────────\n");

    // Individual transforms apply to a serde_json::Value in place.
    let mut state = json!({"name": "Alice", "age": 30, "noise": "ignore"});
    S::pick(&["name", "age"]).apply(&mut state);
    assert_eq!(state, json!({"name": "Alice", "age": 30}));
    println!("S::pick([name, age]) -> {state}");

    let mut state = json!({"price": 100, "quantity": 5});
    S::compute("total", |s| {
        let price = s.get("price").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let qty = s.get("quantity").and_then(|v| v.as_f64()).unwrap_or(0.0);
        json!(price * qty)
    })
    .apply(&mut state);
    assert_eq!(state["total"], json!(500.0));
    println!("S::compute(total = price*qty) -> {state}");

    let mut state = json!({"user": {"name": "Carol", "role": "admin"}});
    S::flatten("user").apply(&mut state);
    assert_eq!(state["name"], "Carol");
    println!("S::flatten(user) -> {state}");

    // Transforms chain sequentially with >>.
    let chain = S::pick(&["findings", "score"])
        >> S::rename(&[("findings", "research")])
        >> S::compute("grade", |s| {
            let score = s.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
            json!(if score >= 90.0 {
                "A"
            } else if score >= 80.0 {
                "B"
            } else {
                "C"
            })
        })
        >> S::set("reviewed", json!(true));
    let mut state = json!({"findings": "Quantum computing...", "score": 92.0, "noise": "x"});
    chain.apply(&mut state);
    assert_eq!(state["research"], "Quantum computing...");
    assert_eq!(state["grade"], "A");
    assert_eq!(state["reviewed"], true);
    assert!(state.get("noise").is_none());
    println!("chained (pick >> rename >> compute >> set) -> {state}\n");
}

// ─────────────────────────────────────────────────────────────────────────────
// P:: — prompt composition (compose with +)
// ─────────────────────────────────────────────────────────────────────────────
fn prompt_composition() {
    println!("── P:: prompt composition ─────────────────────────────\n");

    // Each P:: factory is a PromptSection; + builds a PromptComposite.
    let prompt = P::role("senior technical writer")
        + P::task("Write a blog post about Rust async patterns")
        + P::constraint("Maximum 1500 words")
        + P::format("Markdown with headers and code blocks");
    println!(
        "{} sections, rendered:\n{}\n",
        prompt.sections.len(),
        prompt.render()
    );

    // Personas, context, guidelines, and examples are all sections too.
    let support = P::persona("friendly, patient, and empathetic")
        + P::role("customer support agent")
        + P::task("Help the customer resolve their issue")
        + P::context("The customer is on the Enterprise plan")
        + P::guidelines(&[
            "Always greet the customer by name",
            "Acknowledge the issue before solutions",
            "Offer to escalate if unresolved after 3 exchanges",
        ])
        + P::constraint("Keep responses under 200 words");
    println!("support prompt: {} sections", support.sections.len());

    // PromptComposite implements Into<String> for AgentBuilder::instruction().
    let instruction: String = (P::role("analyst")
        + P::task("Analyze quarterly revenue")
        + P::format("JSON: trend, growth_rate, summary"))
    .into();
    let _agent = AgentBuilder::new("revenue-analyst")
        .instruction(&instruction)
        .temperature(0.2);
    println!("prompt-as-instruction: {} chars", instruction.len());

    // Sections can be filtered and reordered by name.
    let full = P::role("researcher")
        + P::task("Find data on climate change")
        + P::constraint("Peer-reviewed sources only")
        + P::format("APA citation format");
    let minimal = full.clone().only_by_name(&["role", "task"]);
    println!(
        "filtered (role + task): {} sections\n",
        minimal.sections.len()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// T:: — tool composition (compose with |)
// ─────────────────────────────────────────────────────────────────────────────
fn tool_composition() {
    println!("── T:: tool composition ───────────────────────────────\n");

    // Built-in Gemini tools need no local handler.
    let built_ins = T::google_search() | T::code_execution() | T::url_context();
    println!("built-ins: {} entries", built_ins.len());

    // T::simple wraps a closure; T::mock returns a fixed response (great for tests).
    let all_tools = T::google_search()
        | T::simple("get_weather", "Get weather", |args| async move {
            let city = args
                .get("city")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown");
            Ok(json!({ "temp_c": 22, "city": city }))
        })
        | T::mock(
            "search_kb",
            "Search KB",
            json!({"results": [{"title": "FAQ"}]}),
        );
    println!("composite: {} entries", all_tools.len());
    for (i, entry) in all_tools.entries.iter().enumerate() {
        let name = match entry {
            ToolCompositeEntry::Function(f) => f.name().to_string(),
            ToolCompositeEntry::BuiltIn(_) => "(built-in)".to_string(),
            ToolCompositeEntry::Mock { name, .. } => format!("{name} (mock)"),
            _ => "(other)".to_string(),
        };
        println!("  entry {}: {name}", i + 1);
    }

    // T::toolset bulk-registers ToolFunction trait objects.
    let tool_a: Arc<dyn ToolFunction> =
        Arc::new(SimpleTool::new("tool_a", "Tool A", None, |_| async {
            Ok(json!(null))
        }));
    let tool_b: Arc<dyn ToolFunction> =
        Arc::new(SimpleTool::new("tool_b", "Tool B", None, |_| async {
            Ok(json!(null))
        }));
    let bulk = T::toolset(vec![tool_a, tool_b]);
    println!("bulk toolset: {} entries\n", bulk.len());
}

// ─────────────────────────────────────────────────────────────────────────────
// G:: — output guards (compose with |, all must pass)
// ─────────────────────────────────────────────────────────────────────────────
fn guards() {
    println!("── G:: output guards ──────────────────────────────────\n");

    // Single guards validate one string with .check().
    println!("G::json valid:   {:?}", G::json().check(r#"{"k":"v"}"#));
    println!("G::json invalid: {:?}", G::json().check("not json"));
    println!(
        "G::length(10,100) 'hi': {:?}",
        G::length(10, 100).check("hi")
    );
    println!(
        "G::pii email:    {:?}",
        G::pii().check("mail me at a@b.com")
    );
    println!(
        "G::topic block:  {:?}",
        G::topic(&["violence", "gambling"]).check("depicted violence")
    );

    let custom = G::custom(|out| {
        if out.lines().count() > 5 {
            Err(format!("{} lines, max 5", out.lines().count()))
        } else {
            Ok(())
        }
    });
    println!(
        "G::custom 7 lines: {:?}",
        custom.check("1\n2\n3\n4\n5\n6\n7")
    );

    // Composed with | — all must pass; check_all() returns every violation.
    let composite = G::json() | G::length(1, 500) | G::pii();
    println!(
        "\ncomposite (json | length | pii): {} guards",
        composite.len()
    );
    let good = r#"{"analysis": "Revenue grew 15%."}"#;
    assert!(composite.check_all(good).is_empty());
    println!(
        "  good output: {} violations",
        composite.check_all(good).len()
    );
    let bad = "Contact alice@example.com — definitely not JSON.";
    assert!(!composite.check_all(bad).is_empty());
    println!(
        "  bad output:  {} violations",
        composite.check_all(bad).len()
    );

    // A reusable safety guardrail stack.
    let safety = G::pii()
        | G::topic(&["violence", "self-harm", "illegal"])
        | G::length(1, 2000)
        | G::budget(500);
    println!("\nsafety guardrails: {} guards", safety.len());
    println!(
        "  normal: {} violations, risky: {} violations",
        safety.check_all("A helpful response.").len(),
        safety
            .check_all("illegal drugs at user@evil.com for violence")
            .len(),
    );
}
