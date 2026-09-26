//! A fluent `Live` session tested offline against a scripted server: the
//! real runtime (tools, governance, callbacks) runs; only the model is
//! replaced by a script.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gemini_adk_fluent_rs::compose::tools::ToolComposite;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::testing::ScriptedServer;
use gemini_adk_rs::flow::{Flow, Guard};
use serde_json::json;

fn counting_tool(name: &'static str, calls: Arc<AtomicUsize>) -> ToolComposite {
    T::simple(name, name, move |_| {
        let calls = calls.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        }
    })
}

/// Governance blocks a payment the flow has not reached, then allows it once
/// identity is verified. Proven offline, with the real flow monitor.
#[tokio::test]
async fn governance_blocks_an_out_of_order_tool_offline() {
    let verified = Arc::new(AtomicUsize::new(0));
    let paid = Arc::new(AtomicUsize::new(0));
    let flow = Flow::new()
        .step("verify")
        .allow(["verify_identity"])
        .done(Guard::called_ok("verify_identity"))
        .step("pay")
        .after("verify")
        .allow(["take_payment"])
        .done(Guard::called_ok("take_payment"))
        .build()
        .unwrap();

    let run = ScriptedServer::new()
        .calls("take_payment", json!({ "amount": 10 }))
        .calls("verify_identity", json!({}))
        .calls("take_payment", json!({ "amount": 10 }))
        .says("Payment taken.")
        .play(
            Live::builder()
                .tools(counting_tool("verify_identity", verified.clone()))
                .tools(counting_tool("take_payment", paid.clone()))
                .govern(flow),
        )
        .await
        .unwrap();

    assert_eq!(verified.load(Ordering::SeqCst), 1);
    assert_eq!(
        paid.load(Ordering::SeqCst),
        1,
        "the early payment was blocked; only the one after verification ran"
    );
    let responses = run.tool_responses();
    assert_eq!(responses.len(), 3, "every call is answered: {responses:#?}");
    assert_eq!(responses[0]["id"], "call-1");
    assert_ne!(
        responses[0]["response"],
        json!({ "ok": true }),
        "the blocked call is answered with a refusal, not the tool's result"
    );
    assert_eq!(responses[2]["response"], json!({ "ok": true }));
    assert!(run.transcript_text().contains("Payment taken."));
    run.disconnect().await;
}

/// The setup the session opens with is the one the builder configured.
#[tokio::test]
async fn the_setup_message_reflects_the_builder() {
    let run = ScriptedServer::new()
        .says("Hello.")
        .play(
            Live::builder()
                .model(ModelId::LIVE_3_8)
                .instruction("Be brief."),
        )
        .await
        .unwrap();
    let setup = run.setup().expect("a setup message was sent");
    assert_eq!(setup["model"], "models/gemini-3.8-live");
    assert_eq!(setup["systemInstruction"]["parts"][0]["text"], "Be brief.");
    run.disconnect().await;
}

/// A tool reads the session it runs in: the account captured earlier, and
/// the model's call id.
#[tokio::test]
async fn a_tool_gets_the_session_context() {
    let state = State::new();
    state.set("account_id", "A-17").unwrap();
    let run = ScriptedServer::new()
        .calls("balance", json!({}))
        .says("Your balance is twelve dollars.")
        .play(Live::builder().state(state).tools(T::contextual(
            "balance",
            "The caller's balance",
            |_args, ctx| async move {
                let account: String = ctx.state.get("account_id").unwrap_or_default();
                Ok(json!({ "account": account, "call": ctx.call_id }))
            },
        )))
        .await
        .unwrap();
    assert_eq!(
        run.tool_responses()[0]["response"],
        json!({ "account": "A-17", "call": "call-1" })
    );
    run.disconnect().await;
}
