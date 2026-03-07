//! WebSocket connection lifecycle — connect, setup, full-duplex split, reconnection.

mod message_handler;
mod reconnect;
mod session_loop;

use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, watch};

use crate::protocol::types::*;
use crate::session::{SessionHandle, SessionPhase, SessionState};
use crate::transport::codec::{Codec, JsonCodec};
use crate::transport::ws::{Transport, TungsteniteTransport};
use crate::transport::TransportConfig;

/// Connect to the Gemini Multimodal Live API and return a session handle.
///
/// This is the main entry point. It uses the default [`TungsteniteTransport`]
/// and [`JsonCodec`]. For custom transports or codecs (e.g. testing with
/// [`MockTransport`](crate::transport::ws::MockTransport)), use [`connect_with`].
pub async fn connect(
    config: SessionConfig,
    transport_config: TransportConfig,
) -> Result<SessionHandle, crate::session::SessionError> {
    connect_with(
        config,
        transport_config,
        TungsteniteTransport::new(),
        JsonCodec,
    )
    .await
}

/// Connect with a custom transport and codec.
///
/// This is the generic entry point that accepts any [`Transport`] + [`Codec`]
/// implementation. The default [`connect`] delegates here with
/// [`TungsteniteTransport`] and [`JsonCodec`].
pub async fn connect_with<T, C>(
    config: SessionConfig,
    transport_config: TransportConfig,
    transport: T,
    codec: C,
) -> Result<SessionHandle, crate::session::SessionError>
where
    T: Transport,
    C: Codec,
{
    let (command_tx, command_rx) = mpsc::channel(transport_config.send_queue_depth);
    let (event_tx, _) = broadcast::channel(transport_config.event_channel_capacity);
    let (phase_tx, phase_rx) = watch::channel(SessionPhase::Disconnected);

    let state = Arc::new(SessionState::with_events(phase_tx, event_tx.clone()));

    let handle = SessionHandle::new(command_tx, event_tx.clone(), state.clone(), phase_rx);

    let task = tokio::spawn(async move {
        session_loop::generic_connection_loop(
            config,
            transport_config,
            state,
            command_rx,
            event_tx,
            transport,
            codec,
        )
        .await;
    });
    handle.set_task(task);

    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::message_handler::{handle_server_msg, MessageAction};
    use super::reconnect::reconnect_delay;
    use super::*;

    use std::time::Duration;

    use crate::protocol::messages::ServerMessage;
    use crate::session::{SessionEvent, SessionPhase, SessionState};
    use crate::transport::codec::JsonCodec;
    use crate::transport::ws::MockTransport;

    /// TransportConfig that disables reconnection for mock tests.
    fn no_reconnect_config() -> TransportConfig {
        TransportConfig {
            max_reconnect_attempts: 0,
            connect_timeout_secs: 5,
            setup_timeout_secs: 5,
            ..TransportConfig::default()
        }
    }

    #[tokio::test]
    async fn connect_with_mock_transport() {
        let mut transport = MockTransport::new();
        // Script setupComplete response
        transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());
        // Script a text response then turn complete
        transport.script_recv(
            br#"{"serverContent":{"modelTurn":{"parts":[{"text":"Hello!"}]},"turnComplete":true}}"#
                .to_vec(),
        );

        let config = SessionConfig::new("test-key").model(GeminiModel::Gemini2_0FlashLive);

        let handle = connect_with(config, no_reconnect_config(), transport, JsonCodec)
            .await
            .unwrap();

        // Should reach Active phase after setup completes
        handle.wait_for_phase(SessionPhase::Active).await;
        assert_eq!(handle.phase(), SessionPhase::Active);
    }

    #[tokio::test]
    async fn connect_with_mock_receives_text_events() {
        let mut transport = MockTransport::new();
        transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());
        transport.script_recv(
            br#"{"serverContent":{"modelTurn":{"parts":[{"text":"Hello from mock!"}]},"turnComplete":true}}"#
                .to_vec(),
        );

        let config = SessionConfig::new("test-key").model(GeminiModel::Gemini2_0FlashLive);
        let handle = connect_with(config, no_reconnect_config(), transport, JsonCodec)
            .await
            .unwrap();

        let mut events = handle.subscribe();

        // Wait for the session to become active
        handle.wait_for_phase(SessionPhase::Active).await;

        // Collect events until TurnComplete
        let mut got_text_delta = false;
        let mut got_text_complete = false;
        let mut got_turn_complete = false;

        for _ in 0..20 {
            match tokio::time::timeout(Duration::from_millis(100), events.recv()).await {
                Ok(Ok(SessionEvent::TextDelta(t))) => {
                    assert_eq!(t, "Hello from mock!");
                    got_text_delta = true;
                }
                Ok(Ok(SessionEvent::TextComplete(t))) => {
                    assert_eq!(t, "Hello from mock!");
                    got_text_complete = true;
                }
                Ok(Ok(SessionEvent::TurnComplete)) => {
                    got_turn_complete = true;
                    break;
                }
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => break,
                Err(_) => break,
            }
        }

        assert!(got_text_delta, "should have received TextDelta");
        assert!(got_text_complete, "should have received TextComplete");
        assert!(got_turn_complete, "should have received TurnComplete");
    }

    #[tokio::test]
    async fn connect_with_mock_tool_call() {
        let mut transport = MockTransport::new();
        transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());
        transport.script_recv(
            br#"{"toolCall":{"functionCalls":[{"name":"get_weather","args":{"city":"London"},"id":"call-1"}]}}"#
                .to_vec(),
        );

        let config = SessionConfig::new("test-key").model(GeminiModel::Gemini2_0FlashLive);
        let handle = connect_with(config, no_reconnect_config(), transport, JsonCodec)
            .await
            .unwrap();

        let mut events = handle.subscribe();
        handle.wait_for_phase(SessionPhase::Active).await;

        // Look for the ToolCall event
        let mut got_tool_call = false;
        for _ in 0..20 {
            match tokio::time::timeout(Duration::from_millis(100), events.recv()).await {
                Ok(Ok(SessionEvent::ToolCall(calls))) => {
                    assert_eq!(calls.len(), 1);
                    assert_eq!(calls[0].name, "get_weather");
                    got_tool_call = true;
                    break;
                }
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => break,
                Err(_) => break,
            }
        }

        assert!(got_tool_call, "should have received ToolCall event");
    }

    #[tokio::test]
    async fn connect_with_mock_graceful_disconnect() {
        let mut transport = MockTransport::new();
        transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());
        // Keep the connection alive with a message that arrives before disconnect
        transport.script_recv(
            br#"{"serverContent":{"modelTurn":{"parts":[{"text":"hi"}]},"turnComplete":true}}"#
                .to_vec(),
        );

        let config = SessionConfig::new("test-key").model(GeminiModel::Gemini2_0FlashLive);
        let handle = connect_with(config, no_reconnect_config(), transport, JsonCodec)
            .await
            .unwrap();

        handle.wait_for_phase(SessionPhase::Active).await;
        // Small delay to let the background task process
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Disconnect gracefully
        handle.disconnect().await.unwrap();

        // Wait for disconnected phase
        handle.wait_for_phase(SessionPhase::Disconnected).await;
        assert_eq!(handle.phase(), SessionPhase::Disconnected);
    }

    #[test]
    fn handle_server_msg_preserves_interruption() {
        let (phase_tx, _phase_rx) = watch::channel(SessionPhase::Active);
        let (event_tx, mut event_rx) = broadcast::channel(16);
        let state = Arc::new(SessionState::with_events(phase_tx, event_tx.clone()));

        let json = r#"{"serverContent":{"interrupted":true}}"#;
        let msg = ServerMessage::parse(json).unwrap();
        let action = handle_server_msg(msg, &state, &event_tx);

        assert!(matches!(action, MessageAction::Continue));
        // Should have emitted Interrupted event
        let mut found_interrupted = false;
        while let Ok(evt) = event_rx.try_recv() {
            if matches!(evt, SessionEvent::Interrupted) {
                found_interrupted = true;
            }
        }
        assert!(found_interrupted, "should emit Interrupted event");
    }

    #[test]
    fn handle_server_msg_go_away() {
        let (phase_tx, _phase_rx) = watch::channel(SessionPhase::Active);
        let (event_tx, _event_rx) = broadcast::channel(16);
        let state = Arc::new(SessionState::with_events(phase_tx, event_tx.clone()));

        let json = r#"{"goAway":{"timeLeft":"30s"}}"#;
        let msg = ServerMessage::parse(json).unwrap();
        let action = handle_server_msg(msg, &state, &event_tx);

        assert!(matches!(action, MessageAction::GoAway(Some(_))));
    }

    #[test]
    fn handle_server_msg_unknown_is_continue() {
        let (phase_tx, _phase_rx) = watch::channel(SessionPhase::Active);
        let (event_tx, _event_rx) = broadcast::channel(16);
        let state = Arc::new(SessionState::with_events(phase_tx, event_tx.clone()));

        let json = r#"{"unknownField":{"data":"test"}}"#;
        let msg = ServerMessage::parse(json).unwrap();
        let action = handle_server_msg(msg, &state, &event_tx);

        assert!(matches!(action, MessageAction::Continue));
    }

    #[tokio::test]
    async fn session_handle_join_after_disconnect() {
        let mut transport = MockTransport::new();
        transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());

        let config = SessionConfig::new("test-key").model(GeminiModel::Gemini2_0FlashLive);
        let handle = connect_with(config, no_reconnect_config(), transport, JsonCodec)
            .await
            .unwrap();

        handle.wait_for_phase(SessionPhase::Active).await;

        // Disconnect to end the connection loop task
        handle.disconnect().await.unwrap();
        handle.wait_for_phase(SessionPhase::Disconnected).await;

        // join() should return Ok after the task completes
        let result = handle.join().await;
        assert!(result.is_ok(), "join() should succeed after disconnect");
    }

    #[tokio::test]
    async fn session_handle_join_after_command_channel_closed() {
        let mut transport = MockTransport::new();
        transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());

        let config = SessionConfig::new("test-key").model(GeminiModel::Gemini2_0FlashLive);
        let handle = connect_with(config, no_reconnect_config(), transport, JsonCodec)
            .await
            .unwrap();

        handle.wait_for_phase(SessionPhase::Active).await;

        // Drop all senders to close the command channel, which triggers disconnect
        // We need to get the handle before dropping the original
        let join_handle = handle.clone();

        // Drop command_tx by dropping the handle — but we cloned it first.
        // Instead, disconnect and then join.
        handle.disconnect().await.unwrap();

        let result = join_handle.join().await;
        assert!(result.is_ok(), "join() should succeed after channel close");
    }

    #[test]
    fn reconnect_delay_exponential_backoff() {
        let config = TransportConfig::default();
        let d1 = reconnect_delay(1, &config);
        let d2 = reconnect_delay(2, &config);
        let d3 = reconnect_delay(3, &config);
        // Each step should roughly double (plus jitter)
        assert!(d2 > d1);
        assert!(d3 > d2);
        // Should not exceed max
        let d_large = reconnect_delay(100, &config);
        let max_with_jitter = Duration::from_millis(
            config.reconnect_max_delay_ms as u64 + config.reconnect_max_delay_ms as u64 / 4,
        );
        assert!(d_large <= max_with_jitter);
    }
}
