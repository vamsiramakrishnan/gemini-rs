//! WebSocket connection lifecycle — connect, setup, full-duplex split, reconnection.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use crate::protocol::messages::*;
use crate::protocol::types::*;
use crate::session::{
    SessionCommand, SessionError, SessionEvent, SessionHandle, SessionPhase, SessionState,
};
use crate::transport::TransportConfig;

/// Connect to the Gemini Multimodal Live API and return a session handle.
///
/// This is the main entry point. It:
/// 1. Creates shared state and channels
/// 2. Spawns the connection loop as a background Tokio task
/// 3. Returns a cheaply-cloneable `SessionHandle`
pub async fn connect(
    config: SessionConfig,
    transport_config: TransportConfig,
) -> Result<SessionHandle, SessionError> {
    let (command_tx, command_rx) = mpsc::channel(transport_config.send_queue_depth);
    let (event_tx, _) = broadcast::channel(transport_config.event_channel_capacity);
    let (phase_tx, phase_rx) = watch::channel(SessionPhase::Disconnected);

    let state = Arc::new(SessionState::new(phase_tx));

    let handle = SessionHandle::new(
        command_tx,
        event_tx.clone(),
        state.clone(),
        phase_rx,
    );

    // Spawn the connection loop
    let loop_state = state.clone();
    let loop_event_tx = event_tx.clone();
    tokio::spawn(async move {
        connection_loop(config, transport_config, loop_state, command_rx, loop_event_tx).await;
    });

    Ok(handle)
}

/// The main connection loop — manages connect, setup, send/recv, and reconnection.
async fn connection_loop(
    config: SessionConfig,
    transport_config: TransportConfig,
    state: Arc<SessionState>,
    mut command_rx: mpsc::Receiver<SessionCommand>,
    event_tx: broadcast::Sender<SessionEvent>,
) {
    let mut attempt = 0u32;

    loop {
        // Transition to Connecting
        if state.transition_to(SessionPhase::Connecting).is_err() {
            // If we can't transition, force it
            state.force_phase(SessionPhase::Connecting);
        }

        match establish_connection(&config, &transport_config, &state, &event_tx).await {
            Ok((ws_write, ws_read)) => {
                attempt = 0; // Reset backoff on successful connect

                // Run the session until disconnect
                let disconnect_reason = run_session(
                    &config,
                    ws_write,
                    ws_read,
                    &state,
                    &mut command_rx,
                    &event_tx,
                )
                .await;

                match disconnect_reason {
                    DisconnectReason::Graceful => {
                        let _ = state.transition_to(SessionPhase::Disconnected);
                        let _ = event_tx.send(SessionEvent::Disconnected(None));
                        return;
                    }
                    DisconnectReason::GoAway(time_left) => {
                        // Try to reconnect with session resume
                        let _ = event_tx.send(SessionEvent::GoAway(time_left));
                        // Fall through to reconnect
                    }
                    DisconnectReason::Error(e) => {
                        let _ = event_tx.send(SessionEvent::Error(e.clone()));
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
            Err(e) => {
                let _ = event_tx.send(SessionEvent::Error(format!("Connection failed: {e}")));
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

/// Reason for session disconnect.
enum DisconnectReason {
    Graceful,
    GoAway(Option<String>),
    Error(String),
    CommandChannelClosed,
}

/// Establish a WebSocket connection and complete the setup handshake.
async fn establish_connection(
    config: &SessionConfig,
    transport_config: &TransportConfig,
    state: &Arc<SessionState>,
    event_tx: &broadcast::Sender<SessionEvent>,
) -> Result<
    (
        futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
    ),
    SessionError,
> {
    let url = config.ws_url();

    // Build request — attach Authorization header for Vertex AI
    let mut request = url
        .into_client_request()
        .map_err(|e| SessionError::WebSocket(e.to_string()))?;
    if let Some(token) = config.bearer_token() {
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {token}")
                .parse()
                .map_err(|e: tokio_tungstenite::tungstenite::http::header::InvalidHeaderValue| {
                    SessionError::SetupFailed(format!("invalid bearer token header: {e}"))
                })?,
        );
    }

    // Connect WebSocket
    let (ws_stream, _response) = tokio::time::timeout(
        Duration::from_secs(transport_config.connect_timeout_secs),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .map_err(|_| SessionError::Timeout)?
    .map_err(|e| SessionError::WebSocket(e.to_string()))?;

    let (mut ws_write, mut ws_read) = ws_stream.split();

    // Send setup message
    let _ = state.transition_to(SessionPhase::SetupSent);
    let setup_json = config.to_setup_json();
    ws_write
        .send(Message::Text(setup_json))
        .await
        .map_err(|e| SessionError::WebSocket(e.to_string()))?;

    // Wait for setupComplete
    let setup_complete = tokio::time::timeout(
        Duration::from_secs(transport_config.setup_timeout_secs),
        async {
            while let Some(msg) = ws_read.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(ServerMessage::SetupComplete(sc)) =
                            ServerMessage::parse(&text)
                        {
                            return Ok(sc);
                        }
                    }
                    Ok(Message::Close(_)) => {
                        return Err(SessionError::SetupFailed("Connection closed during setup".into()));
                    }
                    Err(e) => {
                        return Err(SessionError::WebSocket(e.to_string()));
                    }
                    _ => {}
                }
            }
            Err(SessionError::SetupFailed("Stream ended before setup complete".into()))
        },
    )
    .await
    .map_err(|_| SessionError::Timeout)??;

    // Extract session resume handle if present
    if let Some(ref resumption) = setup_complete.setup_complete.session_resumption {
        if let Some(ref handle) = resumption.handle {
            *state.resume_handle.lock() = Some(handle.clone());
            let _ = event_tx.send(SessionEvent::SessionResumeHandle(handle.clone()));
        }
    }

    // Transition to Active
    let _ = state.transition_to(SessionPhase::Active);
    let _ = event_tx.send(SessionEvent::Connected);

    Ok((ws_write, ws_read))
}

/// Run the full-duplex session: send task + receive task.
async fn run_session(
    config: &SessionConfig,
    mut ws_write: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    mut ws_read: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    state: &Arc<SessionState>,
    command_rx: &mut mpsc::Receiver<SessionCommand>,
    event_tx: &broadcast::Sender<SessionEvent>,
) -> DisconnectReason {
    let mime_type = config.input_audio_format.mime_type().to_string();

    loop {
        tokio::select! {
            // Receive from WebSocket
            msg = ws_read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match handle_server_message(&text, state, event_tx) {
                            MessageAction::Continue => {}
                            MessageAction::GoAway(time_left) => {
                                return DisconnectReason::GoAway(time_left);
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        return DisconnectReason::Error("Server closed connection".to_string());
                    }
                    Some(Err(e)) => {
                        return DisconnectReason::Error(e.to_string());
                    }
                    None => {
                        return DisconnectReason::Error("WebSocket stream ended".to_string());
                    }
                    _ => {} // Ping/Pong handled by tungstenite
                }
            }

            // Send commands
            cmd = command_rx.recv() => {
                match cmd {
                    Some(SessionCommand::SendAudio(data)) => {
                        let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
                        let msg = RealtimeInputMessage {
                            realtime_input: RealtimeInputPayload {
                                media_chunks: Vec::new(),
                                audio: Some(Blob {
                                    mime_type: mime_type.clone(),
                                    data: encoded,
                                }),
                                video: None,
                                audio_stream_end: None,
                                text: None,
                            },
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            if ws_write.send(Message::Text(json)).await.is_err() {
                                return DisconnectReason::Error("Failed to send audio".to_string());
                            }
                        }
                    }
                    Some(SessionCommand::SendText(text)) => {
                        let msg = ClientContentMessage {
                            client_content: ClientContentPayload {
                                turns: vec![Content {
                                    role: Some("user".to_string()),
                                    parts: vec![Part::Text { text }],
                                }],
                                turn_complete: Some(true),
                            },
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            if ws_write.send(Message::Text(json)).await.is_err() {
                                return DisconnectReason::Error("Failed to send text".to_string());
                            }
                        }
                    }
                    Some(SessionCommand::SendToolResponse(responses)) => {
                        let msg = ToolResponseMessage {
                            tool_response: ToolResponsePayload {
                                function_responses: responses,
                            },
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            if ws_write.send(Message::Text(json)).await.is_err() {
                                return DisconnectReason::Error("Failed to send tool response".to_string());
                            }
                        }
                    }
                    Some(SessionCommand::ActivityStart) => {
                        let msg = ActivitySignalMessage {
                            realtime_input: ActivitySignalPayload {
                                activity_start: Some(ActivityStart {}),
                                activity_end: None,
                            },
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = ws_write.send(Message::Text(json)).await;
                        }
                    }
                    Some(SessionCommand::ActivityEnd) => {
                        let msg = ActivitySignalMessage {
                            realtime_input: ActivitySignalPayload {
                                activity_start: None,
                                activity_end: Some(ActivityEnd {}),
                            },
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = ws_write.send(Message::Text(json)).await;
                        }
                    }
                    Some(SessionCommand::SendClientContent { turns, turn_complete }) => {
                        let msg = ClientContentMessage {
                            client_content: ClientContentPayload {
                                turns,
                                turn_complete: Some(turn_complete),
                            },
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            if ws_write.send(Message::Text(json)).await.is_err() {
                                return DisconnectReason::Error("Failed to send client content".to_string());
                            }
                        }
                    }
                    Some(SessionCommand::Disconnect) => {
                        let _ = state.transition_to(SessionPhase::Disconnecting);
                        let _ = ws_write.send(Message::Close(None)).await;
                        return DisconnectReason::Graceful;
                    }
                    None => {
                        return DisconnectReason::CommandChannelClosed;
                    }
                }
            }
        }
    }
}

/// Action to take after processing a server message.
enum MessageAction {
    Continue,
    GoAway(Option<String>),
}

/// Process a single server message and emit appropriate events.
fn handle_server_message(
    text: &str,
    state: &Arc<SessionState>,
    event_tx: &broadcast::Sender<SessionEvent>,
) -> MessageAction {
    let msg = match ServerMessage::parse(text) {
        Ok(msg) => msg,
        Err(_) => return MessageAction::Continue,
    };

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
                                base64::engine::general_purpose::STANDARD.decode(&inline_data.data)
                            {
                                let _ = event_tx.send(SessionEvent::AudioData(audio_bytes));
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
    let delay_ms = (base_ms * 2u64.saturating_pow(attempt.saturating_sub(1))).min(max_ms);
    // Add ~25% jitter
    let jitter = delay_ms / 4;
    Duration::from_millis(delay_ms + jitter)
}
