//! Conversation policies enforced in a running (scripted, offline) session.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gemini_adk_fluent_rs::policy::Policy;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::testing::ScriptedServer;
use serde_json::json;

/// The model retries a charge (a timeout, a repeated request): the card is
/// charged once, and the retry gets the first charge's result back.
#[tokio::test]
async fn a_retried_commit_charges_once() {
    let charges = Arc::new(AtomicUsize::new(0));
    let counted = charges.clone();
    let state = State::new();
    let _ = state.set("user_id", "u1");
    let run = ScriptedServer::new()
        .calls("charge_card", json!({ "amount": 40 }))
        .calls("charge_card", json!({ "amount": 40 }))
        .says("You're all set.")
        .play(
            Live::builder()
                .state(state)
                .tools(T::simple("charge_card", "Charge the card", move |args| {
                    let counted = counted.clone();
                    async move {
                        let n = counted.fetch_add(1, Ordering::SeqCst) + 1;
                        Ok(json!({ "charge_id": format!("ch_{n}"), "amount": args["amount"] }))
                    }
                }))
                .policy(Policy::commit("charge_card").idempotency_key("{user_id}:{amount}")),
        )
        .await
        .unwrap();

    assert_eq!(charges.load(Ordering::SeqCst), 1, "charged once");
    let responses = run.tool_responses();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["response"], responses[1]["response"]);
    assert_eq!(responses[1]["response"]["charge_id"], "ch_1");
    run.disconnect().await;
}

/// A commit policy naming a tool that does not exist fails at connect.
#[tokio::test]
async fn a_commit_policy_for_a_missing_tool_is_refused() {
    let err = ScriptedServer::new()
        .play(Live::builder().policy(Policy::commit("charge_card")))
        .await
        .err()
        .expect("connect must fail");
    assert!(err.to_string().contains("charge_card"), "{err}");
}
