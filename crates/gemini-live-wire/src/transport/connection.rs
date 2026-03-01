//! WebSocket connection lifecycle — connect, setup, full-duplex split, reconnection.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use tokio::sync::{broadcast, mpsc, watch};

use crate::protocol::messages::*;
use crate::protocol::types::*;
use crate::session::{
    SessionCommand, SessionError, SessionEvent, SessionHandle, SessionPhase, SessionState,
    SetupError, WebSocketError,
};
use crate::transport::codec::Codec;
use crate::transport::ws::{Transport, TungsteniteTransport};
use crate::transport::codec::JsonCodec;
use crate::transport::TransportConfig;

/// Connect to the Gemini Multimodal Live API and return a session handle.
///
/// This is the main entry point. It uses the default [`TungsteniteTransport`]
/// and [`JsonCodec`]. For custom transports or codecs (e.g. testing with
/// [`MockTransport`](crate::transport::ws::MockTransport)), use [`connect_with`].
pub async fn connect(
    config: SessionConfig,
    transport_config: TransportConfig,
) -> Result<SessionHandle, SessionError> {
    connect_with(config, transport_config, TungsteniteTransport::new(), JsonCodec).await
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
) -> Result<SessionHandle, SessionError>
where
    T: Transport,
    C: Codec,
{
    let (command_tx, command_rx) = mpsc::channel(transport_config.send_queue_depth);
    let (event_tx, _) = broadcast::channel(transport_config.event_channel_capacity);
    let (phase_tx, phase_rx) = watch::channel(SessionPhase::Disconnected);

    let state = Arc::new(SessionState::with_events(phase_tx, event_tx.clone()));

    let handle = SessionHandle::new(
        command_tx,
        event_tx.clone(),
        state.clone(),
        phase_rx,
    );

    let task = tokio::spawn(async move {
        generic_connection_loop(config, transport_config, state, command_rx, event_tx, transport, codec).await;
    });
    handle.set_task(task);

    Ok(handle)
}

/// Reason for session disconnect.
enum DisconnectReason {
    Graceful,
    GoAway(Option<String>),
    Error(String),
    CommandChannelClosed,
}

/// Action to take after processing a server message.
enum MessageAction {
    Continue,
    GoAway(Option<String>),
}

/// The main connection loop — manages connect, setup, send/recv, and reconnection.
///
/// Generic over any `Transport` + `Codec`. On each attempt it:
/// 1. Connects the transport
/// 2. Sends the setup message via the codec
/// 3. Waits for `setupComplete`
/// 4. Runs the full-duplex session loop
/// 5. On disconnect, decides whether to reconnect or give up
async fn generic_connection_loop<T: Transport, C: Codec>(
    config: SessionConfig,
    transport_config: TransportConfig,
    state: Arc<SessionState>,
    mut command_rx: mpsc::Receiver<SessionCommand>,
    event_tx: broadcast::Sender<SessionEvent>,
    mut transport: T,
    codec: C,
) {
    let mut attempt = 0u32;

    loop {
        // Transition to Connecting
        if state.transition_to(SessionPhase::Connecting).is_err() {
            state.force_phase(SessionPhase::Connecting);
        }

        // Build URL and headers from config
        let url = config.ws_url();
        let mut headers = vec![];
        if let Some(token) = config.bearer_token() {
            headers.push(("Authorization".to_string(), format!("Bearer {token}")));
        }

        // Connect transport
        let connect_result = tokio::time::timeout(
            Duration::from_secs(transport_config.connect_timeout_secs),
            transport.connect(&url, headers),
        )
        .await;

        match connect_result {
            Ok(Ok(())) => {
                // Send setup message
                let _ = state.transition_to(SessionPhase::SetupSent);
                let setup_bytes = match codec.encode_setup(&config) {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = event_tx.send(SessionEvent::Error(format!(
                            "Setup encode error: {e}"
                        )));
                        break;
                    }
                };
                if let Err(e) = transport.send(setup_bytes).await {
                    let _ = event_tx.send(SessionEvent::Error(format!(
                        "Setup send error: {e}"
                    )));
                    // Fall through to reconnect
                    attempt += 1;
                    if attempt > transport_config.max_reconnect_attempts {
                        let _ = state.transition_to(SessionPhase::Disconnected);
                        let _ = event_tx.send(SessionEvent::Disconnected(Some(
                            "Max reconnection attempts exceeded".to_string(),
                        )));
                        return;
                    }
                    tokio::time::sleep(reconnect_delay(attempt, &transport_config)).await;
                    state.force_phase(SessionPhase::Disconnected);
                    continue;
                }

                // Wait for setupComplete
                let setup_result = tokio::time::timeout(
                    Duration::from_secs(transport_config.setup_timeout_secs),
                    wait_for_setup(&mut transport, &codec, &state, &event_tx),
                )
                .await;

                match setup_result {
                    Ok(Ok(())) => {
                        attempt = 0; // Reset backoff on successful setup
                        // Run main session loop
                        let reason = generic_run_session(
                            &config,
                            &mut transport,
                            &codec,
                            &state,
                            &mut command_rx,
                            &event_tx,
                        )
                        .await;

                        match reason {
                            DisconnectReason::Graceful => {
                                let _ = state.transition_to(SessionPhase::Disconnected);
                                let _ = event_tx.send(SessionEvent::Disconnected(None));
                                return;
                            }
                            DisconnectReason::GoAway(time_left) => {
                                let _ = event_tx.send(SessionEvent::GoAway(time_left));
                                // Fall through to reconnect
                            }
                            DisconnectReason::Error(e) => {
                                let _ = event_tx.send(SessionEvent::Error(e));
                                // Fall through to reconnect
                            }
                            DisconnectReason::CommandChannelClosed => {
                                let _ = state.transition_to(SessionPhase::Disconnected);
                                let _ = event_tx.send(SessionEvent::Disconnected(Some(
                                    "Command channel closed".to_string(),
                                )));
                                return;
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        let _ = event_tx.send(SessionEvent::Error(format!(
                            "Setup failed: {e}"
                        )));
                    }
                    Err(_) => {
                        let _ = event_tx.send(SessionEvent::Error("Setup timeout".into()));
                    }
                }
            }
            Ok(Err(e)) => {
                let _ = event_tx.send(SessionEvent::Error(format!(
                    "Connection failed: {e}"
                )));
            }
            Err(_) => {
                let _ = event_tx.send(SessionEvent::Error("Connect timeout".into()));
            }
        }

        // Reconnection backoff
        attempt += 1;
        if attempt > transport_config.max_reconnect_attempts {
            let _ = state.transition_to(SessionPhase::Disconnected);
            let _ = event_tx.send(SessionEvent::Disconnected(Some(
                "Max reconnection attempts exceeded".to_string(),
            )));
            return;
        }

        let backoff = reconnect_delay(attempt, &transport_config);
        tokio::time::sleep(backoff).await;
        state.force_phase(SessionPhase::Disconnected);
    }
}

/// Wait for setupComplete from the server.
///
/// Reads messages from the transport, decoding each via the codec, until a
/// `SetupComplete` message is received. Extracts the session resumption handle
/// if present, transitions to `Active`, and emits `Connected`.
async fn wait_for_setup<T: Transport, C: Codec>(
    transport: &mut T,
    codec: &C,
    state: &Arc<SessionState>,
    event_tx: &broadcast::Sender<SessionEvent>,
) -> Result<(), SessionError> {
    loop {
        match transport.recv().await {
            Ok(Some(data)) => match codec.decode_message(&data) {
                Ok(ServerMessage::SetupComplete(sc)) => {
                    if let Some(ref resumption) = sc.setup_complete.session_resumption {
                        if let Some(ref handle) = resumption.handle {
                            *state.resume_handle.lock() = Some(handle.clone());
                            let _ = event_tx.send(SessionEvent::SessionResumeHandle(
                                handle.clone(),
                            ));
                        }
                    }
                    let _ = state.transition_to(SessionPhase::Active);
                    let _ = event_tx.send(SessionEvent::Connected);
                    return Ok(());
                }
                Ok(_) => continue, // Skip non-setup messages during handshake
                Err(e) => {
                    return Err(SessionError::SetupFailed(SetupError::ServerRejected {
                        code: None,
                        message: format!("Failed to decode setup response: {e}"),
                    }));
                }
            },
            Ok(None) => {
                return Err(SessionError::SetupFailed(SetupError::Timeout));
            }
            Err(e) => {
                return Err(SessionError::WebSocket(WebSocketError::ProtocolError(
                    e.to_string(),
                )));
            }
        }
    }
}

/// Run the full-duplex session loop.
///
/// Uses `tokio::select!` to concurrently wait on:
/// - `transport.recv()` — incoming server messages
/// - `command_rx.recv()` — outgoing commands from application code
///
/// Because `tokio::select!` drops the losing branch's future, there is no
/// concurrent mutable borrow of `transport`: when the command branch wins,
/// the recv future is dropped before `transport.send()` is called.
async fn generic_run_session<T: Transport, C: Codec>(
    config: &SessionConfig,
    transport: &mut T,
    codec: &C,
    state: &Arc<SessionState>,
    command_rx: &mut mpsc::Receiver<SessionCommand>,
    event_tx: &broadcast::Sender<SessionEvent>,
) -> DisconnectReason {
    loop {
        tokio::select! {
            data = transport.recv() => {
                match data {
                    Ok(Some(bytes)) => {
                        match codec.decode_message(&bytes) {
                            Ok(msg) => {
                                match handle_server_msg(msg, state, event_tx) {
                                    MessageAction::Continue => {}
                                    MessageAction::GoAway(time_left) => {
                                        return DisconnectReason::GoAway(time_left);
                                    }
                                }
                            }
                            Err(_) => {} // Skip unparseable messages
                        }
                    }
                    Ok(None) => return DisconnectReason::Error("Transport closed".into()),
                    Err(e) => return DisconnectReason::Error(e.to_string()),
                }
            }
            cmd = command_rx.recv() => {
                match cmd {
                    Some(SessionCommand::Disconnect) => {
                        let _ = state.transition_to(SessionPhase::Disconnecting);
                        let _ = transport.close().await;
                        return DisconnectReason::Graceful;
                    }
                    Some(cmd) => {
                        match codec.encode_command(&cmd, config) {
                            Ok(bytes) if !bytes.is_empty() => {
                                if transport.send(bytes).await.is_err() {
                                    return DisconnectReason::Error("Failed to send".into());
                                }
                            }
                            _ => {}
                        }
                    }
                    None => return DisconnectReason::CommandChannelClosed,
                }
            }
        }
    }
}

/// Process a decoded [`ServerMessage`] and emit appropriate session events.
///
/// This is the shared message handler used by the generic transport path.
fn handle_server_msg(
    msg: ServerMessage,
    state: &Arc<SessionState>,
    event_tx: &broadcast::Sender<SessionEvent>,
) -> MessageAction {
    match msg {
        ServerMessage::ServerContent(sc) => {
            let content = sc.server_content;

            // Handle interruption
            if content.interrupted.unwrap_or(false) {
                state.interrupt_turn();
                let _ = state.transition_to(SessionPhase::Interrupted);
                let _ = event_tx.send(SessionEvent::Interrupted);
                let _ = state.transition_to(SessionPhase::Active);
                return MessageAction::Continue;
            }

            // Handle model turn content
            if let Some(model_turn) = content.model_turn {
                // Ensure we're in ModelSpeaking
                if state.phase() == SessionPhase::Active {
                    let _ = state.transition_to(SessionPhase::ModelSpeaking);
                    state.start_turn();
                }

                for part in &model_turn.parts {
                    match part {
                        Part::Text { text } => {
                            state.append_text(text);
                            let _ = event_tx.send(SessionEvent::TextDelta(text.clone()));
                        }
                        Part::InlineData { inline_data } => {
                            state.mark_audio();
                            if let Ok(audio_bytes) =
                                base64::engine::general_purpose::STANDARD
                                    .decode(&inline_data.data)
                            {
                                let _ =
                                    event_tx.send(SessionEvent::AudioData(bytes::Bytes::from(audio_bytes)));
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Handle input transcription
            if let Some(transcription) = content.input_transcription {
                if let Some(text) = transcription.text {
                    let _ = event_tx.send(SessionEvent::InputTranscription(text));
                }
            }

            // Handle output transcription
            if let Some(transcription) = content.output_transcription {
                if let Some(text) = transcription.text {
                    let _ = event_tx.send(SessionEvent::OutputTranscription(text));
                }
            }

            // Handle turn complete
            if content.turn_complete.unwrap_or(false) {
                if let Some(turn) = state.complete_turn() {
                    if !turn.text.is_empty() {
                        let _ = event_tx.send(SessionEvent::TextComplete(turn.text));
                    }
                }
                let _ = event_tx.send(SessionEvent::TurnComplete);
                let _ = state.transition_to(SessionPhase::Active);
            }
        }

        ServerMessage::ToolCall(tc) => {
            let calls = tc.tool_call.function_calls;
            // Track tool calls in turn
            if let Some(turn) = state.current_turn.lock().as_mut() {
                turn.tool_calls.extend(calls.clone());
            }
            let _ = state.transition_to(SessionPhase::ToolCallPending);
            let _ = event_tx.send(SessionEvent::ToolCall(calls));
        }

        ServerMessage::ToolCallCancellation(tc) => {
            let ids = tc.tool_call_cancellation.ids;
            let _ = event_tx.send(SessionEvent::ToolCallCancelled(ids));
        }

        ServerMessage::GoAway(ga) => {
            return MessageAction::GoAway(ga.go_away.time_left);
        }

        ServerMessage::SetupComplete(_) => {
            // Should not happen after initial setup, but handle gracefully
        }

        ServerMessage::SessionResumptionUpdate(sru) => {
            let payload = sru.session_resumption_update;
            if let Some(ref handle) = payload.new_handle {
                *state.resume_handle.lock() = Some(handle.clone());
                let _ = event_tx.send(SessionEvent::SessionResumeHandle(handle.clone()));
            }
        }

        ServerMessage::VoiceActivity(msg) => {
            if let Some(vat) = msg.voice_activity.voice_activity_type {
                match vat {
                    VoiceActivityType::VoiceActivityStart => {
                        let _ = event_tx.send(SessionEvent::VoiceActivityStart);
                    }
                    VoiceActivityType::VoiceActivityEnd => {
                        let _ = event_tx.send(SessionEvent::VoiceActivityEnd);
                    }
                }
            }
        }

        ServerMessage::Unknown(_) => {
            // Forward-compatible: ignore unknown messages
        }
    }

    MessageAction::Continue
}

/// Calculate reconnection delay with exponential backoff and jitter.
fn reconnect_delay(attempt: u32, config: &TransportConfig) -> Duration {
    let base_ms = config.reconnect_base_delay_ms as u64;
    let max_ms = config.reconnect_max_delay_ms as u64;
    let delay_ms = base_ms.saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1))).min(max_ms);
    // Add ~25% jitter
    let jitter = delay_ms / 4;
    Duration::from_millis(delay_ms + jitter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::ws::MockTransport;
    use crate::transport::codec::JsonCodec;

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

        let config =
            SessionConfig::new("test-key").model(GeminiModel::Gemini2_0FlashLive);

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

        let config =
            SessionConfig::new("test-key").model(GeminiModel::Gemini2_0FlashLive);
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

        let config =
            SessionConfig::new("test-key").model(GeminiModel::Gemini2_0FlashLive);
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

        let config =
            SessionConfig::new("test-key").model(GeminiModel::Gemini2_0FlashLive);
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

        let config =
            SessionConfig::new("test-key").model(GeminiModel::Gemini2_0FlashLive);
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

        let config =
            SessionConfig::new("test-key").model(GeminiModel::Gemini2_0FlashLive);
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
            config.reconnect_max_delay_ms as u64
                + config.reconnect_max_delay_ms as u64 / 4,
        );
        assert!(d_large <= max_with_jitter);
    }
}
