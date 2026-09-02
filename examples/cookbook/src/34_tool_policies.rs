//! # 34 — Tool Policies: Timeout, Cache, and Confirmation
//!
//! Per-tool execution policies let you attach safety and performance
//! enforcement directly to a tool rather than scattering that logic
//! throughout your application code.
//!
//! Key concepts:
//! - `T::timeout(tool, duration)` — race the tool future against a deadline;
//!   on elapse return `ToolError::Timeout` and drop the inner future
//! - `T::cached(tool)` — memoize successful results keyed by
//!   `(tool name, canonical-JSON args)`; identical args return the cached
//!   value without re-invoking the tool; errors are never cached
//! - `T::confirm(tool, message)` — mark the tool as requiring user
//!   confirmation; the flag travels to the runtime via
//!   `PolicyTool::requires_confirmation()` and is never silently dropped
//! - Policies nest: `T::cached(T::timeout(tool, dur))` applies both
//! - At the lower level: `PolicyTool::new(arc_fn, ToolPolicy::new().with_cache())`
//!   is the direct API; `T::*` wrappers build on top of it
//! - Register the wrapped tool in a `ToolDispatcher` exactly like any other
//!   `ToolFunction`
//!
//! Runs without credentials — all logic is exercised locally.

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};
use std::time::Duration;

use gemini_adk_fluent_rs::compose::tools::ToolCompositeEntry;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_rs::tool::{PolicyTool, ToolPolicy};
use serde_json::json;

#[tokio::main]
async fn main() {
    println!("=== 34: Tool Policies \u{2014} Timeout, Cache, and Confirmation ===\n");

    // ── 1. Caching ──────────────────────────────────────────────────────────
    //
    // T::cached() memoizes successful results by (name, canonical args).
    // The underlying tool is only invoked once per unique arg set.

    println!("--- 1. Caching ---\n");

    let call_count = Arc::new(AtomicU32::new(0));
    let cc = call_count.clone();

    let cached_composite = T::cached(T::simple(
        "lookup_rate",
        "Look up the exchange rate for a currency pair",
        move |args| {
            let cc = cc.clone();
            async move {
                let n = cc.fetch_add(1, Ordering::SeqCst) + 1;
                let pair = args["pair"].as_str().unwrap_or("USD/EUR");
                Ok(json!({ "pair": pair, "rate": 1.08, "invocation": n }))
            }
        },
    ));

    // Extract the underlying ToolFunction so we can call it directly.
    let lookup_fn = match &cached_composite.entries[0] {
        ToolCompositeEntry::Function(f) => f.clone(),
        _ => panic!("expected Function entry"),
    };

    let first: serde_json::Value = lookup_fn
        .call(json!({"pair": "USD/EUR"}))
        .await
        .expect("first call should succeed");
    println!("  First call  (USD/EUR): {first}");

    let second: serde_json::Value = lookup_fn
        .call(json!({"pair": "USD/EUR"}))
        .await
        .expect("second call should succeed (cache hit)");
    println!("  Second call (USD/EUR): {second}");

    assert_eq!(first, second, "cache hit must return identical value");
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "inner tool invoked only once"
    );
    println!(
        "  \u{2713} Cache hit confirmed \u{2014} inner tool invoked {} time(s)",
        call_count.load(Ordering::SeqCst)
    );

    // Different args -> cache miss.
    let third: serde_json::Value = lookup_fn
        .call(json!({"pair": "USD/GBP"}))
        .await
        .expect("third call should succeed (cache miss)");
    println!("  Third call  (USD/GBP): {third}");
    assert_eq!(call_count.load(Ordering::SeqCst), 2);
    println!(
        "  \u{2713} Cache miss confirmed \u{2014} inner tool invoked {} time(s) total\n",
        call_count.load(Ordering::SeqCst)
    );

    // ── 2. Timeout ──────────────────────────────────────────────────────────
    //
    // T::timeout() races the tool future against a duration.
    // On elapse: ToolError::Timeout(duration) is returned; the future is dropped.
    // Under the deadline: the result passes through unmodified.

    println!("--- 2. Timeout ---\n");

    let slow_composite = T::timeout(
        T::simple(
            "slow_search",
            "Simulates a slow external API call",
            |_args| async move {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(json!({"results": []}))
            },
        ),
        Duration::from_millis(50),
    );

    let slow_fn = match &slow_composite.entries[0] {
        ToolCompositeEntry::Function(f) => f.clone(),
        _ => panic!("expected Function entry"),
    };

    match slow_fn.call(json!({"query": "rust async"})).await {
        Err(ToolError::Timeout(d)) => {
            println!("  \u{2713} Timed out after {d:?} as expected");
        }
        other => panic!("expected Timeout error, got {other:?}"),
    }

    // A fast tool with a generous deadline succeeds normally.
    let fast_composite = T::timeout(
        T::simple("fast_tool", "Returns immediately", |args| async move {
            Ok(json!({"echo": args}))
        }),
        Duration::from_secs(5),
    );

    let fast_fn = match &fast_composite.entries[0] {
        ToolCompositeEntry::Function(f) => f.clone(),
        _ => panic!("expected Function entry"),
    };

    let result: serde_json::Value = fast_fn
        .call(json!({"input": "hello"}))
        .await
        .expect("fast tool should succeed within deadline");
    println!("  \u{2713} Fast tool succeeded: {result}\n");

    // ── 3. Confirmation ─────────────────────────────────────────────────────
    //
    // T::confirm() records a confirmation requirement on the tool's ToolPolicy.
    // At L1 this is PolicyTool::new(fn, ToolPolicy::new().with_confirm(msg)).
    // The flag is surfaced via PolicyTool::requires_confirmation() so the
    // session runtime can prompt the user before executing the tool.

    println!("--- 3. Confirmation ---\n");

    // Build directly with PolicyTool + ToolPolicy to see the low-level API.
    let inner: Arc<dyn ToolFunction> = Arc::new(SimpleTool::new(
        "delete_records",
        "Delete all matching records from the database",
        None,
        |args| async move {
            let table = args["table"].as_str().unwrap_or("unknown");
            Ok(json!({"deleted": 42, "table": table}))
        },
    ));
    let policy = ToolPolicy::new().with_confirm(Some(
        "This will permanently delete records. Are you sure?".to_string(),
    ));
    let policy_tool = PolicyTool::new(inner.clone(), policy);

    assert!(policy_tool.requires_confirmation());
    println!(
        "  \u{2713} requires_confirmation = {}",
        policy_tool.requires_confirmation()
    );
    println!(
        "  \u{2713} confirm_message       = {:?}",
        policy_tool.policy().confirm_message
    );

    // The same result via the T:: fluent API (T::confirm wraps in PolicyTool internally).
    let confirm_composite = T::confirm(
        T::simple(
            "delete_records_t",
            "Delete records (via T::)",
            |args| async move {
                let table = args["table"].as_str().unwrap_or("unknown");
                Ok(json!({"deleted": 42, "table": table}))
            },
        ),
        "Confirm before deleting",
    );
    println!(
        "  T::confirm composite entry count: {}",
        confirm_composite.len()
    );
    println!("  (The session runtime checks requires_confirmation() before invoking)\n");

    // ── 4. Nested policies ───────────────────────────────────────────────────
    //
    // Policies compose: T::cached(T::timeout(tool, dur)) applies both.
    // The cache intercepts before the timeout fires, so cache hits bypass
    // the timeout entirely.

    println!("--- 4. Nested policies: cache + timeout ---\n");

    let nested_count = Arc::new(AtomicU32::new(0));
    let nc = nested_count.clone();

    let nested = T::cached(T::timeout(
        T::simple(
            "priced_lookup",
            "Expensive lookup with a deadline",
            move |args| {
                let nc = nc.clone();
                async move {
                    nc.fetch_add(1, Ordering::SeqCst);
                    let symbol = args["symbol"].as_str().unwrap_or("?");
                    Ok(json!({"symbol": symbol, "price": 99.50}))
                }
            },
        ),
        Duration::from_secs(5),
    ));

    let nested_fn = match &nested.entries[0] {
        ToolCompositeEntry::Function(f) => f.clone(),
        _ => panic!("expected Function entry"),
    };

    let r1: serde_json::Value = nested_fn
        .call(json!({"symbol": "GOOG"}))
        .await
        .expect("first call");
    let r2: serde_json::Value = nested_fn
        .call(json!({"symbol": "GOOG"}))
        .await
        .expect("second call (cache hit)");
    assert_eq!(r1, r2);
    assert_eq!(nested_count.load(Ordering::SeqCst), 1, "inner invoked once");
    println!("  \u{2713} nested cache+timeout: inner invoked once, second call from cache");
    println!("    r1 = {r1}");
    println!("    r2 = {r2} (cache hit)\n");

    // ── 5. Register policies in a ToolDispatcher ────────────────────────────
    //
    // Policy-wrapped tools are plain ToolFunction implementations.
    // Register them with dispatcher.register_function() like any other tool.

    println!("--- 5. Register in ToolDispatcher ---\n");

    let counter = Arc::new(AtomicU32::new(0));
    let c = counter.clone();

    let mut dispatcher = ToolDispatcher::new();

    // Build a cached + timeout-guarded tool via the T:: API.
    let composite = T::cached(T::timeout(
        T::simple(
            "get_stock_price",
            "Get the current stock price",
            move |args| {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    let ticker = args["ticker"].as_str().unwrap_or("?");
                    Ok(json!({"ticker": ticker, "price": 150.25}))
                }
            },
        ),
        Duration::from_secs(10),
    ));

    // Extract and register the policy-wrapped function.
    if let ToolCompositeEntry::Function(f) = composite.entries.into_iter().next().unwrap() {
        dispatcher.register_function(f);
    }

    // Also register a plain tool for comparison.
    dispatcher.register(SimpleTool::new(
        "get_company_name",
        "Look up company name from ticker",
        None,
        |args| async move {
            let ticker = args["ticker"].as_str().unwrap_or("?");
            Ok(json!({"ticker": ticker, "name": format!("{ticker} Corporation")}))
        },
    ));

    println!("  Registered {} tools in dispatcher.", dispatcher.len());

    let price1 = dispatcher
        .call_function("get_stock_price", json!({"ticker": "RUST"}))
        .await
        .expect("first dispatch");
    let price2 = dispatcher
        .call_function("get_stock_price", json!({"ticker": "RUST"}))
        .await
        .expect("second dispatch (cache hit)");
    assert_eq!(price1, price2);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    println!("  \u{2713} dispatcher caching works: price = {price1}");
    println!(
        "    inner invoked {} time(s) for identical args",
        counter.load(Ordering::SeqCst)
    );

    let name = dispatcher
        .call_function("get_company_name", json!({"ticker": "RUST"}))
        .await
        .expect("name lookup");
    println!("  \u{2713} plain tool dispatch: {name}");

    // ── Summary ─────────────────────────────────────────────────────────────
    println!("\n--- Summary ---\n");
    println!("  T::cached(tool)              \u{2192} memoize by (name, canonical args)");
    println!("  T::timeout(tool, duration)   \u{2192} ToolError::Timeout on elapse");
    println!("  T::confirm(tool, msg)        \u{2192} PolicyTool::requires_confirmation() == true");
    println!("  T::cached(T::timeout(...))   \u{2192} both policies active (policies nest)");
    println!(
        "  ToolPolicy::new()            \u{2192} low-level builder: .with_cache() / .with_timeout() / .with_confirm()"
    );
    println!(
        "  PolicyTool::new(fn, policy)  \u{2192} direct decorator; T::* wrappers call this internally"
    );
    println!("\n=== Done ===");
}
