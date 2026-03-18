//! Walk Example 16: Context Engineering — C::window, C::user_only, C::from_state, C::budget, C::redact
//!
//! Demonstrates the C namespace for controlling what conversation history
//! (context) an agent sees. Context engineering is critical for managing
//! token budgets, filtering irrelevant messages, injecting state, and
//! protecting sensitive information.
//!
//! Features used:
//!   - C::window (sliding window of recent messages)
//!   - C::user_only / C::model_only (role-based filtering)
//!   - C::from_state (inject state values as context)
//!   - C::budget (token-aware truncation)
//!   - C::redact (PII/sensitive data masking)
//!   - C::exclude_tools (strip tool call noise)
//!   - C::head / C::sample / C::truncate (various selection strategies)
//!   - C::dedup (deduplicate adjacent messages)
//!   - C::prepend / C::append (inject system context)
//!   - C::summarize / C::distill / C::relevant (LLM-powered markers)
//!   - `+` operator for policy composition

use gemini_adk_fluent::prelude::*;

fn main() {
    println!("=== Walk 16: Context Engineering ===\n");

    // ── Build sample conversation history ────────────────────────────────

    let history = vec![
        Content::user("My name is Alice and my email is alice@example.com"),
        Content::model("Hello Alice! How can I help you today?"),
        Content::user("I need help with my account, ID: ACC-12345"),
        Content::model("I can see your account. What do you need?"),
        Content::user("What is my current balance?"),
        Content::model("Your balance is $1,234.56"),
        Content::user("Can you transfer $500 to bob@example.com?"),
        Content::model("I will process that transfer for you."),
        Content::user("Actually, make it $600 instead"),
        Content::model("Updated: transferring $600 to bob@example.com"),
    ];

    println!("Full history: {} messages\n", history.len());

    // ── C::window — Sliding Window ───────────────────────────────────────

    println!("--- C::window(4) — Last 4 Messages ---");
    let windowed = C::window(4).apply(&history);
    println!(
        "  Kept {} messages (from {} total)",
        windowed.len(),
        history.len()
    );
    for msg in &windowed {
        print_content(msg);
    }

    // ── C::user_only — Keep Only User Messages ───────────────────────────

    println!("\n--- C::user_only() ---");
    let user_msgs = C::user_only().apply(&history);
    println!("  {} user messages:", user_msgs.len());
    for msg in &user_msgs {
        print_content(msg);
    }

    // ── C::model_only — Keep Only Model Messages ─────────────────────────

    println!("\n--- C::model_only() ---");
    let model_msgs = C::model_only().apply(&history);
    println!("  {} model messages", model_msgs.len());

    // ── C::from_state — Inject State as Context ──────────────────────────

    println!("\n--- C::from_state() ---");
    let with_state =
        C::from_state(&["user:name", "app:account_balance", "derived:risk"]).apply(&history);
    println!("  Total messages after injection: {}", with_state.len());
    print_content(&with_state[0]); // The injected context message

    // ── C::budget — Token-Aware Truncation ───────────────────────────────

    println!("\n--- C::budget(50) — ~50 Token Budget ---");
    let budgeted = C::budget(50).apply(&history);
    println!("  Kept {} messages within ~50 token budget", budgeted.len());

    // ── C::redact — Mask Sensitive Data ──────────────────────────────────

    println!("\n--- C::redact() — PII Masking ---");
    let redacted =
        C::redact(&["alice@example.com", "bob@example.com", "ACC-12345"]).apply(&history);
    println!("  Redacted {} messages:", redacted.len());
    for msg in &redacted {
        print_content(msg);
    }

    // ── C::head — Keep First N Messages ──────────────────────────────────

    println!("\n--- C::head(3) — First 3 Messages ---");
    let head = C::head(3).apply(&history);
    println!("  Kept {} messages from the start", head.len());

    // ── C::sample — Every Nth Message ────────────────────────────────────

    println!("\n--- C::sample(3) — Every 3rd Message ---");
    let sampled = C::sample(3).apply(&history);
    println!("  Sampled {} messages (every 3rd)", sampled.len());

    // ── C::dedup — Remove Adjacent Duplicates ────────────────────────────

    println!("\n--- C::dedup() ---");
    let with_dupes = vec![
        Content::user("hello"),
        Content::user("hello"),
        Content::model("hi"),
        Content::model("hi"),
        Content::model("hi"),
        Content::user("bye"),
    ];
    let deduped = C::dedup().apply(&with_dupes);
    println!(
        "  {} messages -> {} after dedup",
        with_dupes.len(),
        deduped.len()
    );

    // ── C::prepend / C::append — Inject System Context ───────────────────

    println!("\n--- C::prepend() + C::append() ---");
    let short_history = vec![Content::user("What is 2+2?")];
    let enriched = C::prepend(Content::model("You are a math tutor.")).apply(&short_history);
    let enriched = C::append(Content::user("[End of conversation]")).apply(&enriched);
    println!("  {} messages after prepend + append", enriched.len());

    // ── C::empty — Isolated Agent ────────────────────────────────────────

    println!("\n--- C::empty() — Isolated Agent ---");
    let empty = C::empty().apply(&history);
    println!("  {} messages (agent sees no history)", empty.len());

    // ── C::exclude_tools — Strip Tool Noise ──────────────────────────────

    println!("\n--- C::exclude_tools() ---");
    let clean = C::exclude_tools().apply(&history);
    println!("  {} messages after removing tool calls", clean.len());

    // ── Composing Policies with `+` ──────────────────────────────────────
    // The `+` operator combines multiple policies.

    println!("\n--- Policy Composition with + ---");

    let production_policy = C::window(10) + C::user_only() + C::exclude_tools();
    println!(
        "  Composed policy: {} individual policies",
        production_policy.policies.len()
    );

    // Apply the composed policy (each policy runs independently)
    let result = production_policy.apply(&history);
    println!(
        "  Result: {} messages total from all policies",
        result.len()
    );

    // ── Advanced: LLM-Powered Context Markers ────────────────────────────
    // These policies prepend markers that the runtime interprets.

    println!("\n--- Advanced: LLM-Powered Markers ---");

    let summarized = C::summarize("Focus on action items and decisions").apply(&history);
    print_content(&summarized[0]);

    let distilled = C::distill("Keep only financial transactions").apply(&history);
    print_content(&distilled[0]);

    let relevant = C::relevant("user:current_topic").apply(&history);
    print_content(&relevant[0]);

    // ── C::fit — Smart Truncation with Marker ────────────────────────────

    println!("\n--- C::fit(20) — Smart Token Fit ---");
    let fitted = C::fit(20).apply(&history);
    println!("  {} messages within ~20 token budget", fitted.len());
    // Check if truncation marker was added
    for msg in &fitted {
        let text = extract_text(msg);
        if text.contains("truncated") {
            println!("  Truncation marker present: {text}");
        }
    }

    // ── C::from_agents / C::exclude_agents ───────────────────────────────

    println!("\n--- Agent-Attributed Context ---");
    let agent_history = vec![
        Content::user("[Agent: researcher] Found 3 relevant papers"),
        Content::user("[Agent: logger] Debug: query took 42ms"),
        Content::user("[Agent: researcher] Key finding: ownership prevents data races"),
    ];

    let research_only = C::from_agents(&["researcher"]).apply(&agent_history);
    println!("  From 'researcher' only: {} messages", research_only.len());

    let no_logger = C::exclude_agents(&["logger"]).apply(&agent_history);
    println!("  Excluding 'logger': {} messages", no_logger.len());

    // ── C::notes — Scratchpad ────────────────────────────────────────────

    println!("\n--- C::notes() — Scratchpad ---");
    let with_notes = C::notes("session:scratchpad").apply(&history);
    print_content(&with_notes[0]);

    println!("\nDone.");
}

/// Helper to print a Content message's text parts.
fn print_content(content: &Content) {
    let role = match content.role {
        Some(Role::User) => "USER ",
        Some(Role::Model) => "MODEL",
        _ => "?????",
    };
    let text = extract_text(content);
    let truncated = if text.len() > 80 {
        format!("{}...", &text[..77])
    } else {
        text
    };
    println!("    [{role}] {truncated}");
}

/// Extract text from a Content message.
fn extract_text(content: &Content) -> String {
    content
        .parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}
