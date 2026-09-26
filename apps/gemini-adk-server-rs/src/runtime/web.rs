//! The browser and app entry point: `GET /ws/{bundle}`.
//!
//! The message shapes are the web app's ([`ClientMessage`] in,
//! [`ServerMessage`] out): the client sends `start`, then `text` turns and
//! audio (binary frames of 16 kHz mono PCM16, or base64 in `audio`
//! messages); the server sends `connected`, a `stateUpdate` naming the
//! bundle version, then text, transcripts, turn boundaries and binary
//! frames of 24 kHz PCM16 model audio. The spec comes from the store, so
//! the overrides a `start` message may carry (instruction, model, voice,
//! config) are ignored, and so are posture edits.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State as AxumState};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use tokio::sync::broadcast::error::RecvError;
use tracing::{info, warn};

use gemini_adk_rs::live::{LiveEvent, LiveHandle};

use super::{LoadedBundle, Runtime, SessionPermit, auth, unauthorized};
use crate::ws::{ClientMessage, ServerMessage};

/// How long a client has to send `start` after the socket opens.
const START_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) async fn session(
    AxumState(runtime): AxumState<Arc<Runtime>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if !runtime.client_allowed(&headers) {
        return unauthorized();
    }
    let Some(bundle) = runtime.bundle(&name) else {
        return (
            StatusCode::NOT_FOUND,
            format!("no bundle '{name}' is served here"),
        )
            .into_response();
    };
    let permit = match runtime.admit() {
        Ok(permit) => permit,
        Err(refusal) => return refusal.into_response(),
    };
    let upgrade = match upgrade {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };
    let limit = runtime.config.max_message_bytes;
    upgrade
        .protocols([auth::SUBPROTOCOL])
        .max_message_size(limit)
        .max_frame_size(limit)
        .on_upgrade(move |socket| run(runtime, bundle, socket, permit))
}

type Sink = SplitSink<WebSocket, Message>;

async fn send(sink: &mut Sink, message: ServerMessage) -> Result<(), axum::Error> {
    match message {
        ServerMessage::Audio { data } => sink.send(Message::Binary(data.into())).await,
        other => {
            let json = serde_json::to_string(&other).unwrap_or_default();
            sink.send(Message::Text(json.into())).await
        }
    }
}

fn error(message: impl Into<String>) -> ServerMessage {
    ServerMessage::Error {
        message: message.into(),
    }
}

async fn run(
    runtime: Arc<Runtime>,
    bundle: Arc<LoadedBundle>,
    socket: WebSocket,
    _permit: SessionPermit,
) {
    let (mut sink, mut stream) = socket.split();

    // 1. Wait for `start`.
    let started = tokio::time::timeout(START_TIMEOUT, async {
        while let Some(Ok(message)) = stream.next().await {
            match message {
                Message::Text(text) => {
                    return matches!(
                        serde_json::from_str::<ClientMessage>(&text),
                        Ok(ClientMessage::Start { .. })
                    );
                }
                Message::Close(_) => return false,
                _ => {}
            }
        }
        false
    })
    .await;
    if started != Ok(true) {
        let _ = send(&mut sink, error("expected a start message first")).await;
        let _ = sink.close().await;
        return;
    }

    // 2. Connect a session on the version this bundle has now.
    let handle = match runtime.connect(&bundle).await {
        Ok(handle) => handle,
        Err(e) => {
            warn!(bundle = %bundle.reference, "session failed to start: {e}");
            let _ = send(&mut sink, error("the session could not be started")).await;
            let _ = sink.close().await;
            return;
        }
    };
    let mut events = handle.events();
    info!(bundle = %bundle.reference, version = %bundle.version.version, "web session started");
    let hello = [
        ServerMessage::Connected,
        ServerMessage::StateUpdate {
            key: "runtime:bundle".into(),
            value: serde_json::json!({
                "name": bundle.name,
                "reference": bundle.reference,
                "version": bundle.version.version,
            }),
        },
    ];
    for message in hello {
        if send(&mut sink, message).await.is_err() {
            let _ = handle.disconnect().await;
            return;
        }
    }

    // 3. Relay until either side ends, the time limit, or shutdown.
    let deadline = tokio::time::sleep(runtime.config.max_session);
    tokio::pin!(deadline);
    let mut closing = runtime.closing();
    loop {
        tokio::select! {
            inbound = stream.next() => match inbound {
                Some(Ok(Message::Text(text))) => {
                    let reply = match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(ClientMessage::Stop) => break,
                        Ok(message) => apply(&handle, message).await,
                        Err(e) => Some(error(format!("unreadable message: {e}"))),
                    };
                    if let Some(reply) = reply
                        && send(&mut sink, reply).await.is_err()
                    {
                        break;
                    }
                }
                Some(Ok(Message::Binary(pcm))) => {
                    if let Err(e) = handle.send_audio(pcm.to_vec()).await
                        && send(&mut sink, error(e.to_string())).await.is_err()
                    {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            event = events.recv() => match event {
                Ok(LiveEvent::Disconnected { .. }) | Err(RecvError::Closed) => break,
                Ok(event) => {
                    if let Some(message) = to_client(event)
                        && send(&mut sink, message).await.is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => {}
            },
            () = &mut deadline => {
                let _ = send(&mut sink, error("session time limit reached")).await;
                break;
            }
            () = super::closed(&mut closing) => {
                let _ = send(&mut sink, error("server is shutting down")).await;
                break;
            }
        }
    }
    let _ = handle.disconnect().await;
    let _ = sink.close().await;
    info!(bundle = %bundle.reference, "web session ended");
}

/// Apply one client message; returns a message for the client, if any.
async fn apply(handle: &LiveHandle, message: ClientMessage) -> Option<ServerMessage> {
    let result = match message {
        ClientMessage::Text { text } => handle.send_text(&text).await,
        ClientMessage::Audio { data } => {
            match base64::engine::general_purpose::STANDARD.decode(data.as_bytes()) {
                Ok(pcm) => handle.send_audio(pcm).await,
                Err(e) => return Some(error(format!("audio is not base64: {e}"))),
            }
        }
        ClientMessage::PlaybackDrained => handle.playback_drained().await,
        ClientMessage::UserSpeechStarted => handle.user_speech_started().await,
        ClientMessage::UserSpeechEnded => handle.user_speech_ended().await,
        ClientMessage::UpdateFlowPostures { .. } => {
            return Some(error("posture edits are not accepted by adk-runtime"));
        }
        ClientMessage::Start { .. } | ClientMessage::Stop => Ok(()),
    };
    result.err().map(|e| error(e.to_string()))
}

/// What a client sees of a session: its conversation, not its internals
/// (state, tool calls and telemetry stay on the server).
fn to_client(event: LiveEvent) -> Option<ServerMessage> {
    Some(match event {
        LiveEvent::Audio(data) => ServerMessage::Audio {
            data: data.to_vec(),
        },
        LiveEvent::TextDelta(text) => ServerMessage::TextDelta { text },
        LiveEvent::TextComplete(text) => ServerMessage::TextComplete { text },
        LiveEvent::InputTranscript { text, .. } => ServerMessage::InputTranscription { text },
        LiveEvent::OutputTranscript { text, .. } => ServerMessage::OutputTranscription { text },
        LiveEvent::VadStart => ServerMessage::VoiceActivityStart,
        LiveEvent::VadEnd => ServerMessage::VoiceActivityEnd,
        LiveEvent::TurnComplete => ServerMessage::TurnComplete,
        LiveEvent::Interrupted => ServerMessage::Interrupted,
        LiveEvent::Error(message) => ServerMessage::Error { message },
        _ => return None,
    })
}
