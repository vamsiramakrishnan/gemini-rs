//! # 03 — Callbacks (Middleware)
//!
//! Demonstrates the M:: middleware composition module for hooking into
//! the agent lifecycle: before/after model calls, before/after tool
//! invocations, event tapping, and operational middleware (retry, circuit
//! breaker, rate limiting).
//!
//! Key concepts:
//! - `M::log()` / `M::latency()` — built-in observability middleware
//! - `M::before_model()` / `M::after_model()` — model call hooks
//! - `M::before_tool()` — tool invocation hooks
//! - `M::tap()` — generic event observer
//! - `M::retry()` / `M::circuit_breaker()` — resilience middleware
//! - `|` operator to compose middleware layers

use gemini_adk_fluent::prelude::*;
use std::time::Duration;

fn main() {
    println!("=== 03: Callbacks (Middleware) ===\n");

    // ── Built-in middleware ──
    // M::log() logs agent events; M::latency() tracks timing.
    let observability = M::log() | M::latency();
    println!("Observability stack: {} layers", observability.len());

    // ── Before/after model hooks ──
    // These fire around every LLM call. Useful for logging, token counting,
    // or injecting custom logic.
    let model_hooks = M::before_model(|_req| {
        println!("  [before_model] About to call the LLM");
        Ok(())
    }) | M::after_model(|_req, _resp| {
        println!("  [after_model] LLM responded");
        Ok(())
    });
    println!("Model hooks: {} layers", model_hooks.len());

    // ── Before tool hooks ──
    // Fires before each tool invocation. Can reject calls by returning Err.
    let tool_hooks = M::before_tool(|call| {
        println!("  [before_tool] Dispatching tool: {}", call.name);
        if call.name == "dangerous_tool" {
            return Err("Blocked: dangerous_tool is not allowed".into());
        }
        Ok(())
    });
    println!("Tool hooks: {} layers", tool_hooks.len());

    // ── Event tapping ──
    // M::tap() receives every AgentEvent — useful for custom telemetry.
    let tap = M::tap(|event| {
        println!("  [tap] Event: {:?}", event);
    });
    println!("Tap middleware: {} layers", tap.len());

    // ── Resilience middleware ──
    // Retry, circuit breaker, rate limiting, and timeout.
    let resilience = M::retry(3)
        | M::circuit_breaker(5)
        | M::rate_limit(10)
        | M::timeout(Duration::from_secs(30));
    println!("Resilience stack: {} layers", resilience.len());

    // ── Full middleware stack ──
    // Compose everything with the | operator into a single stack.
    let full_stack = M::log()
        | M::latency()
        | M::before_model(|_req| {
            println!("  [before_model] request intercepted");
            Ok(())
        })
        | M::after_model(|_req, _resp| {
            println!("  [after_model] response intercepted");
            Ok(())
        })
        | M::before_tool(|call| {
            println!("  [before_tool] tool={}", call.name);
            Ok(())
        })
        | M::retry(3)
        | M::trace()
        | M::audit()
        | M::metrics()
        | M::cost();

    println!("\nFull middleware stack: {} layers", full_stack.len());

    // ── Scoped middleware ──
    // Apply middleware only to specific agents by name.
    let scoped = M::scope(&["researcher", "writer"], M::log() | M::latency());
    println!(
        "Scoped middleware: {} layers (applied to researcher, writer only)",
        scoped.len()
    );

    // ── Agent lifecycle hooks ──
    let lifecycle = M::before_agent(|ctx| {
        println!("  [before_agent] session={:?}", ctx.session_id);
        Ok(())
    }) | M::after_agent(|ctx| {
        println!("  [after_agent] session={:?}", ctx.session_id);
        Ok(())
    });
    println!("Lifecycle hooks: {} layers", lifecycle.len());

    println!("\nDone.");
}
