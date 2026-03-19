//! Walk Example 15: Middleware Stack — M::log | M::latency | M::retry | M::cost | M::circuit_breaker
//!
//! Demonstrates composing middleware layers using the M namespace and `|` operator.
//! Middleware provides cross-cutting concerns like logging, latency tracking,
//! retry logic, cost accounting, and circuit breaking — without modifying agent code.
//!
//! Features used:
//!   - M::log (request/response logging)
//!   - M::latency (timing middleware)
//!   - M::retry (automatic retry on failure)
//!   - M::cost (tool call cost tracking)
//!   - M::circuit_breaker (failure threshold protection)
//!   - M::audit (audit trail recording)
//!   - M::trace (distributed tracing spans)
//!   - M::timeout (duration-based timeouts)
//!   - M::validate (tool input validation)
//!   - M::metrics (request/error counters)
//!   - M::tap (custom event observer)
//!   - M::before_tool / M::on_route / M::on_fallback (lifecycle hooks)
//!   - `|` operator for middleware composition

use std::time::Duration;

use gemini_adk_fluent_rs::prelude::*;

fn main() {
    println!("=== Walk 15: Middleware Stack ===\n");

    // ── Part 1: Individual Middleware Layers ──────────────────────────────
    // Each M:: factory creates a MiddlewareComposite with a single layer.

    println!("--- Part 1: Individual Layers ---");

    let log = M::log();
    println!("  M::log         — {} layer(s)", log.len());

    let latency = M::latency();
    println!("  M::latency     — {} layer(s)", latency.len());

    let retry = M::retry(3);
    println!("  M::retry(3)    — {} layer(s)", retry.len());

    let cost = M::cost();
    println!("  M::cost        — {} layer(s)", cost.len());

    let breaker = M::circuit_breaker(5);
    println!("  M::circuit_breaker(5) — {} layer(s)", breaker.len());

    // ── Part 2: Composing with the `|` Operator ──────────────────────────
    // The `|` operator merges middleware composites into a single stack.

    println!("\n--- Part 2: Composing Middleware ---");

    let production_stack =
        M::log() | M::latency() | M::retry(3) | M::cost() | M::circuit_breaker(5);

    println!("  Production stack: {} layers", production_stack.len());

    // ── Part 3: Observability Stack ──────────────────────────────────────
    // Combine tracing, metrics, and structured logging for production.

    println!("\n--- Part 3: Observability Stack ---");

    let observability = M::trace() | M::metrics() | M::structured_log() | M::latency() | M::audit();

    println!("  Observability stack: {} layers", observability.len());

    // ── Part 4: Safety & Validation Stack ────────────────────────────────
    // Middleware that validates inputs and provides guardrails.

    println!("\n--- Part 4: Safety Stack ---");

    let safety_stack = M::validate(|call| {
        // Reject tool calls with empty arguments
        if call.args.is_null() || call.args.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            Err("Tool call has empty arguments".to_string())
        } else {
            Ok(())
        }
    }) | M::before_tool(|call| {
        // Log every tool invocation
        println!("    [before_tool] Calling: {}", call.name);
        Ok(())
    }) | M::circuit_breaker(3)
        | M::timeout(Duration::from_secs(30));

    println!("  Safety stack: {} layers", safety_stack.len());

    // ── Part 5: Event Hook Middleware ─────────────────────────────────────
    // Custom hooks for specific agent lifecycle events.

    println!("\n--- Part 5: Event Hooks ---");

    let hooks = M::tap(|event| {
        // Observe every agent event
        let _ = event; // In production, log or send to monitoring
    }) | M::on_route(|agent_name| {
        println!("    [on_route] Routed to: {agent_name}");
    }) | M::on_fallback(|agent_name| {
        println!("    [on_fallback] Fell back to: {agent_name}");
    }) | M::on_loop(|iteration| {
        println!("    [on_loop] Iteration: {iteration}");
    }) | M::on_timeout(|| {
        println!("    [on_timeout] Agent timed out!");
    });

    println!("  Event hooks: {} layers", hooks.len());

    // ── Part 6: Full Production Stack ────────────────────────────────────
    // Combine all categories into a comprehensive middleware configuration.

    println!("\n--- Part 6: Full Production Stack ---");

    let full_stack = M::log()
        | M::latency()
        | M::trace()
        | M::metrics()
        | M::audit()
        | M::retry(3)
        | M::cost()
        | M::rate_limit(100)
        | M::circuit_breaker(10)
        | M::timeout(Duration::from_secs(60))
        | M::validate(|_call| Ok(()))
        | M::cache()
        | M::dedup()
        | M::before_agent(|_ctx| {
            // Pre-flight checks
            Ok(())
        })
        | M::after_agent(|_ctx| {
            // Post-processing
            Ok(())
        })
        | M::before_model(|_req| {
            // Request interception
            Ok(())
        })
        | M::after_model(|_req, _resp| {
            // Response interception
            Ok(())
        });

    println!("  Full stack: {} layers", full_stack.len());

    // ── Part 7: Scoped Middleware ─────────────────────────────────────────
    // Apply middleware only to specific agents.

    println!("\n--- Part 7: Scoped Middleware ---");

    let agent_specific = M::scope(
        &["premium_agent", "vip_agent"],
        M::log() | M::latency() | M::cost(),
    );

    println!(
        "  Scoped to [premium_agent, vip_agent]: {} layers",
        agent_specific.len()
    );

    // ── Part 8: Development vs Production Configs ────────────────────────

    println!("\n--- Part 8: Environment-Based Configs ---");

    let is_production = false; // Toggle for demo

    let stack = if is_production {
        // Production: full observability + safety
        M::trace()
            | M::metrics()
            | M::audit()
            | M::retry(3)
            | M::circuit_breaker(10)
            | M::rate_limit(1000)
            | M::timeout(Duration::from_secs(30))
    } else {
        // Development: verbose logging + no rate limits
        M::log()
            | M::latency()
            | M::tap(|_event| {
                // Verbose debug output
            })
            | M::cost()
            | M::timeout(Duration::from_secs(120))
            // Pad to same layer count for the demo
            | M::cache()
            | M::metrics()
    };

    println!(
        "  {} stack: {} layers",
        if is_production {
            "Production"
        } else {
            "Development"
        },
        stack.len()
    );

    // ── Part 9: Fallback Model Middleware ─────────────────────────────────

    println!("\n--- Part 9: Fallback Model ---");

    let resilient = M::fallback_model("gemini-1.5-flash") | M::retry(2) | M::circuit_breaker(5);

    println!(
        "  Resilient stack with model fallback: {} layers",
        resilient.len()
    );

    println!("\nDone.");
}
