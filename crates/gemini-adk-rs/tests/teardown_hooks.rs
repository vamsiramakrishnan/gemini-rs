//! `on_teardown` — the additive end-of-session hook.
//!
//! Every other callback in `EventCallbacks` is a single `Option` that the last
//! registration silently replaces. That is fine for an application, which has
//! one place to write its handler, and unusable for an extension: it cannot
//! know whether the application will register afterwards and quietly drop it.
//! `gemini-memory-rs` needs exactly this seam to reconcile its session ledger,
//! and before it existed a memory session forgot everything on hang-up.
//!
//! These drive the **real** control plane over a replay transport — no network,
//! no mocked callbacks-registry — so a hook the loop never invokes fails here
//! rather than in production.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gemini_adk_rs::live::LiveSessionBuilder;
use gemini_adk_rs::live::replay::{collect_events_until_idle, replay_session};
use gemini_genai_rs::prelude::{ModelId, SessionConfig};
use gemini_genai_rs::transport::{WireDirection, WireEntry};

/// The minimum viable server script: complete the handshake and finish a turn.
fn server_script() -> Vec<WireEntry> {
    let frames: Vec<&[u8]> = vec![
        br#"{"setupComplete":{}}"#,
        br#"{"serverContent":{"modelTurn":{"parts":[{"text":"Hi."}]},"turnComplete":true}}"#,
    ];
    frames
        .into_iter()
        .enumerate()
        .map(|(i, payload)| WireEntry {
            seq: (i + 1) as u64,
            dir: WireDirection::Inbound,
            ts_ms: 1_718_000_000_000 + i as u64,
            payload: payload.to_vec(),
        })
        .collect()
}

/// Run a session to disconnect with the given callbacks installed.
async fn run_to_disconnect(callbacks: gemini_adk_rs::live::EventCallbacks) {
    let config = SessionConfig::new("test-key").model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO);
    let builder = LiveSessionBuilder::new(config.clone()).callbacks(callbacks);

    let replay = replay_session(config, builder, &server_script())
        .await
        .expect("connects over the replay transport");

    let mut rx = replay.handle().events();
    replay.release();
    replay.drained().await;
    let _ = collect_events_until_idle(&mut rx, Duration::from_millis(200), Duration::from_secs(5))
        .await;

    replay.disconnect().await.expect("disconnect");
    // Disconnect is observed by the control loop asynchronously; give the
    // teardown hooks a bounded window to run before asserting.
    let _ = collect_events_until_idle(&mut rx, Duration::from_millis(200), Duration::from_secs(5))
        .await;
}

#[tokio::test]
async fn a_teardown_hook_runs_on_disconnect() {
    let ran = Arc::new(AtomicUsize::new(0));
    let counter = ran.clone();

    let mut callbacks = gemini_adk_rs::live::EventCallbacks::default();
    callbacks.on_teardown.push(Arc::new(move || {
        let c = counter.clone();
        Box::pin(async move {
            c.fetch_add(1, Ordering::SeqCst);
        })
    }));

    run_to_disconnect(callbacks).await;

    assert_eq!(
        ran.load(Ordering::SeqCst),
        1,
        "a registered teardown hook must run exactly once when the session ends"
    );
}

#[tokio::test]
async fn teardown_hooks_accumulate_and_do_not_replace_each_other() {
    // The whole reason this channel exists: two independent registrations, both
    // honoured. On any other callback the second would silently win.
    let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));

    let mut callbacks = gemini_adk_rs::live::EventCallbacks::default();
    for name in ["first", "second"] {
        let log = order.clone();
        callbacks.on_teardown.push(Arc::new(move || {
            let log = log.clone();
            Box::pin(async move {
                log.lock().expect("not poisoned").push(name);
            })
        }));
    }

    run_to_disconnect(callbacks).await;

    assert_eq!(
        *order.lock().expect("not poisoned"),
        vec!["first", "second"],
        "both hooks must run, in registration order"
    );
}

#[tokio::test]
async fn teardown_runs_before_the_application_callback() {
    // Ordering is the contract: a teardown hook flushes durable state, so the
    // application's own `on_disconnected` should observe a settled world rather
    // than race it.
    let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));

    let teardown_log = order.clone();
    let disconnect_log = order.clone();
    let callbacks = gemini_adk_rs::live::EventCallbacks {
        on_teardown: vec![Arc::new(move || {
            let log = teardown_log.clone();
            Box::pin(async move {
                log.lock().expect("not poisoned").push("teardown");
            })
        })],
        on_disconnected: Some(Arc::new(move |_reason| {
            let log = disconnect_log.clone();
            Box::pin(async move {
                log.lock().expect("not poisoned").push("on_disconnected");
            })
        })),
        ..Default::default()
    };

    run_to_disconnect(callbacks).await;

    assert_eq!(
        *order.lock().expect("not poisoned"),
        vec!["teardown", "on_disconnected"],
        "teardown must complete before the application's disconnect handler runs"
    );
}

#[tokio::test]
async fn an_application_callback_alone_still_works() {
    // The additive channel must not have disturbed the ordinary single-slot
    // path for callers who never touch `on_teardown`.
    let ran = Arc::new(AtomicUsize::new(0));
    let counter = ran.clone();

    let callbacks = gemini_adk_rs::live::EventCallbacks {
        on_disconnected: Some(Arc::new(move |_reason| {
            let c = counter.clone();
            Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
            })
        })),
        ..Default::default()
    };

    run_to_disconnect(callbacks).await;

    assert_eq!(ran.load(Ordering::SeqCst), 1);
}
