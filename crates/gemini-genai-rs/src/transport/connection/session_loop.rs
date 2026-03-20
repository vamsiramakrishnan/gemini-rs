//! Session lifecycle — connection loop, setup handshake, full-duplex send/recv.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};

use crate::protocol::messages::*;
use crate::protocol::types::*;
use crate::session::{
    ResumeInfo, SessionCommand, SessionError, SessionEvent, SessionPhase, SessionState, SetupError,
    WebSocketError,
};
use crate::transport::codec::Codec;
use crate::transport::ws::Transport;
use crate::transport::TransportConfig;

use super::message_handler::{handle_server_msg, MessageAction};
use super::reconnect::{reconnect_delay, DisconnectReason};

/// The main connection loop — manages connect, setup, send/recv, and reconnection.
///
/// Generic over any `Transport` + `Codec`. On each attempt it:
/// 1. Connects the transport
/// 2. Sends the setup message via the codec
/// 3. Waits for `setupComplete`
/// 4. Runs the full-duplex session loop
/// 5. On disconnect, decides whether to reconnect or give up
pub(super) async fn generic_connection_loop<T: Transport, C: Codec>(
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
        let model_uri = config.model_uri();
        tracing::info!(url = %url, model = %model_uri, "WebSocket connecting");
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
                        let _ =
                            event_tx.send(SessionEvent::Error(format!("Setup encode error: {e}")));
                        break;
                    }
                };
                if let Err(e) = transport.send(setup_bytes).await {
                    let _ = event_tx.send(SessionEvent::Error(format!("Setup send error: {e}")));
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
                        tracing::warn!(error = %e, "WebSocket setup failed");
                        let _ = event_tx.send(SessionEvent::Error(format!("Setup failed: {e}")));
                    }
                    Err(_) => {
                        tracing::warn!("WebSocket setup timeout ({}s)", transport_config.setup_timeout_secs);
                        let _ = event_tx.send(SessionEvent::Error("Setup timeout".into()));
                    }
                }
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "WebSocket connection failed");
                let _ = event_tx.send(SessionEvent::Error(format!("Connection failed: {e}")));
            }
            Err(_) => {
                tracing::warn!("WebSocket connect timeout");
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
                            let _ = event_tx.send(SessionEvent::SessionResumeUpdate(ResumeInfo {
                                handle: handle.clone(),
                                resumable: true,
                                last_consumed_index: None,
                            }));
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
                tracing::warn!("Server closed connection during setup (no setupComplete received)");
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
                        if let Ok(msg) = codec.decode_message(&bytes) {
                            match handle_server_msg(msg, state, event_tx) {
                                MessageAction::Continue => {}
                                MessageAction::GoAway(time_left) => {
                                    return DisconnectReason::GoAway(time_left);
                                }
                            }
                        }
                        // Skip unparseable messages
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
