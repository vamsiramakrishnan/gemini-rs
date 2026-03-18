//! Walk Example 11: RouteTextAgent — Conditional Agent Selection
//!
//! Demonstrates using RouteTextAgent to branch to different agents based on
//! state predicates. This is useful for building routers that dispatch to
//! specialized agents depending on user intent or conversation context.
//!
//! Features used:
//!   - FnTextAgent (zero-cost state-driven agents)
//!   - RouteTextAgent (deterministic state-driven routing)
//!   - RouteRule (predicate + target agent pairs)
//!   - State (shared key-value store)
//!   - S::is_true / S::eq (state predicates)

use std::sync::Arc;

use adk_rs_fluent::prelude::*;

#[tokio::main]
async fn main() {
    println!("=== Walk 11: RouteTextAgent — Conditional Agent Selection ===\n");

    // ── Build specialized agents ──────────────────────────────────────────
    // Each agent handles a different category of support request.
    // FnTextAgent runs a closure without an LLM call — perfect for demos.

    let billing_agent: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("billing", |state| {
        let account = state
            .get::<String>("account_id")
            .unwrap_or_else(|| "unknown".into());
        Ok(format!(
            "[Billing] Processing billing inquiry for account {account}. \
             Your current balance is $42.00."
        ))
    }));

    let technical_agent: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("technical", |state| {
        let issue = state
            .get::<String>("issue_description")
            .unwrap_or_else(|| "unspecified".into());
        Ok(format!(
            "[Technical] Troubleshooting: {issue}. \
             Have you tried restarting the device?"
        ))
    }));

    let sales_agent: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("sales", |state| {
        let product = state
            .get::<String>("product_interest")
            .unwrap_or_else(|| "general".into());
        Ok(format!(
            "[Sales] Great choice! The {product} plan includes unlimited \
             features. Would you like to upgrade?"
        ))
    }));

    let default_agent: Arc<dyn TextAgent> = Arc::new(FnTextAgent::new("general", |_state| {
        Ok("[General] Welcome! How can I help you today? \
             I can assist with billing, technical issues, or sales."
            .to_string())
    }));

    // ── Define routing rules ──────────────────────────────────────────────
    // Rules are evaluated in order; the first matching predicate wins.
    // If none match, the default agent handles the request.

    let router = RouteTextAgent::new(
        "support_router",
        vec![
            // Route to billing if department == "billing"
            RouteRule::new(
                |state| {
                    state
                        .get::<String>("department")
                        .map(|d| d == "billing")
                        .unwrap_or(false)
                },
                billing_agent.clone(),
            ),
            // Route to technical if department == "technical"
            RouteRule::new(
                |state| {
                    state
                        .get::<String>("department")
                        .map(|d| d == "technical")
                        .unwrap_or(false)
                },
                technical_agent.clone(),
            ),
            // Route to sales if the user expressed purchase intent
            RouteRule::new(
                |state| state.get::<bool>("wants_to_buy").unwrap_or(false),
                sales_agent.clone(),
            ),
        ],
        default_agent.clone(),
    );

    // ── Scenario 1: Billing request ──────────────────────────────────────

    println!("--- Scenario 1: Billing Request ---");
    let state = State::new();
    state.set("department", "billing");
    state.set("account_id", "ACC-12345");

    let result = router.run(&state).await.unwrap();
    println!("Router selected: {}\n", result);

    // ── Scenario 2: Technical support ────────────────────────────────────

    println!("--- Scenario 2: Technical Support ---");
    let state = State::new();
    state.set("department", "technical");
    state.set("issue_description", "Wi-Fi keeps disconnecting");

    let result = router.run(&state).await.unwrap();
    println!("Router selected: {}\n", result);

    // ── Scenario 3: Sales intent (no department set) ─────────────────────

    println!("--- Scenario 3: Sales Intent ---");
    let state = State::new();
    state.set("wants_to_buy", true);
    state.set("product_interest", "Enterprise Pro");

    let result = router.run(&state).await.unwrap();
    println!("Router selected: {}\n", result);

    // ── Scenario 4: No matching route → default ──────────────────────────

    println!("--- Scenario 4: No Match → Default ---");
    let state = State::new();

    let result = router.run(&state).await.unwrap();
    println!("Router selected: {}\n", result);

    // ── Demonstrate S:: state predicates ─────────────────────────────────
    // S::is_true, S::eq, S::one_of provide ergonomic predicate construction.

    println!("--- S:: Predicate Helpers ---");
    let predicate_state = State::new();
    predicate_state.set("vip", true);
    predicate_state.set("tier", "gold");
    predicate_state.set("intent", "upgrade");

    let is_vip = S::is_true("vip");
    let is_gold = S::eq("tier", "gold");
    let wants_change = S::one_of("intent", &["upgrade", "downgrade", "cancel"]);

    println!("  is_vip:       {}", is_vip(&predicate_state));
    println!("  is_gold:      {}", is_gold(&predicate_state));
    println!("  wants_change: {}", wants_change(&predicate_state));

    println!("\nDone.");
}
