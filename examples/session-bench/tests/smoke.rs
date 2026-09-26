//! The harness runs under `cargo test --workspace`: a handful of sessions,
//! a couple of turns each, every turn completes, and the runtime's own
//! telemetry agrees with the harness about how many turns there were.

use std::time::Duration;

use example_session_bench::{BenchConfig, JitterConfig, run, run_jitter};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_sessions_complete_every_turn() {
    let cfg = BenchConfig {
        sessions: 4,
        turns: 2,
        // Not zero: with no gap at all, the next `send_text` can land before
        // the telemetry lane (a third consumer of the L0 broadcast) has seen
        // the previous turn's first delta, and the runtime then attributes
        // that delta to the new send. See "Known limitation" in
        // docs/user-guide/capacity.md. A few milliseconds is enough; a
        // caller never types the next line inside the lane's scheduling lag.
        think: Duration::from_millis(10),
        response_delay: Duration::from_millis(5),
        hold: Duration::ZERO,
        turn_timeout: Duration::from_secs(10),
    };
    let report = run(cfg).await;

    assert_eq!(report.turns_completed, 8, "{}", report.summary());
    assert_eq!(report.turns_failed, 0, "{}", report.summary());
    assert_eq!(
        report.telemetry_response_count,
        8,
        "runtime telemetry must count the same turns the harness drove: {}",
        report.summary()
    );
    assert_eq!(report.connect_ms.count, 4);
    assert_eq!(report.first_text_ms.count, 8);
    assert_eq!(report.turn_ms.count, 8);
    // The scripted delay is a floor on every turn. No ordering is asserted
    // between first text and turn completion: they travel on different lanes.
    assert!(report.first_text_ms.p50_ms >= 5.0, "{}", report.summary());
    assert!(report.turn_ms.p50_ms >= 5.0, "{}", report.summary());

    // The JSON report round-trips. Floats are compared with a tolerance:
    // serde_json's parser is not guaranteed correctly rounded without the
    // `float_roundtrip` feature, and a last-ulp difference is not a defect.
    let json = serde_json::to_string(&report).unwrap();
    let back: example_session_bench::Report = serde_json::from_str(&json).unwrap();
    assert_eq!(back.sessions, report.sessions);
    assert_eq!(back.turns_completed, report.turns_completed);
    assert_eq!(
        back.telemetry_response_count,
        report.telemetry_response_count
    );
    assert_eq!(back.rss, report.rss);
    assert_eq!(back.turn_ms.count, report.turn_ms.count);
    assert!((back.turn_ms.p99_ms - report.turn_ms.p99_ms).abs() < 1e-6);
    assert!((back.first_text_ms.p50_ms - report.first_text_ms.p50_ms).abs() < 1e-6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_microphone_chunk_reaches_the_wire_in_order() {
    let report = run_jitter(JitterConfig {
        sessions: 3,
        duration: Duration::from_millis(600),
        chunk: Duration::from_millis(20),
    })
    .await;
    assert_eq!(report.sessions, 3, "{}", report.summary());
    assert!(report.chunks_sent >= 3 * 25, "{}", report.summary());
    assert_eq!(
        report.chunks_wired,
        report.chunks_sent,
        "{}",
        report.summary()
    );
    assert_eq!(report.chunks_reordered, 0, "{}", report.summary());
    assert_eq!(report.mic_to_wire_ms.count, report.chunks_sent);
    // Spacing is measured between consecutive frames of one session.
    assert_eq!(report.wire_jitter_ms.count, report.chunks_sent - 3);
}
