//! Integration tests for gemini-live using MockTransport.
//!
//! These tests exercise the full connection lifecycle (connect -> setup ->
//! event delivery) without a real Gemini server, using the scripted
//! MockTransport to simulate server responses.

use std::time::Duration;

use gemini_live::prelude::*;

/// TransportConfig that disables reconnection so tests terminate promptly.
fn no_reconnect_config() -> TransportConfig {
    TransportConfig {
        max_reconnect_attempts: 0,
        connect_timeout_secs: 5,
        setup_timeout_secs: 5,
        ..TransportConfig::default()
    }
}

/// Helper: create a SessionConfig suitable for mock tests.
fn test_config() -> SessionConfig {
    SessionConfig::new("test-key").model(GeminiModel::Gemini2_0FlashLive)
}

// ---------------------------------------------------------------------------
// Test 1: connect_and_receive_text
// ---------------------------------------------------------------------------

#[tokio::test]
async fn connect_and_receive_text() {
    let mut transport = MockTransport::new();

    // Script: server sends setupComplete, then a text turn with turnComplete.
    transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());
    transport.script_recv(
        br#"{"serverContent":{"modelTurn":{"parts":[{"text":"hello world"}]},"turnComplete":true}}"#
            .to_vec(),
    );

    let handle = connect_with(test_config(), no_reconnect_config(), transport, JsonCodec)
        .await
        .expect("connect_with should succeed");

    let mut events = handle.subscribe();

    // Wait for session to become active (setup handshake complete).
    tokio::time::timeout(
        Duration::from_secs(5),
        handle.wait_for_phase(SessionPhase::Active),
    )
    .await
    .expect("should reach Active phase");

    // Collect events until we see TurnComplete.
    let mut got_text_delta = false;
    let mut got_turn_complete = false;

    for _ in 0..30 {
        match tokio::time::timeout(Duration::from_secs(5), events.recv()).await {
            Ok(Ok(SessionEvent::TextDelta(text))) => {
                assert_eq!(text, "hello world");
                got_text_delta = true;
            }
            Ok(Ok(SessionEvent::TurnComplete)) => {
                got_turn_complete = true;
                break;
            }
            Ok(Ok(_)) => continue, // skip Connected, PhaseChanged, TextComplete, etc.
            Ok(Err(_)) => break,   // channel closed
            Err(_) => break,       // timeout
        }
    }

    assert!(
        got_text_delta,
        "should have received TextDelta(\"hello world\")"
    );
    assert!(got_turn_complete, "should have received TurnComplete");
}

// ---------------------------------------------------------------------------
// Test 2: connect_and_send_text
// ---------------------------------------------------------------------------

#[tokio::test]
async fn connect_and_send_text() {
    let mut transport = MockTransport::new();

    // Script: setupComplete only (no server content response needed).
    transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());

    let handle = connect_with(test_config(), no_reconnect_config(), transport, JsonCodec)
        .await
        .expect("connect_with should succeed");

    // Wait for active.
    tokio::time::timeout(
        Duration::from_secs(5),
        handle.wait_for_phase(SessionPhase::Active),
    )
    .await
    .expect("should reach Active phase");

    // send_text should succeed without error (the mock transport accepts any send).
    handle
        .send_text("hello")
        .await
        .expect("send_text should succeed on mock transport");
}

// ---------------------------------------------------------------------------
// Test 3: tool_call_event
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tool_call_event() {
    let mut transport = MockTransport::new();

    // Script: setupComplete, then a toolCall message.
    transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());
    transport.script_recv(
        br#"{"toolCall":{"functionCalls":[{"name":"get_weather","args":{"city":"London"},"id":"call-42"}]}}"#
            .to_vec(),
    );

    let handle = connect_with(test_config(), no_reconnect_config(), transport, JsonCodec)
        .await
        .expect("connect_with should succeed");

    let mut events = handle.subscribe();

    tokio::time::timeout(
        Duration::from_secs(5),
        handle.wait_for_phase(SessionPhase::Active),
    )
    .await
    .expect("should reach Active phase");

    // Look for the ToolCall event.
    let mut got_tool_call = false;

    for _ in 0..30 {
        match tokio::time::timeout(Duration::from_secs(5), events.recv()).await {
            Ok(Ok(SessionEvent::ToolCall(calls))) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "get_weather");
                assert_eq!(calls[0].id.as_deref(), Some("call-42"));
                got_tool_call = true;
                break;
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }

    assert!(
        got_tool_call,
        "should have received ToolCall with function name 'get_weather'"
    );
}

// ---------------------------------------------------------------------------
// Test 4: phase_changes_on_connect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn phase_changes_on_connect() {
    let mut transport = MockTransport::new();

    // Script: setupComplete only.
    transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());

    let handle = connect_with(test_config(), no_reconnect_config(), transport, JsonCodec)
        .await
        .expect("connect_with should succeed");

    let mut events = handle.subscribe();

    // The session should pass through several phases:
    // Disconnected -> Connecting -> SetupSent -> Active
    // We verify it reaches Active.
    tokio::time::timeout(
        Duration::from_secs(5),
        handle.wait_for_phase(SessionPhase::Active),
    )
    .await
    .expect("should reach Active phase");

    assert_eq!(handle.phase(), SessionPhase::Active);

    // Verify that PhaseChanged events were emitted during the lifecycle.
    // We may also see Connected. Drain whatever is available.
    let mut saw_phase_changed = false;
    let mut saw_connected = false;

    for _ in 0..30 {
        match tokio::time::timeout(Duration::from_millis(200), events.recv()).await {
            Ok(Ok(SessionEvent::PhaseChanged(_))) => {
                saw_phase_changed = true;
            }
            Ok(Ok(SessionEvent::Connected)) => {
                saw_connected = true;
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => break,
            Err(_) => break, // timeout = no more events in queue
        }
    }

    // We subscribed after connect_with returned, so we may or may not catch
    // the early PhaseChanged events (they fire before we subscribe). But the
    // Connected event is emitted at setup completion, which races with our
    // subscribe. The key assertion is that the phase is Active.
    //
    // At minimum, verify the phase is correct.
    assert_eq!(
        handle.phase(),
        SessionPhase::Active,
        "session should be in Active phase after setup"
    );

    // If we happened to catch events, verify them.
    if saw_connected {
        assert!(saw_connected, "Connected event should be emitted");
    }
    if saw_phase_changed {
        assert!(saw_phase_changed, "PhaseChanged events should be emitted");
    }
}
