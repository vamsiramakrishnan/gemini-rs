//! Session lifecycle through the fluent API, against a scripted server and
//! no network: GoAway, session resumption and reconnect, barge-in during a
//! tool call, and cancellation of in-flight tool calls.
//!
//! Each test plays a script through the real router, lanes and control
//! plane (`ScriptedServer`), so what is asserted is what an application
//! observes: its callbacks, the `LiveEvent`s, and the frames it sent.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use gemini_adk_fluent_rs::live::LiveEvent;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::testing::ScriptedServer;
use serde_json::{Value, json};
use tokio::sync::Notify;

fn go_away(time_left: &str) -> Value {
    json!({ "goAway": { "timeLeft": time_left } })
}

fn resumption(handle: &str) -> Value {
    json!({ "sessionResumptionUpdate": { "newHandle": handle, "resumable": true } })
}

fn cancellation(ids: &[&str]) -> Value {
    json!({ "toolCallCancellation": { "ids": ids } })
}

/// A tool that waits until told to finish, counting how often it completed.
fn gated_tool(
    name: &'static str,
    release: Arc<Notify>,
    completed: Arc<AtomicUsize>,
) -> impl Into<gemini_adk_fluent_rs::compose::tools::ToolComposite> {
    T::simple(name, "waits to be released", move |_args| {
        let release = release.clone();
        let completed = completed.clone();
        async move {
            release.notified().await;
            completed.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "done": true }))
        }
    })
}

#[tokio::test]
async fn go_away_reaches_the_callback_and_the_event_stream_with_its_deadline() {
    let seen = Arc::new(parking_lot::Mutex::new(Vec::<Duration>::new()));
    let seen_cb = seen.clone();
    let run = ScriptedServer::new()
        .frame(go_away("2.5s"))
        .says("Still here.")
        .play(Live::builder().instruction("test").on_go_away(move |left| {
            let seen = seen_cb.clone();
            async move {
                seen.lock().push(left);
            }
        }))
        .await
        .expect("script plays");

    assert_eq!(seen.lock().as_slice(), &[Duration::from_millis(2500)]);
    let events: Vec<_> = run
        .events()
        .iter()
        .filter_map(|e| match e {
            LiveEvent::GoAway { time_left } => Some(*time_left),
            _ => None,
        })
        .collect();
    assert_eq!(events, vec![Duration::from_millis(2500)]);
    // Today the wire layer leaves the connection on a GoAway and reconnects
    // with the latest resumption handle at once, so what the server still
    // sends on the old connection (here "Still here.") is not delivered; a
    // replay transport cannot reconnect, so the session ends. Whether to
    // drain the old connection until the server closes it is decision D1
    // in docs/cleanup/decisions.md; this assertion pins the current
    // behaviour so a change to it is a deliberate one.
    assert!(
        !run.events()
            .iter()
            .any(|e| matches!(e, LiveEvent::TextDelta(t) if t.contains("Still here"))),
        "text after a GoAway was delivered: the reconnect-at-once behaviour changed (see D1)"
    );
    assert!(
        run.events()
            .iter()
            .any(|e| matches!(e, LiveEvent::Disconnected { .. })),
        "the session did not end after the GoAway reconnect failed: {:?}",
        run.events()
    );
    run.disconnect().await;
}

#[tokio::test]
async fn an_unparsable_go_away_deadline_is_still_delivered() {
    let seen = Arc::new(AtomicUsize::new(0));
    let seen_cb = seen.clone();
    let run = ScriptedServer::new()
        .frame(json!({ "goAway": { "timeLeft": "soon" } }))
        .says("ok")
        .play(Live::builder().instruction("test").on_go_away(move |_| {
            let seen = seen_cb.clone();
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
            }
        }))
        .await
        .expect("script plays");
    assert_eq!(
        seen.load(Ordering::SeqCst),
        1,
        "the GoAway itself is never lost"
    );
    run.disconnect().await;
}

#[tokio::test]
async fn the_latest_resumption_handle_is_kept_and_sent_on_reconnect() {
    let run = ScriptedServer::new()
        .frame(resumption("handle-1"))
        .says("first")
        .frame(resumption("handle-2"))
        .says("second")
        .play(Live::builder().instruction("test").session_resume())
        .await
        .expect("script plays");
    assert_eq!(run.handle().resume_handle().as_deref(), Some("handle-2"));
    let handle = run.handle().resume_handle().expect("a handle");
    run.disconnect().await;

    // Reconnect with the handle the server last gave: the new session's
    // setup frame carries it, which is what makes the server resume.
    let resumed = ScriptedServer::new()
        .says("welcome back")
        .play(
            Live::builder()
                .instruction("test")
                .session_resume_from(handle),
        )
        .await
        .expect("script plays");
    let setup = resumed.setup().expect("a setup frame was sent");
    assert_eq!(
        setup.pointer("/sessionResumption/handle"),
        Some(&json!("handle-2")),
        "setup did not carry the resumption handle: {setup}"
    );
    resumed.disconnect().await;
}

#[tokio::test]
async fn the_resumption_handle_survives_the_server_closing_after_go_away() {
    // The server warns, then closes: the handle stays readable so the
    // application can reconnect with it.
    let disconnected = Arc::new(AtomicUsize::new(0));
    let disconnected_cb = disconnected.clone();
    let run = ScriptedServer::new()
        .frame(resumption("handle-9"))
        .frame(go_away("1s"))
        .play(
            Live::builder()
                .instruction("test")
                .session_resume()
                .on_disconnected(move |_reason| {
                    let disconnected = disconnected_cb.clone();
                    async move {
                        disconnected.fetch_add(1, Ordering::SeqCst);
                    }
                }),
        )
        .await
        .expect("script plays");
    // The script ran out: the transport reports the close.
    run.handle().done().await.ok();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(run.handle().resume_handle().as_deref(), Some("handle-9"));
    // The close reaches the application on both paths: the event stream
    // and the `on_disconnected` callback.
    assert!(
        run.events()
            .iter()
            .any(|e| matches!(e, LiveEvent::Disconnected { .. })),
        "the close after GoAway is not in the event stream: {:?}",
        run.events()
    );
    assert_eq!(
        disconnected.load(Ordering::SeqCst),
        1,
        "the close after GoAway did not reach on_disconnected exactly once"
    );
}

#[tokio::test]
async fn a_barge_in_during_an_inline_tool_cancels_it_without_a_response() {
    let release = Arc::new(Notify::new());
    let completed = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new(parking_lot::Mutex::new(Vec::<Vec<String>>::new()));
    let cancelled_cb = cancelled.clone();
    let run = ScriptedServer::new()
        .calls("wait", json!({}))
        .interrupts()
        .says("Sorry, go on.")
        .settle_after(Duration::from_millis(300))
        .play(
            Live::builder()
                .instruction("test")
                .tools(gated_tool("wait", release.clone(), completed.clone()))
                .on_tool_cancelled(move |ids| {
                    let cancelled = cancelled_cb.clone();
                    async move {
                        cancelled.lock().push(ids);
                    }
                }),
        )
        .await
        .expect("script plays");

    // The interruption won: the tool's future was dropped before it finished.
    assert_eq!(
        completed.load(Ordering::SeqCst),
        0,
        "a cancelled tool must not complete"
    );
    assert!(
        run.tool_responses().is_empty(),
        "no response may be sent for a cancelled call: {:?}",
        run.tool_responses()
    );
    assert_eq!(cancelled.lock().as_slice(), &[vec!["call-1".to_string()]]);
    assert!(run.events().iter().any(|e| matches!(
        e,
        LiveEvent::ToolCancelled { ids } if ids == &["call-1".to_string()]
    )));
    assert!(
        run.events()
            .iter()
            .any(|e| matches!(e, LiveEvent::Interrupted))
    );
    // Releasing the tool now changes nothing: nobody is waiting.
    release.notify_waiters();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(completed.load(Ordering::SeqCst), 0);
    run.disconnect().await;
}

#[tokio::test]
async fn a_server_cancellation_of_an_in_flight_background_tool_sends_no_result() {
    let release = Arc::new(Notify::new());
    let completed = Arc::new(AtomicUsize::new(0));
    let run = ScriptedServer::new()
        .calls("wait", json!({}))
        .frame(cancellation(&["call-1"]))
        .says("Never mind.")
        .settle_after(Duration::from_millis(300))
        .play(
            Live::builder()
                .instruction("test")
                .tools(gated_tool("wait", release.clone(), completed.clone()))
                .tool_background("wait"),
        )
        .await
        .expect("script plays");

    // The background tool acknowledged its start, then the server cancelled
    // the call: its result, if it ever finishes, is not sent.
    assert!(run.events().iter().any(|e| matches!(
        e,
        LiveEvent::ToolCancelled { ids } if ids == &["call-1".to_string()]
    )));
    release.notify_waiters();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let results: Vec<Value> = run
        .tool_responses()
        .into_iter()
        .filter(|r| r["response"]["done"] == json!(true))
        .collect();
    assert!(
        results.is_empty(),
        "a cancelled background call must not deliver its result: {results:?}"
    );
    run.disconnect().await;
}

#[tokio::test]
async fn disconnect_cancels_an_in_flight_tool_at_the_end_of_the_session() {
    let release = Arc::new(Notify::new());
    let completed = Arc::new(AtomicUsize::new(0));
    let run = ScriptedServer::new()
        .calls("wait", json!({}))
        .settle_after(Duration::from_millis(200))
        .play(
            Live::builder()
                .instruction("test")
                .tools(gated_tool("wait", release.clone(), completed.clone()))
                .tool_background("wait"),
        )
        .await
        .expect("script plays");
    assert_eq!(
        completed.load(Ordering::SeqCst),
        0,
        "the tool is still waiting"
    );
    run.disconnect().await;
    // The session is gone: releasing the tool must not let it complete and
    // write anywhere.
    release.notify_waiters();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        completed.load(Ordering::SeqCst),
        0,
        "disconnect must cancel background tools that are still running"
    );
}
